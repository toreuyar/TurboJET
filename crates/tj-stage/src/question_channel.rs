//! Stage-side implementation of the runner's blocking [`QuestionChannel`]
//! (plan T-5). Backed by the run's [`ChatSession`] (HybridSession = Zulip ∥
//! webui `WebInputBridge`), an ask:
//!
//! 1. renders the [`QuestionRequest`] to plain text (OQ-3 lettered template)
//!    and `session.send()`s it — the webui input UI and Zulip both receive it;
//! 2. durably flips the stage row `Running → WaitingForInput` (`db.save_stage`).
//!    The flip is load-bearing: `send_web_input` hard-gates on the per-stage
//!    `waiting_for_input` status and `run_has_waiting_stage`
//!    (`crates/tj-manager/src/actor.rs`), so without it `POST /input` 409s;
//! 3. blocks on `session.receive()` for the user's answer;
//! 4. resurrect-guards the restore: re-reads the stage row and aborts the ask
//!    with an error instead of writing when the row went terminal mid-ask
//!    (e.g. `finalize_stale_waiting_stages` flipped it via the watchdog) —
//!    a terminal row is never overwritten;
//! 5. parses the reply into the structured [`QuestionAnswer`] (JSON object
//!    answers from the webui; letter/number/label selections plus free text
//!    from Zulip per OQ-2/OQ-3) and logs the round-trip
//!    (`agent_message`/`question`/`user_input`/`question_answer`) so StageChat
//!    keeps working and T-10 can render the structured payloads.
//!
//! # T-12 — reply parsing contract (deterministic precedence)
//!
//! A reply is parsed with a fixed, documented precedence — the first
//! matching arm wins, so parsing is deterministic:
//!
//! 1. **JSON object** with recognizable answer keys (`selected`/`text`) —
//!    the webui path (OQ-2; bare JSON scalars fall through per OQ-2b);
//! 2. **exact option-id match** (case-insensitive) — an id equal to a
//!    letter/digit beats the positional maps;
//! 3. **positional letter map** — a single `a`–`z` token selects the
//!    nth option (this beats a label match: the dispute "label `a` vs
//!    letter `a`" resolves positionally);
//! 4. **positional digit map** — a single 1-based digit (`1` = first
//!    option; `0` never matches);
//! 5. **option-label match** (case-insensitive);
//! 6. **free text** — anything unmatched is kept as `text`.
//!
//! # T-12 — bounded re-ask contract
//!
//! A reply that yields no selection and no free-form text (the question
//! disallowed free form, or the reply was empty) is **unusable**. The
//! channel does not feed garbage to the model: it re-asks the user once per
//! unusable reply with a clarification ("couldn't parse your reply, answer
//! with letters like `a, c`"), at most `max_reasks` times (the ADC's
//! `max_output_retries`, mirroring D-2). Past the bound the ask returns an
//! error naming the last reply — the stage fails visibly through the
//! runner's existing bounds instead of waiting forever (NFR-1). A late user
//! reply after the ask gave up is queued and consumed by the *next* ask —
//! pre-existing HybridSession behavior, noted in the plan (OQ-3), not fixed
//! here.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use tj_chat::ChatSession;
use tj_db::{Database, QuestionRecord, QuestionStatus, RunId, StageLog, StageStatus};
use tj_runner::{QuestionAnswer, QuestionChannel, QuestionRequest};

/// Blocking question channel for one interactive stage, over the run's chat
/// session and the stage-log database.
pub struct ChatQuestionChannel {
    db: Arc<Database>,
    session: Arc<dyn ChatSession>,
    run_id: String,
    flow_name: String,
    /// How many clarification re-asks one ask may issue before erroring
    /// (the stage ADC's `max_output_retries`; plan T-12, D-2).
    max_reasks: u32,
}

impl ChatQuestionChannel {
    pub fn new(
        db: Arc<Database>,
        session: Arc<dyn ChatSession>,
        run_id: String,
        flow_name: String,
        max_reasks: u32,
    ) -> Self {
        Self {
            db,
            session,
            run_id,
            flow_name,
            max_reasks,
        }
    }

    fn append_log(&self, stage_id: &str, stage_name: &str, kind: &str, content: &str) {
        let l = StageLog {
            id: 0,
            run_id: RunId(self.run_id.clone()),
            stage_id: stage_id.to_string(),
            stage_name: Some(stage_name.to_string()),
            flow_name: Some(self.flow_name.clone()),
            kind: kind.to_string(),
            content: content.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(e) = self.db.append_stage_log(&l) {
            tracing::warn!(stage_id = %stage_id, kind = %kind, error = %e, "Failed to append question log");
        }
    }

    /// Durable status flip with the resurrect-guard (architect review finding
    /// 3): re-read the stage row and refuse to write when it is terminal. A
    /// watchdog finalize (or any downstream finalization) must never be
    /// resurrected into Running/WaitingForInput by an in-flight ask.
    fn guarded_set_status(
        &self,
        stage_id: &str,
        stage_name: &str,
        target: StageStatus,
    ) -> Result<()> {
        let mut row = self
            .db
            .load_stage(stage_id)?
            .with_context(|| format!("stage row '{}' vanished from the database", stage_id))?;
        if matches!(
            row.status(),
            StageStatus::Completed | StageStatus::Failed | StageStatus::TimedOut
        ) {
            return Err(anyhow!(
                "stage '{}' went terminal ({}) while a question was in flight; refusing to \
                 overwrite the terminal row — the run was finalized elsewhere (watchdog or resume)",
                stage_name,
                row.status()
            ));
        }
        if *row.status() != target {
            let previous = row.status().clone();
            row.set_status(target.clone());
            self.db.save_stage(&row)?;
            tracing::info!(
                stage_id = %stage_id,
                stage = %stage_name,
                from = %previous,
                to = %target,
                "Stage status flipped for blocking question"
            );
        }
        Ok(())
    }
}

impl QuestionChannel for ChatQuestionChannel {
    // Hand-desugared #[async_trait] signature (the workspace rule against new
    // dependencies applies; async-trait is not otherwise needed here).
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn ask<'life0, 'life1, 'life2, 'life3, 'async_trait>(
        &'life0 self,
        stage_id: &'life1 str,
        stage_name: &'life2 str,
        request: &'life3 QuestionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<QuestionAnswer>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let stage = self
                .db
                .load_stage(stage_id)?
                .with_context(|| format!("stage row '{}' vanished from the database", stage_id))?;
            if matches!(
                stage.status(),
                StageStatus::Completed | StageStatus::Failed | StageStatus::TimedOut
            ) {
                return Err(anyhow!(
                    "stage '{}' is already terminal ({}); refusing to open a question",
                    stage_name,
                    stage.status()
                ));
            }

            // TJ-016: every new question round is charged before delivery.
            // Unparseable-reply clarification re-asks reuse this same round;
            // a model that keeps asking creates new rounds and is bounded.
            if self.db.has_run_budget(&self.run_id).unwrap_or(false) {
                // Scope is the flow/stage lineage, not the row id: a retry
                // creates a new stage execution row and must not reset D5.
                let scope = format!("{}:{}", self.flow_name, stage_name);
                self.db.charge_budget(
                    &self.run_id,
                    tj_db::BUDGET_QUESTION_ROUNDS,
                    Some(&scope),
                    &format!("question:{}:{}", scope, uuid::Uuid::new_v4()),
                )?;
            }

            // Durable identity is created before delivery: run + flow + stage
            // execution + attempt. A later answer can only address this row.
            let question_id_value = format!("question_{}", uuid::Uuid::new_v4());
            let question = QuestionRecord::new(
                question_id_value.clone(),
                RunId(self.run_id.clone()),
                stage.flow_id().as_str().to_string(),
                stage_id.to_string(),
                stage_name.to_string(),
                self.flow_name.clone(),
                stage.iteration(),
                serde_json::to_string(request).context("serialize QuestionRequest")?,
            );
            self.db.create_question(&question)?;

            let rendered = render_question(stage_name, request);
            self.append_log(stage_id, stage_name, "agent_message", &rendered);
            let mut payload = serde_json::to_value(request).context("serialize QuestionRequest")?;
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "question_id".into(),
                    serde_json::Value::String(question_id_value.clone()),
                );
                obj.insert(
                    "stage_id".into(),
                    serde_json::Value::String(stage_id.to_string()),
                );
                obj.insert("attempt".into(), serde_json::Value::from(stage.iteration()));
                obj.insert("status".into(), serde_json::Value::String("open".into()));
            }
            self.append_log(stage_id, stage_name, "question", &payload.to_string());
            tracing::info!(
                stage_id = %stage_id,
                stage = %stage_name,
                question_id = %question_id_value,
                attempt = stage.iteration(),
                options = request.options.len(),
                multi_select = request.multi_select,
                free_form = request.free_form,
                "Question asked over chat channel"
            );

            if let Err(error) = self.session.send(&rendered).await {
                let _ = self.db.close_question(
                    &question_id_value,
                    QuestionStatus::Cancelled,
                    "question delivery failed",
                );
                return Err(anyhow!(
                    "failed to deliver question for stage '{}': {}",
                    stage_name,
                    error
                ));
            }

            // Load-bearing durable flip: opens the POST /input gates
            // (send_web_input -> run_has_waiting_stage + per-stage status).
            if let Err(error) =
                self.guarded_set_status(stage_id, stage_name, StageStatus::WaitingForInput)
            {
                let _ = self.db.close_question(
                    &question_id_value,
                    QuestionStatus::Cancelled,
                    "stage could not enter waiting state",
                );
                return Err(error);
            }

            // One receive future is kept across polls; the DB is the source of
            // truth so an API acceptance wakes the ask even over the webui
            // transport (and survives the runner not holding a bridge message).
            let mut receive = self.session.receive();
            let mut reasks_used: u32 = 0;
            let accepted = loop {
                match self.db.load_question(&question_id_value)? {
                    Some(current) if current.status == QuestionStatus::Answered => break current,
                    Some(current)
                        if matches!(
                            current.status,
                            QuestionStatus::Closed | QuestionStatus::Cancelled
                        ) =>
                    {
                        let _ = self.guarded_set_status(stage_id, stage_name, StageStatus::Running);
                        return Err(anyhow!(
                            "question '{}' for stage '{}' is {}; the reply is late",
                            question_id_value,
                            stage_name,
                            current.status.as_str()
                        ));
                    }
                    Some(_) => {}
                    None => {
                        return Err(anyhow!(
                            "durable question '{}' disappeared",
                            question_id_value
                        ));
                    }
                }

                tokio::select! {
                    reply = &mut receive => {
                        let reply = match reply {
                            Ok(reply) => reply,
                            Err(error) => {
                                let _ = self.db.close_question(&question_id_value, QuestionStatus::Cancelled, "chat channel closed while waiting");
                                let _ = self.guarded_set_status(stage_id, stage_name, StageStatus::Running);
                                return Err(anyhow!(
                                    "waiting for the answer to stage '{}' failed: {}",
                                    stage_name,
                                    error
                                ));
                            }
                        };

                        // A watchdog/superseded row must never be resurrected.
                        let current_stage = self.db.load_stage(stage_id)?
                            .with_context(|| format!("stage row '{}' vanished during a question", stage_id))?;
                        if matches!(
                            current_stage.status(),
                            StageStatus::Completed | StageStatus::Failed | StageStatus::TimedOut
                        ) {
                            let _ = self.db.close_question(&question_id_value, QuestionStatus::Cancelled, "stage went terminal while a question was in flight");
                            return Err(anyhow!(
                                "stage '{}' went terminal ({}) while a question was in flight; refusing to overwrite the terminal row",
                                stage_name,
                                current_stage.status()
                            ));
                        }

                        self.append_log(stage_id, stage_name, "user_input", &reply);
                        let answer = parse_answer(&reply, request);
                        if !reply_is_usable(&answer, request) {
                            if reasks_used >= self.max_reasks {
                                let _ = self.db.close_question(&question_id_value, QuestionStatus::Cancelled, "unparseable replies exhausted the re-ask bound");
                                let _ = self.guarded_set_status(stage_id, stage_name, StageStatus::Running);
                                return Err(anyhow!(
                                    "stage '{}': the user's reply could not be parsed as an answer after {} \
                                     re-ask(s); last reply: {:?} — giving up visibly rather than feeding an \
                                     unparseable answer to the model or waiting forever",
                                    stage_name,
                                    reasks_used,
                                    reply
                                ));
                            }
                            reasks_used += 1;
                            let clarification = reask_message(request);
                            self.append_log(stage_id, stage_name, "agent_message", &clarification);
                            if let Err(error) = self.session.send(&clarification).await {
                                let _ = self.db.close_question(&question_id_value, QuestionStatus::Cancelled, "re-ask delivery failed");
                                let _ = self.guarded_set_status(stage_id, stage_name, StageStatus::Running);
                                return Err(anyhow!(
                                    "failed to deliver the re-ask for stage '{}': {}",
                                    stage_name,
                                    error
                                ));
                            }
                            // The previous receive future has completed; a
                            // fresh one is needed for the next reply.
                            receive = self.session.receive();
                            continue;
                        }

                        let answer_json = serde_json::to_string(&answer).context("serialize QuestionAnswer")?;
                        let idempotency_key = chat_idempotency_key(&question_id_value, &answer_json);
                        match self.db.answer_question(
                            &question_id_value,
                            &self.run_id,
                            stage_id,
                            Some(stage.iteration()),
                            &answer_json,
                            "chat_session",
                            &idempotency_key,
                        ) {
                            Ok(acceptance) => break acceptance.question,
                            Err(error) => {
                                // Another transport may have won the race. The
                                // model receives the accepted answer, not the
                                // loser's text, and the conflict is visible in
                                // the activity log.
                                if let Some(current) = self.db.load_question(&question_id_value)?
                                    && current.status == QuestionStatus::Answered
                                {
                                    self.append_log(
                                        stage_id,
                                        stage_name,
                                        "question_conflict",
                                        &serde_json::json!({
                                            "question_id": question_id_value,
                                            "reason": error.to_string(),
                                            "winner": current.answer_source,
                                            "losing_source": "chat_session",
                                        }).to_string(),
                                    );
                                    break current;
                                }
                                let _ = self.db.close_question(&question_id_value, QuestionStatus::Cancelled, "answer acceptance failed");
                                let _ = self.guarded_set_status(stage_id, stage_name, StageStatus::Running);
                                return Err(error);
                            }
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                        // Re-check the durable row on the next loop turn.
                        continue;
                    }
                }
            };

            self.guarded_set_status(stage_id, stage_name, StageStatus::Running)?;
            let answer_json = accepted.answer.clone().unwrap_or_default();
            let answer: QuestionAnswer = serde_json::from_str(&answer_json).with_context(|| {
                format!(
                    "accepted answer for question '{}' is not valid JSON",
                    question_id_value
                )
            })?;
            self.append_log(
                stage_id,
                stage_name,
                "question_answer",
                &serde_json::json!({
                    "question_id": question_id_value,
                    "source": accepted.answer_source,
                    "selected": answer.selected,
                    "text": answer.text,
                })
                .to_string(),
            );
            tracing::info!(
                stage_id = %stage_id,
                stage = %stage_name,
                question_id = %question_id_value,
                source = ?accepted.answer_source,
                "Question answered"
            );
            Ok(answer)
        })
    }
}

fn chat_idempotency_key(question_id: &str, answer_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(question_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(answer_json.as_bytes());
    format!("chat:{}", hex::encode(hasher.finalize()))
}

/// Render a [`QuestionRequest`] as the OQ-3 plain-text template (Zulip
/// parity): lettered positional list plus the reply convention.
///
/// Zulip-markdown survival (plan T-12 scope 4): the option lines are indented
/// **two** spaces — below Zulip's four-space code-block threshold — and use
/// `a)` markers, which Zulip does not treat as list syntax (only `-`, `*` and
/// `N.` start lists; a bare `N.` would be auto-renumbered). The `**bold**`
/// header and the `` `a, c` `` inline code are native Zulip markup, so the
/// template survives transport unchanged.
pub fn render_question(stage_name: &str, request: &QuestionRequest) -> String {
    let mut out = format!("**{} asks:**\n", stage_name);
    out.push_str(request.question.trim());
    out.push(' ');
    out.push_str(&hint_for(request));
    out.push('\n');
    for (index, option) in request.options.iter().enumerate() {
        out.push_str(&format!("  {}) {}\n", option_letter(index), option.label));
    }
    if request.options.is_empty() {
        out.push_str("Reply with your answer.");
    } else {
        out.push_str("Reply with letters, e.g. `a, c`, plus any notes — or just type your answer.");
    }
    out
}

fn hint_for(request: &QuestionRequest) -> &'static str {
    if request.options.is_empty() {
        "(free text)"
    } else if request.multi_select && request.free_form {
        "(select all that apply; free text welcome)"
    } else if request.multi_select {
        "(select all that apply)"
    } else if request.free_form {
        "(reply with one letter; free text welcome)"
    } else {
        "(reply with one letter)"
    }
}

fn option_letter(index: usize) -> char {
    char::from_u32(b'a' as u32 + index as u32).unwrap_or('?')
}

/// Parse a raw user reply into the structured [`QuestionAnswer`] (OQ-2):
/// a JSON **object** answer wins (webui path; a bare JSON scalar falls
/// through to free text); otherwise each token resolves by the documented
/// precedence — exact option id > positional letter > positional digit >
/// label — with the remainder kept as free text (Zulip path); free-text-only
/// questions take the whole reply as `text`. Tokens may be separated by
/// commas, semicolons, whitespace or newlines, joined by `and`/`&`, in any
/// case, with wrapping punctuation (`` ` `` `"` `.` `(` `)` `*`) tolerated.
pub fn parse_answer(reply: &str, request: &QuestionRequest) -> QuestionAnswer {
    let trimmed = reply.trim();

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        // OQ-2: only a JSON object with recognizable answer keys counts.
        let is_answer_object = value
            .as_object()
            .is_some_and(|obj| obj.contains_key("selected") || obj.contains_key("text"));
        if is_answer_object {
            if let Ok(mut answer) = serde_json::from_value::<QuestionAnswer>(value) {
                answer.selected = normalize_selected(std::mem::take(&mut answer.selected), request);
                return answer;
            }
            // Unreadable answer object — treat the raw reply as free text.
            return QuestionAnswer {
                selected: Vec::new(),
                text: Some(trimmed.to_string()),
            };
        }
        // Other JSON (objects without answer keys, bare scalars like "teal",
        // arrays) falls through to letter/free-text parsing (OQ-2b).
    }

    if !request.options.is_empty() {
        let (selector_part, notes) = split_selection_and_notes(trimmed);
        let mut selected: Vec<String> = Vec::new();
        let mut free: Vec<&str> = Vec::new();
        for token in selector_part.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
            let token = token.trim();
            if token.is_empty() || token.eq_ignore_ascii_case("and") || token == "&" {
                continue;
            }
            match resolve_option_token(token, request) {
                Some(id) => {
                    if !selected.contains(&id) {
                        selected.push(id);
                    }
                }
                None => free.push(token),
            }
        }
        let mut text_parts: Vec<String> = Vec::new();
        if !notes.is_empty() {
            text_parts.push(notes.to_string());
        }
        if !free.is_empty() {
            text_parts.push(free.join(" "));
        }
        let text = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(" "))
        };
        return QuestionAnswer { selected, text };
    }

    QuestionAnswer {
        selected: Vec::new(),
        text: Some(trimmed.to_string()),
    }
}

/// OQ-2a fallback: `selected` carries option ids; label text (and single
/// letters/numbers) are accepted case-insensitively and normalized to ids.
fn normalize_selected(selected: Vec<String>, request: &QuestionRequest) -> Vec<String> {
    if request.options.is_empty() {
        return selected;
    }
    selected
        .into_iter()
        .map(|entry| resolve_option_token(&entry, request).unwrap_or(entry))
        .collect()
}

/// Content after the first `—`/`--`/`:` separator is free-text notes (OQ-3).
fn split_selection_and_notes(reply: &str) -> (&str, &str) {
    for sep in ["—", "--", ":"] {
        if let Some((head, tail)) = reply.split_once(sep) {
            return (head, tail.trim());
        }
    }
    (reply, "")
}

/// Map one reply token to an option id, by the fixed precedence (first match
/// wins — deterministic):
///
/// 1. exact option-id match (case-insensitive) — beats the positional maps,
///    so an id like `b` is never reinterpreted as the letter for option 2;
/// 2. positional letter map — a single `a`–`z` selects the nth option (a
///    label equal to a letter loses to the letter: "label `b` vs letter `b`"
///    resolves positionally);
/// 3. positional digit map — a single 1-based digit (`1` = first option);
/// 4. option-label match (case-insensitive);
/// 5. no match → `None` → the token becomes free text.
fn resolve_option_token(token: &str, request: &QuestionRequest) -> Option<String> {
    let cleaned: String = token
        .trim()
        .trim_matches(|c| matches!(c, '`' | '*' | '"' | '.' | '(' | ')' | '\''))
        .to_lowercase();
    if cleaned.is_empty() {
        return None;
    }
    // 1. exact option-id match beats the positional letter/digit maps.
    if let Some(option) = request
        .options
        .iter()
        .find(|o| o.id.trim().to_lowercase() == cleaned)
    {
        return Some(option.id.clone());
    }
    // 2./3. single positional letter, then single 1-based digit.
    let mut chars = cleaned.chars();
    if let (Some(single), None) = (chars.next(), chars.next()) {
        if single.is_ascii_lowercase() {
            let index = single as usize - 'a' as usize;
            return request.options.get(index).map(|o| o.id.clone());
        }
        if let Some(digit) = single.to_digit(10) {
            return if digit >= 1 {
                request
                    .options
                    .get((digit - 1) as usize)
                    .map(|o| o.id.clone())
            } else {
                None
            };
        }
    }
    // 4. label match (case-insensitive).
    request
        .options
        .iter()
        .find(|o| o.label.trim().to_lowercase() == cleaned)
        .map(|o| o.id.clone())
}

/// T-12: does a parsed reply count as an answer? It does when at least one
/// option was selected, or — only when the question allowed free-form input —
/// it carried non-empty text. An empty reply is never usable (plan T-12:
/// empty reply → re-ask), and prose on a letters-only question is unusable
/// too, so the channel re-asks instead of feeding it to the model.
fn reply_is_usable(answer: &QuestionAnswer, request: &QuestionRequest) -> bool {
    if !answer.selected.is_empty() {
        return true;
    }
    let has_text = answer.text.as_deref().is_some_and(|t| !t.trim().is_empty());
    has_text && request.free_form
}

/// The clarification sent on an unusable reply (plan T-12 re-ask contract).
fn reask_message(request: &QuestionRequest) -> String {
    if request.options.is_empty() {
        "I couldn't parse your reply as an answer — please type your answer as plain text (an \
         empty message doesn't count)."
            .to_string()
    } else {
        "I couldn't parse your reply as an answer — reply with the option letters, e.g. `a, c` \
         (or just `a`), plus any notes after a dash."
            .to_string()
    }
}
