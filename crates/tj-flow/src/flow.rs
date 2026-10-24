use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tj_chat::ChatSession;
use tj_config::bundle::ExecutionBundle;
use tj_config::loader::ConfigLoader;
use tj_config::smg::{FlowDef, FlowType, RouteTarget, StageDef, StateMachineGraph};
use tj_db::{Database, FlowRecord, FlowStatus, StageStatus};
use tj_stage::{Stage, StageConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FlowOutcome {
    Completed {
        flow_id: String,
        run_id: String,
        flow_name: String,
        artifacts: HashMap<String, serde_json::Value>,
    },
    WaitingForInput {
        flow_id: String,
        run_id: String,
        flow_name: String,
        stage_name: String,
        stage_id: String,
        message: String,
    },
    Failed {
        flow_id: String,
        run_id: String,
        flow_name: String,
        error: String,
    },
    TimedOut {
        flow_id: String,
        run_id: String,
        flow_name: String,
        stage_name: String,
    },
}

pub struct FlowConfig {
    /// Reserved by the manager before the execution task is spawned.
    pub flow_id: String,
    pub run_id: String,
    pub flow_name: String,
    pub flow_number: Option<u64>,
    pub smg: StateMachineGraph,
    pub description: String,
    pub existing_artifacts: HashMap<String, serde_json::Value>,
    pub workspace_path: Option<PathBuf>,
    pub branch_name: Option<String>,
    pub slug: String,
    /// Attempt offset for artifact versioning: 0 for first run of a flow,
    /// bumped when the manager re-enters the same flow for rework so rework
    /// artifacts land in a fresh attempt lineage.
    pub attempt_offset: u32,
    /// Immutable bundle for runs admitted after TJ-012. `None` is a legacy run
    /// whose stage files are still resolved from disk (explicitly marked).
    pub bundle: Option<Arc<ExecutionBundle>>,
}

pub struct Flow {
    db: Arc<Database>,
    config_loader: Arc<ConfigLoader>,
    chat_session: Option<Arc<dyn ChatSession>>,

    // Execution state
    config: FlowConfig,
    flow_id: String,
    artifacts: HashMap<String, serde_json::Value>,
    current_stage: String,
    iteration_counts: HashMap<String, u32>,
    steps: u32,
    previous_stage: Option<String>,
    attempt_offset: u32,
}

/// Extract a single-line, length-capped summary from a stage's output artifact.
/// JSON objects: prefers "content", then "message", then "summary"; otherwise the
/// first (short) string value found anywhere in the object; non-JSON text is
/// summarized as-is.
fn summarize_stage_output(output: Option<&str>) -> Option<String> {
    let raw = output?;
    let parsed: Option<serde_json::Value> = serde_json::from_str(raw).ok();
    let text = match parsed {
        Some(serde_json::Value::Object(ref map)) => ["content", "message", "summary"]
            .iter()
            .find_map(|k| map.get(*k).and_then(|s| s.as_str()))
            .map(|s| s.to_string())
            .or_else(|| first_short_string(&serde_json::Value::Object(map.clone())))?,
        Some(serde_json::Value::Array(_)) | Some(serde_json::Value::String(_)) => raw.to_string(),
        Some(_) => return None,
        None => raw.to_string(),
    };
    summarize_line(&text)
}

/// First string value in the value tree that looks like prose (>= 8 chars, not a hash/id).
fn first_short_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => {
            let t = s.trim();
            (t.len() >= 8 && !t.chars().all(|c| c.is_ascii_hexdigit())).then(|| t.to_string())
        }
        serde_json::Value::Array(items) => items.iter().find_map(first_short_string),
        serde_json::Value::Object(map) => map.values().find_map(first_short_string),
        _ => None,
    }
}

fn summarize_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    const MAX_CHARS: usize = 120;
    if line.chars().count() <= MAX_CHARS {
        Some(line.to_string())
    } else {
        let cut: String = line.chars().take(MAX_CHARS).collect();
        Some(format!("{}…", cut.trim_end()))
    }
}

/// Stage-log row kind carrying the routing decision (FR-23e).
const STAGE_VERDICT_LOG_KIND: &str = "stage_verdict";

/// Pull the `verdict` string out of a completed stage's artifact JSON (FR-24).
fn extract_verdict(output: Option<&str>) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(output?).ok()?;
    parsed.get("verdict")?.as_str().map(str::to_string)
}

/// FR-24: `stage_completed` run events keep their human summary and now also
/// carry the routed verdict in the event payload, so the gate review screen can
/// show why the pipeline moved on.
fn completed_event_fields(output: Option<&str>) -> (Option<String>, Option<String>) {
    let summary = summarize_stage_output(output);
    let payload = extract_verdict(output).map(|v| serde_json::json!({ "verdict": v }).to_string());
    (summary, payload)
}

/// FR-13: after a stage failure (an `output contract violation` included) the
/// flow re-executes the stage from scratch — fresh conversation, new attempt —
/// until `max_iterations` failed attempts are recorded for that stage.
fn stage_retry_exhausted(failed_attempts: u32, max_iterations: u32) -> bool {
    failed_attempts >= max_iterations
}

/// A run-wide budget denial is terminal for the flow: retrying the same stage
/// cannot create budget and would only spend more model/tool work trying.
fn budget_denial_is_terminal(error: &str) -> bool {
    error.contains("budget exhausted:")
}

/// Loud-failure reason for a completion whose artifact carries no routable
/// `verdict` (FR-16). Names the stage and the reason — this string is
/// user-facing (it becomes the flow's Failed error).
fn missing_verdict_error(stage: &str) -> String {
    format!(
        "stage '{}' produced no routable verdict (missing artifact key) — routing is unreachable without a validated artifact",
        stage
    )
}

/// A successful verdict-first routing decision (FR-16).
struct Routing {
    verdict: String,
    /// Raw SMG route target as declared (e.g. `_next-flow`, `implement`, `_exit`).
    target: String,
    next: NextStage,
}

/// Core verdict lookup + route resolution, pure — no I/O (test seam for
/// `Flow::resolve_next`, which wraps this with logging + defensive arms).
///
/// Contract (fail-open removed, plan T-6): the artifact MUST carry a `verdict`
/// string and it must match one of the stage's declared route keys. The runner
/// (plan T-3) completes a stage only with validated `verdict` tool arguments,
/// so these errors are unreachable by construction — defensive, LOUD, and
/// never a silent `success` default (NFR-3).
fn route_on_verdict(
    current_stage: &str,
    stage_def: &StageDef,
    flow_def: &FlowDef,
    artifacts: &HashMap<String, serde_json::Value>,
) -> Result<Routing, String> {
    let artifact = artifacts
        .get(current_stage)
        .ok_or_else(|| missing_verdict_error(current_stage))?;
    let verdict = artifact
        .get("verdict")
        .and_then(|v| v.as_str())
        .ok_or_else(|| missing_verdict_error(current_stage))?;
    let target = stage_def.routes.get(verdict).ok_or_else(|| {
        format!(
            "unknown verdict '{}' for stage '{}'",
            verdict, current_stage
        )
    })?;
    Ok(Routing {
        verdict: verdict.to_string(),
        target: target.to_string(),
        next: resolve_target(target, flow_def)?,
    })
}

impl Flow {
    pub fn new(
        db: Arc<Database>,
        config_loader: Arc<ConfigLoader>,
        chat_session: Option<Arc<dyn ChatSession>>,
        config: FlowConfig,
    ) -> Self {
        let attempt_offset = config.attempt_offset;
        Self {
            db,
            config_loader,
            chat_session,
            config,
            flow_id: String::new(),
            artifacts: HashMap::new(),
            current_stage: String::new(),
            iteration_counts: HashMap::new(),
            attempt_offset,
            steps: 0,
            previous_stage: None,
        }
    }

    pub async fn execute(mut self) -> FlowOutcome {
        let flow_name = self.config.flow_name.clone();
        let run_id = self.config.run_id.clone();
        tracing::debug!(
            run_id = %run_id,
            flow = %flow_name,
            workspace_path = ?self.config.workspace_path,
            branch_name = ?self.config.branch_name,
            "Flow::execute entered"
        );

        let flow_def = match self.config.smg.flows.get(&flow_name) {
            Some(f) => f,
            None => {
                return FlowOutcome::Failed {
                    flow_id: String::new(),
                    run_id,
                    flow_name: flow_name.clone(),
                    error: format!("Flow '{}' not found in SMG", flow_name),
                };
            }
        };
        if let Err(error) = tj_config::validation::validate_graph(&self.config.smg) {
            return FlowOutcome::Failed {
                flow_id: String::new(),
                run_id,
                flow_name,
                error: format!("Invalid workflow contract: {error}"),
            };
        }
        if flow_def.flow_type != FlowType::Flow {
            return FlowOutcome::Failed {
                flow_id: String::new(),
                run_id,
                flow_name: flow_name.clone(),
                error: format!(
                    "Flow '{}' is a gate and cannot be executed as a stage flow",
                    flow_name
                ),
            };
        }

        let flow_record = self.create_flow_record(
            &run_id,
            &flow_name,
            self.config.flow_number,
            self.config
                .workspace_path
                .as_ref()
                .map(|p| p.to_str().unwrap_or_default().to_string()),
            self.config.branch_name.clone(),
        );
        if let Err(e) = self.db.save_flow(&flow_record) {
            return FlowOutcome::Failed {
                flow_id: flow_record.id().to_string(),
                run_id,
                flow_name,
                error: format!("Failed to save flow record: {}", e),
            };
        }

        self.flow_id = flow_record.id().to_string();
        tracing::info!(flow_id = %self.flow_id, flow = %flow_name, "Flow started");

        self.current_stage = match &flow_def.entry {
            Some(e) => e.clone(),
            None => {
                return FlowOutcome::Failed {
                    flow_id: self.flow_id,
                    run_id,
                    flow_name: flow_name.clone(),
                    error: format!("Flow '{}' has no entry stage", flow_name),
                };
            }
        };
        self.artifacts = std::mem::take(&mut self.config.existing_artifacts);

        let flow_def_clone = flow_def.clone();
        let max_steps = self.config.smg.limits.max_steps;
        let max_iterations = self.config.smg.limits.max_iterations;

        self.execute_loop(&flow_def_clone, max_steps, max_iterations)
            .await
    }

    pub async fn resume(mut self, completed_stage: &str) -> FlowOutcome {
        let flow_name = self.config.flow_name.clone();
        let run_id = self.config.run_id.clone();

        let flow_def = match self.config.smg.flows.get(&flow_name) {
            Some(f) => f,
            None => {
                return FlowOutcome::Failed {
                    flow_id: String::new(),
                    run_id,
                    flow_name: flow_name.clone(),
                    error: format!("Flow '{}' not found in SMG", flow_name),
                };
            }
        };
        if let Err(error) = tj_config::validation::validate_graph(&self.config.smg) {
            return FlowOutcome::Failed {
                flow_id: String::new(),
                run_id,
                flow_name,
                error: format!("Invalid workflow contract: {error}"),
            };
        }
        if flow_def.flow_type != FlowType::Flow {
            return FlowOutcome::Failed {
                flow_id: String::new(),
                run_id,
                flow_name: flow_name.clone(),
                error: format!("Flow '{}' is a gate and cannot be resumed", flow_name),
            };
        }

        let flows = match self.db.load_flows_for_run(&run_id) {
            Ok(f) => f,
            Err(e) => {
                return FlowOutcome::Failed {
                    flow_id: String::new(),
                    run_id,
                    flow_name,
                    error: format!("Failed to load flows for run: {}", e),
                };
            }
        };

        let flow_record = match flows
            .into_iter()
            .find(|f| f.id() == self.config.flow_id && *f.status() == FlowStatus::Running)
        {
            Some(f) => f,
            None => {
                return FlowOutcome::Failed {
                    flow_id: String::new(),
                    run_id,
                    flow_name,
                    error: "No running flow found for resume".into(),
                };
            }
        };
        self.flow_id = flow_record.id().to_string();

        // Reconstruct workspace_path from flow record
        if self.config.workspace_path.is_none() {
            self.config.workspace_path = flow_record.worktree_path().map(PathBuf::from);
        }

        self.artifacts = std::mem::take(&mut self.config.existing_artifacts);
        if let Ok(Some(stage)) = self.db.load_stage_by_name(&self.flow_id, completed_stage)
            && let Some(output) = stage.output_artifacts()
            && let Ok(output_val) = serde_json::from_str::<serde_json::Value>(output)
        {
            self.artifacts
                .insert(completed_stage.to_string(), output_val);
        }

        let next_stage_name = {
            let current = completed_stage.to_string();
            // Legacy-loud-failure semantics (Migration §5.2, NFR-3): with
            // fail-open gone, a legacy artifact lacking `verdict` fails the
            // flow loudly here instead of advancing silently. Reachability
            // rationale (architect review finding 4): this direct resolve_next
            // call is reachable only via `spawn_flow_resume`, whose
            // `ResumeFlow` trigger is never emitted anywhere in the workspace —
            // so legacy verdict-less artifacts cannot reach the loud path in
            // practice; the check is defensive. A heal step (fabricating
            // verdicts into historical artifacts) was explicitly rejected.
            let next = self.resolve_next(&current, flow_def, &self.artifacts);
            match next {
                Ok(NextStage::Stage(name)) => name,
                Ok(NextStage::FlowDone) => {
                    if let Err(e) = self.db.complete_flow(&self.flow_id) {
                        tracing::error!(flow_id = %self.flow_id, error = %e, "Failed to complete flow during resume");
                        return self.fail_flow(
                            &run_id,
                            &flow_name,
                            format!("Cannot persist flow completion: {e}"),
                        );
                    }
                    return FlowOutcome::Completed {
                        flow_id: self.flow_id.clone(),
                        run_id,
                        flow_name,
                        artifacts: std::mem::take(&mut self.artifacts),
                    };
                }
                Ok(NextStage::Escalate) => {
                    return self.fail_flow(
                        &run_id,
                        &flow_name,
                        format!(
                            "Stage '{}' escalated — requires human intervention",
                            current
                        ),
                    );
                }
                Err(reason) => {
                    return self.fail_flow(&run_id, &flow_name, reason);
                }
            }
        };

        tracing::info!(flow_id = %self.flow_id, flow = %flow_name, "Resuming flow from stage '{}' -> '{}'", completed_stage, next_stage_name);

        self.current_stage = next_stage_name;
        self.previous_stage = Some(completed_stage.to_string());

        let flow_def_clone = flow_def.clone();
        let max_steps = self.config.smg.limits.max_steps;
        let max_iterations = self.config.smg.limits.max_iterations;

        self.execute_loop(&flow_def_clone, max_steps, max_iterations)
            .await
    }

    fn store_artifact(&self, run_id: &str, flow_name: &str, stage: &str, content: &str) {
        let attempt =
            self.attempt_offset as i64 + *self.iteration_counts.get(stage).unwrap_or(&0) as i64;
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let hash = hex::encode(hasher.finalize());
        // Content dedupe: skip append when the latest version of this lineage is byte-identical.
        if let Ok(rows) = self.db.list_artifacts(run_id) {
            if let Some(latest) = rows
                .iter()
                .filter(|r| {
                    r.flow_name == flow_name && r.stage_name == stage && r.attempt == attempt
                })
                .max_by_key(|r| r.version)
            {
                if latest.content_hash == hash {
                    return;
                }
            }
        }
        let mut a = tj_db::RunArtifact {
            id: format!("art_{}", uuid::Uuid::new_v4()),
            run_id: tj_db::RunId(run_id.to_string()),
            flow_name: flow_name.to_string(),
            stage_name: stage.to_string(),
            attempt,
            version: 0,
            content: content.to_string(),
            content_hash: hash,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(e) = self.db.append_next_artifact(&mut a) {
            tracing::warn!(run_id = %run_id, stage = %stage, error = %e, "Failed to store artifact");
        }
    }

    fn emit_stage_event(
        &self,
        run_id: &str,
        flow_name: &str,
        stage: &str,
        kind: &str,
        summary: Option<&str>,
        payload: Option<&str>,
    ) {
        let e = tj_db::RunEvent {
            id: format!("evt_{}", uuid::Uuid::new_v4()),
            run_id: tj_db::RunId(run_id.to_string()),
            kind: kind.to_string(),
            actor: None,
            flow_name: Some(flow_name.to_string()),
            stage_name: Some(stage.to_string()),
            summary: summary.map(|s| s.to_string()),
            payload: payload.map(|p| p.to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(err) = self.db.append_event(&e) {
            tracing::warn!(run_id = %run_id, stage = %stage, error = %err, "Failed to append stage event");
        }
    }

    async fn execute_loop(
        &mut self,
        flow_def: &FlowDef,
        max_steps: u32,
        max_iterations: u32,
    ) -> FlowOutcome {
        let flow_name = self.config.flow_name.clone();
        let run_id = self.config.run_id.clone();
        let description = self.config.description.clone();

        loop {
            if self.steps >= max_steps {
                return self.fail_flow(&run_id, &flow_name, "Max steps exceeded");
            }

            let stage_def = match flow_def.stages.get(&self.current_stage) {
                Some(s) => s.clone(),
                None => {
                    return self.fail_flow(
                        &run_id,
                        &flow_name,
                        format!("Stage '{}' not found", self.current_stage),
                    );
                }
            };
            let interactive = stage_def.interactive;

            let input = self.build_stage_input(
                &description,
                &flow_name,
                &self.current_stage,
                &self.artifacts,
            );

            let iteration = *self.iteration_counts.get(&self.current_stage).unwrap_or(&0);
            let stage_cfg = match self.resolve_stage_config(
                &flow_name,
                &self.current_stage,
                iteration,
                stage_def,
                input,
                &description,
                interactive,
            ) {
                Ok(cfg) => cfg,
                Err(e) => {
                    return self.fail_flow(
                        &run_id,
                        &flow_name,
                        format!("Stage config resolution failed: {}", e),
                    );
                }
            };

            tracing::debug!(
                flow_id = %self.flow_id,
                stage = %self.current_stage,
                iteration = iteration,
                step = self.steps,
                "Executing stage"
            );

            self.emit_stage_event(
                &run_id,
                &flow_name,
                &self.current_stage,
                "stage_started",
                None,
                None,
            );

            // Create Stage object and execute
            let stage = match Stage::new(
                stage_cfg,
                self.db.clone(),
                self.chat_session.clone(),
                &self.config_loader,
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    return self.fail_flow(
                        &run_id,
                        &flow_name,
                        format!("Stage creation failed: {}", e),
                    );
                }
            };

            let stage_result = match stage.execute().await {
                Ok(r) => r,
                Err(e) => {
                    return self.fail_flow(
                        &run_id,
                        &flow_name,
                        format!("Stage execution error: {}", e),
                    );
                }
            };

            match stage_result.status().clone() {
                StageStatus::Completed => {
                    tracing::debug!(
                        flow_id = %self.flow_id,
                        stage = %self.current_stage,
                        "Stage completed"
                    );

                    let (stage_summary, verdict_payload) =
                        completed_event_fields(stage_result.output_artifacts());
                    self.emit_stage_event(
                        &run_id,
                        &flow_name,
                        &self.current_stage,
                        "stage_completed",
                        stage_summary.as_deref(),
                        verdict_payload.as_deref(),
                    );
                    if let Some(output_str) = stage_result.output_artifacts() {
                        self.store_artifact(&run_id, &flow_name, &self.current_stage, output_str);
                    }
                    self.previous_stage = Some(self.current_stage.clone());
                    if let Some(output_str) = stage_result.output_artifacts()
                        && let Ok(output_val) =
                            serde_json::from_str::<serde_json::Value>(output_str)
                    {
                        self.artifacts
                            .insert(self.current_stage.clone(), output_val);
                    }
                }
                StageStatus::Failed => {
                    let error_msg = stage_result
                        .error()
                        .map(|s| s.to_string())
                        .or_else(|| stage_result.output_artifacts().map(|s| s.to_string()))
                        .unwrap_or_else(|| "Unknown error".to_string());
                    self.emit_stage_event(
                        &run_id,
                        &flow_name,
                        &self.current_stage,
                        "stage_failed",
                        Some(&error_msg),
                        None,
                    );
                    tracing::debug!(
                        flow_id = %self.flow_id,
                        stage = %self.current_stage,
                        error = %error_msg,
                        "Stage failed"
                    );

                    let count = {
                        let c = self
                            .iteration_counts
                            .entry(self.current_stage.clone())
                            .or_insert(0);
                        *c += 1;
                        *c
                    };
                    if budget_denial_is_terminal(&error_msg) {
                        return self.fail_flow(&run_id, &flow_name, &error_msg);
                    }
                    if stage_retry_exhausted(count, max_iterations) {
                        return self.fail_flow(
                            &run_id,
                            &flow_name,
                            format!(
                                "Stage '{}' failed {} times (max iterations); last error: {}",
                                self.current_stage, count, error_msg
                            ),
                        );
                    }
                    continue;
                }
                StageStatus::WaitingForInput => {
                    let stage_summary = summarize_stage_output(stage_result.output_artifacts());
                    self.emit_stage_event(
                        &run_id,
                        &flow_name,
                        &self.current_stage,
                        "stage_waiting_input",
                        stage_summary.as_deref(),
                        None,
                    );
                    tracing::debug!(
                        flow_id = %self.flow_id,
                        stage = %self.current_stage,
                        "Stage waiting for input"
                    );

                    let message = stage_result
                        .output_artifacts()
                        .unwrap_or_default()
                        .to_string();
                    return FlowOutcome::WaitingForInput {
                        flow_id: self.flow_id.clone(),
                        run_id: run_id.clone(),
                        flow_name: flow_name.clone(),
                        stage_name: std::mem::take(&mut self.current_stage),
                        stage_id: stage_result.id().to_string(),
                        message,
                    };
                }
                StageStatus::TimedOut => {
                    self.emit_stage_event(
                        &run_id,
                        &flow_name,
                        &self.current_stage,
                        "stage_timed_out",
                        None,
                        None,
                    );
                    tracing::debug!(
                        flow_id = %self.flow_id,
                        stage = %self.current_stage,
                        "Stage timed out"
                    );

                    return FlowOutcome::TimedOut {
                        flow_id: self.flow_id.clone(),
                        run_id: run_id.clone(),
                        flow_name: flow_name.clone(),
                        stage_name: std::mem::take(&mut self.current_stage),
                    };
                }
                StageStatus::Pending | StageStatus::Running => {
                    return self.fail_flow(&run_id, &flow_name, "Stage in unexpected state");
                }
            }

            self.steps += 1;

            // FR-16: routing consumes the just-completed stage's validated
            // verdict. A missing/unroutable verdict fails the flow LOUDLY —
            // never a silent `success` default, never a silent advance.
            let next = match self.resolve_next(&self.current_stage, flow_def, &self.artifacts) {
                Ok(next) => next,
                Err(reason) => return self.fail_flow(&run_id, &flow_name, reason),
            };
            match next {
                NextStage::Stage(name) => {
                    self.current_stage = name;
                }
                NextStage::FlowDone => {
                    if let Err(e) = self.db.complete_flow(&self.flow_id) {
                        tracing::error!(flow_id = %self.flow_id, error = %e, "Failed to complete flow");
                        return self.fail_flow(
                            &run_id,
                            &flow_name,
                            format!("Cannot persist flow completion: {e}"),
                        );
                    }
                    return FlowOutcome::Completed {
                        flow_id: self.flow_id.clone(),
                        run_id: run_id.clone(),
                        flow_name: flow_name.clone(),
                        artifacts: std::mem::take(&mut self.artifacts),
                    };
                }
                NextStage::Escalate => {
                    return self.fail_flow(
                        &run_id,
                        &flow_name,
                        format!(
                            "Stage '{}' escalated — requires human intervention",
                            self.current_stage
                        ),
                    );
                }
            }
        }
    }

    fn create_flow_record(
        &self,
        run_id: &str,
        flow_name: &str,
        flow_number: Option<u64>,
        worktree_path: Option<String>,
        branch_name: Option<String>,
    ) -> FlowRecord {
        FlowRecord::new(
            self.config.flow_id.clone(),
            tj_db::RunId(run_id.into()),
            flow_name.into(),
            flow_number,
            worktree_path,
            branch_name,
        )
    }

    fn fail_flow(&self, run_id: &str, flow_name: &str, error: impl AsRef<str>) -> FlowOutcome {
        if let Err(e) = self
            .db
            .update_flow_status(&self.flow_id, FlowStatus::Failed)
        {
            tracing::error!(flow_id = %self.flow_id, error = %e, "Failed to update flow status to Failed");
        }
        FlowOutcome::Failed {
            flow_id: self.flow_id.clone(),
            run_id: run_id.into(),
            flow_name: flow_name.into(),
            error: error.as_ref().into(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_stage_config(
        &self,
        flow_name: &str,
        stage_name: &str,
        iteration: u32,
        stage_def: StageDef,
        input: serde_json::Value,
        description: &str,
        interactive: bool,
    ) -> anyhow::Result<StageConfig> {
        let common = (
            self.config.run_id.clone(),
            self.flow_id.clone(),
            flow_name.to_string(),
            stage_name.to_string(),
            self.previous_stage.clone(),
            iteration,
            stage_def,
            input,
            description.to_string(),
            self.config.workspace_path.clone(),
            interactive,
        );
        if let Some(bundle) = &self.config.bundle {
            let frozen = bundle
                .stage(flow_name, stage_name)
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "frozen execution bundle for run '{}' has no stage '{}:{}'",
                        self.config.run_id,
                        flow_name,
                        stage_name
                    )
                })?;
            let (
                run_id,
                flow_id,
                flow_name,
                stage_name,
                previous_stage,
                iteration,
                stage_def,
                input,
                description,
                worktree_path,
                interactive,
            ) = common;
            StageConfig::resolve_frozen(
                run_id.clone(),
                run_id,
                flow_id,
                flow_name,
                stage_name,
                previous_stage,
                iteration,
                stage_def,
                input,
                description,
                worktree_path,
                interactive,
                frozen,
            )
        } else {
            let (
                run_id,
                flow_id,
                flow_name,
                stage_name,
                previous_stage,
                iteration,
                stage_def,
                input,
                description,
                worktree_path,
                interactive,
            ) = common;
            StageConfig::resolve(
                run_id.clone(),
                run_id,
                flow_id,
                flow_name,
                stage_name,
                previous_stage,
                iteration,
                stage_def,
                input,
                description,
                worktree_path,
                interactive,
                &self.config_loader,
            )
        }
    }

    fn build_stage_input(
        &self,
        description: &str,
        flow_name: &str,
        current_stage: &str,
        artifacts: &HashMap<String, serde_json::Value>,
    ) -> serde_json::Value {
        let mut input = serde_json::Map::new();
        input.insert(
            "description".into(),
            serde_json::Value::String(description.into()),
        );
        input.insert(
            "current_stage".into(),
            serde_json::Value::String(current_stage.into()),
        );
        input.insert(
            "current_flow".into(),
            serde_json::Value::String(flow_name.into()),
        );

        let mut completed = serde_json::Map::new();
        for (k, v) in artifacts {
            completed.insert(k.clone(), v.clone());
        }
        input.insert(
            "completed_artifacts".into(),
            serde_json::Value::Object(completed),
        );

        serde_json::Value::Object(input)
    }

    /// Resolve the next step after `current_stage` completed — verdict-first
    /// routing against the stage's validated artifact (FR-16, plan T-6).
    ///
    /// Contract (fail-open removed):
    /// - the artifact MUST carry a `verdict` string. The runner (plan T-3)
    ///   completes a stage only with validated `verdict` tool arguments, so a
    ///   completed stage always has one in practice; a missing/invalid verdict
    ///   is a LOUD flow failure (`FlowOutcome::Failed` with a naming reason) —
    ///   never a silent `success` default, never a silent advance;
    /// - the verdict must match one of the stage's declared route keys; an
    ///   unknown value (config/artifact mismatch) surfaces as a loud failure;
    /// - Missing stage definitions and empty route tables are loud failures;
    ///   they can never silently complete a flow.
    ///
    /// Both resolved routings and their failure reasons are logged as
    /// `stage_verdict` stage-log rows (FR-23e, AC-8 evidence).
    fn resolve_next(
        &self,
        current_stage: &str,
        flow_def: &FlowDef,
        artifacts: &HashMap<String, serde_json::Value>,
    ) -> Result<NextStage, String> {
        let stage_def = match flow_def.stages.get(current_stage) {
            Some(s) => s,
            None => {
                return Err(format!(
                    "unknown stage '{}' in flow '{}'",
                    current_stage, self.config.flow_name
                ));
            }
        };

        if stage_def.routes.is_empty() {
            return Err(format!(
                "stage '{}' in flow '{}' has no routes",
                current_stage, self.config.flow_name
            ));
        }

        match route_on_verdict(current_stage, stage_def, flow_def, artifacts) {
            Ok(routing) => {
                self.log_stage_verdict(
                    &self.config.run_id,
                    &self.config.flow_name,
                    current_stage,
                    &serde_json::json!({
                        "verdict": routing.verdict,
                        "target": routing.target,
                    })
                    .to_string(),
                );
                Ok(routing.next)
            }
            Err(reason) => {
                self.log_stage_verdict(
                    &self.config.run_id,
                    &self.config.flow_name,
                    current_stage,
                    &reason,
                );
                Err(reason)
            }
        }
    }

    /// FR-23e: persist the routing decision (or its failure reason) as a
    /// `stage_verdict` stage-log row — same plumbing as `DbStageLogSink`.
    /// Best-effort: the row must never break routing over a logging error.
    fn log_stage_verdict(&self, run_id: &str, flow_name: &str, stage_name: &str, content: &str) {
        let stage_id = self
            .db
            .load_stage_by_name(&self.flow_id, stage_name)
            .ok()
            .flatten()
            .map(|s| s.id().to_string())
            .unwrap_or_default();
        let row = tj_db::StageLog {
            id: 0,
            run_id: tj_db::RunId(run_id.to_string()),
            stage_id,
            stage_name: Some(stage_name.to_string()),
            flow_name: Some(flow_name.to_string()),
            kind: STAGE_VERDICT_LOG_KIND.to_string(),
            content: content.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(e) = self.db.append_stage_log(&row) {
            tracing::warn!(run_id = %run_id, stage = %stage_name, error = %e, "Failed to append stage_verdict row");
        }
    }
}

/// Resolve a declared SMG route target to the next flow step.
fn resolve_target(target: &RouteTarget, flow_def: &FlowDef) -> Result<NextStage, String> {
    match target {
        RouteTarget::Stage(s) => match s.as_str() {
            "_next-flow" | "_end" | "_exit" => Ok(NextStage::FlowDone),
            "_escalate" => {
                tracing::debug!(flow_type = ?flow_def.flow_type, "Escalation requested via route target");
                Ok(NextStage::Escalate)
            }
            name if flow_def.stages.contains_key(name) => Ok(NextStage::Stage(name.into())),
            _ => Err(format!("unknown route target '{}'", s)),
        },
        RouteTarget::FlowStage { flow, stage } => Err(format!(
            "unsupported cross-flow route target {{flow: '{}', stage: '{}'}}; use flow-order",
            flow, stage
        )),
    }
}

#[derive(Debug, PartialEq)]
enum NextStage {
    Stage(String),
    FlowDone,
    Escalate,
}

#[cfg(test)]
mod stage_summary_tests {
    use super::summarize_stage_output;

    #[test]
    fn prefers_content_field() {
        let out = r#"{"content":"Requirements captured","message":"other"}"#;
        assert_eq!(
            summarize_stage_output(Some(out)).as_deref(),
            Some("Requirements captured")
        );
    }

    #[test]
    fn falls_back_to_message_then_summary() {
        assert_eq!(
            summarize_stage_output(Some(r#"{"message":"msg text"}"#)).as_deref(),
            Some("msg text")
        );
        assert_eq!(
            summarize_stage_output(Some(r#"{"summary":"sum text"}"#)).as_deref(),
            Some("sum text")
        );
    }

    #[test]
    fn takes_first_nonempty_line() {
        assert_eq!(
            summarize_stage_output(Some("line one\n\nline two")).as_deref(),
            Some("line one")
        );
    }

    #[test]
    fn truncates_on_char_boundary() {
        let long = "x".repeat(200);
        let s = summarize_stage_output(Some(long.as_str())).unwrap();
        assert_eq!(s.chars().count(), 121); // 120 + ellipsis
        assert!(s.ends_with('…'));
    }

    #[test]
    fn json_without_known_keys_yields_none() {
        assert_eq!(
            summarize_stage_output(Some(r#"{"status":"ok","n":3}"#)),
            None
        );
        assert_eq!(summarize_stage_output(Some(r#"{"content":123}"#)), None);
    }

    #[test]
    fn json_without_known_keys_scans_first_string_value() {
        let nested = r#"{"dimension_summary":{"correctness":"Deliverable satisfies every criterion"},"count":3}"#;
        assert_eq!(
            summarize_stage_output(Some(nested)).as_deref(),
            Some("Deliverable satisfies every criterion")
        );
        // hashes/ids and tiny strings are skipped
        assert_eq!(
            summarize_stage_output(Some(r#"{"hash":"deadbeef01","n":3}"#)),
            None
        );
    }

    #[test]
    fn passthrough_non_json_and_none() {
        assert_eq!(
            summarize_stage_output(Some("plain text")).as_deref(),
            Some("plain text")
        );
        assert_eq!(summarize_stage_output(None), None);
        assert_eq!(summarize_stage_output(Some("{\"content\":\"\"}")), None);
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use tj_config::smg::{FlowType, SmgLimits, WorkflowMaturity};
    use tj_db::{FlowId, PipelineRun, RunStatus, StageRecord};

    const MISSING_ERR: &str = "stage 'start' produced no routable verdict (missing artifact key) — routing is unreachable without a validated artifact";

    // ---------- harness ----------

    fn route_map(routes: &[(&str, &str)]) -> HashMap<String, RouteTarget> {
        routes
            .iter()
            .map(|(k, v)| (k.to_string(), RouteTarget::Stage(v.to_string())))
            .collect()
    }

    fn stage_def(routes: &[(&str, &str)]) -> StageDef {
        StageDef {
            agent: "agent.json".into(),
            interactive: false,
            routes: route_map(routes),
            runner_type: None,
            timeout: 600,
        }
    }

    fn flow_def(stages: Vec<(&str, StageDef)>) -> FlowDef {
        let exit = stages.last().map(|(name, _)| name.to_string());
        FlowDef {
            flow_type: FlowType::Flow,
            entry: stages.first().map(|(k, _)| k.to_string()),
            exit,
            stages: stages
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            routes: HashMap::new(),
            merge_strategy: None,
            pr_config: None,
        }
    }

    fn artifacts(entries: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn verdict_artifact(verdict: &str) -> serde_json::Value {
        serde_json::json!({ "verdict": verdict, "content": format!("stage said {}", verdict) })
    }

    /// Route `start`'s artifact through the pure core of resolve_next.
    fn route_start(
        flow: &FlowDef,
        arts: &HashMap<String, serde_json::Value>,
    ) -> Result<NextStage, String> {
        let stage = flow.stages.get("start").unwrap();
        route_on_verdict("start", stage, flow, arts).map(|r| r.next)
    }

    fn test_config_loader() -> Arc<ConfigLoader> {
        static TREE: std::sync::OnceLock<tj_test_support::SyntheticTree> =
            std::sync::OnceLock::new();
        let tree = TREE.get_or_init(tj_test_support::SyntheticTree::new);
        Arc::new(
            ConfigLoader::from_file(tree.config_path())
                .expect("synthetic fixture config must load"),
        )
    }

    fn test_db() -> Arc<Database> {
        let dir = std::env::temp_dir().join(format!("tjflow-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(Database::open(&dir.join("state.db").to_string_lossy()).expect("test db"))
    }

    fn test_smg() -> StateMachineGraph {
        StateMachineGraph {
            version: 1,
            name: "test".into(),
            maturity: WorkflowMaturity::Experimental,
            description: String::new(),
            catalog: None,
            capabilities: None,
            entry: "flow-a".into(),
            exit: "flow-a".into(),
            limits: SmgLimits {
                max_iterations: 3,
                max_steps: 20,
                max_depth: 5,
            },
            flow_order: vec!["flow-a".into()],
            flows: [(
                "flow-a".to_string(),
                flow_def(vec![(
                    "start",
                    stage_def(&[("success", "_exit"), ("reject", "start")]),
                )]),
            )]
            .into_iter()
            .collect(),
        }
    }

    fn test_flow(db: Arc<Database>, run_id: &str) -> Flow {
        Flow::new(
            db.clone(),
            test_config_loader(),
            None,
            FlowConfig {
                flow_id: db
                    .load_flows_for_run(run_id)
                    .unwrap()
                    .last()
                    .map(|f| f.id().to_string())
                    .unwrap_or_else(|| format!("flow_{}", uuid::Uuid::new_v4())),
                run_id: run_id.to_string(),
                flow_name: "flow-a".into(),
                flow_number: Some(1),
                smg: test_smg(),
                description: "test".into(),
                existing_artifacts: HashMap::new(),
                workspace_path: None,
                branch_name: None,
                slug: "test".into(),
                attempt_offset: 0,
                bundle: None,
            },
        )
    }

    struct ResumeFixture {
        db: Arc<Database>,
        run_id: String,
        stage_id: String,
    }

    /// Seed run + Running flow + a Completed `start` stage row carrying
    /// `artifact_json`, as a flow re-entry would find them.
    fn seed_resume(artifact_json: Option<&str>) -> ResumeFixture {
        let db = test_db();
        let run_id = format!("run-{}", uuid::Uuid::new_v4());
        let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
        let stage_id = format!("stage-{}", uuid::Uuid::new_v4());
        db.save_run(&PipelineRun::new(
            run_id.clone(),
            None,
            None,
            "test run".into(),
            RunStatus::Running,
            None,
            None,
            None,
            None,
        ))
        .unwrap();
        db.save_flow(&FlowRecord::new(
            flow_id.clone(),
            tj_db::RunId(run_id.clone()),
            "flow-a".into(),
            Some(1),
            None,
            None,
        ))
        .unwrap();
        let mut stage = StageRecord::new(
            stage_id.clone(),
            FlowId(flow_id.clone()),
            "start".into(),
            StageStatus::Completed,
            0,
            None,
        );
        stage.set_output_artifacts(artifact_json.map(|s| s.to_string()));
        db.save_stage(&stage).unwrap();
        ResumeFixture {
            db,
            run_id,
            stage_id,
        }
    }

    /// Drive the real `resume` path (public API): reconstructs artifacts from
    /// the DB stage row, then routes via resolve_next + fail_flow mapping.
    async fn resumed_outcome(artifact_json: Option<&str>) -> (FlowOutcome, ResumeFixture) {
        let fx = seed_resume(artifact_json);
        let flow = test_flow(fx.db.clone(), &fx.run_id);
        (flow.resume("start").await, fx)
    }

    fn stage_verdict_rows(fx: &ResumeFixture) -> Vec<tj_db::StageLog> {
        fx.db
            .list_stage_logs(&fx.run_id, 0, 200)
            .unwrap()
            .into_iter()
            .filter(|l| l.kind == STAGE_VERDICT_LOG_KIND)
            .collect()
    }

    // ---------- resolve_next contract (FR-16, pure core) ----------

    #[test]
    fn proceed_verdict_routes_to_next_flow_done() {
        let flow = flow_def(vec![(
            "start",
            stage_def(&[("proceed", "_next-flow"), ("reject", "start")]),
        )]);
        let arts = artifacts(&[("start", verdict_artifact("proceed"))]);
        assert_eq!(route_start(&flow, &arts), Ok(NextStage::FlowDone));
    }

    #[test]
    fn named_stage_verdict_routes_to_next_stage() {
        let flow = flow_def(vec![(
            "start",
            stage_def(&[("proceed", "_next-flow"), ("reject", "start")]),
        )]);
        let arts = artifacts(&[("start", verdict_artifact("reject"))]);
        assert_eq!(
            route_start(&flow, &arts),
            Ok(NextStage::Stage("start".into()))
        );
    }

    #[test]
    fn fan_out_verdict_routes_to_declared_stage() {
        // test-style fan-out: `failed` re-enters implement (flow.rs retry loop
        // then re-executes it from scratch — FR-13 interplay).
        let flow = flow_def(vec![
            (
                "test",
                stage_def(&[
                    ("success", "review"),
                    ("failed", "implement"),
                    ("cannot-complete", "_escalate"),
                ]),
            ),
            ("implement", stage_def(&[("success", "test")])),
            ("review", stage_def(&[("success", "_next-flow")])),
        ]);
        let arts = artifacts(&[("test", verdict_artifact("failed"))]);
        let stage = flow.stages.get("test").unwrap();
        let r = route_on_verdict("test", stage, &flow, &arts).unwrap();
        assert_eq!(r.next, NextStage::Stage("implement".into()));
        assert_eq!(r.target, "implement");
        assert_eq!(r.verdict, "failed");
    }

    #[test]
    fn exit_target_resolves_to_flow_done_observe_case() {
        // observe routes success → _exit (synthetic fixture contract): a
        // validated `verdict: "success"` completes the flow exactly like any
        // conditional-route stage — the routes-less exception never fires.
        let flow = flow_def(vec![("observe", stage_def(&[("success", "_exit")]))]);
        let arts = artifacts(&[("observe", verdict_artifact("success"))]);
        let stage = flow.stages.get("observe").unwrap();
        let r = route_on_verdict("observe", stage, &flow, &arts).unwrap();
        assert_eq!(r.next, NextStage::FlowDone);
        assert_eq!(r.target, "_exit");
        assert_eq!(r.verdict, "success");
    }

    #[test]
    fn escalate_target_resolves_to_escalate() {
        let flow = flow_def(vec![(
            "start",
            stage_def(&[("success", "_next-flow"), ("cannot-complete", "_escalate")]),
        )]);
        let arts = artifacts(&[("start", verdict_artifact("cannot-complete"))]);
        assert_eq!(route_start(&flow, &arts), Ok(NextStage::Escalate));
    }

    #[test]
    fn missing_artifact_fails_loudly() {
        let flow = flow_def(vec![("start", stage_def(&[("success", "_next-flow")]))]);
        assert_eq!(
            route_start(&flow, &artifacts(&[])),
            Err(MISSING_ERR.to_string())
        );
    }

    #[test]
    fn artifact_without_verdict_key_fails_loudly() {
        let flow = flow_def(vec![("start", stage_def(&[("success", "_next-flow")]))]);
        let arts = artifacts(&[("start", serde_json::json!({"content": "legacy artifact"}))]);
        assert_eq!(route_start(&flow, &arts), Err(MISSING_ERR.to_string()));
    }

    #[test]
    fn non_string_verdict_fails_loudly() {
        let flow = flow_def(vec![("start", stage_def(&[("success", "_next-flow")]))]);
        let arts = artifacts(&[("start", serde_json::json!({"verdict": 3}))]);
        assert_eq!(route_start(&flow, &arts), Err(MISSING_ERR.to_string()));
    }

    #[test]
    fn unknown_verdict_fails_loudly_config_mismatch() {
        // Proves the implicit routes["success"] fallback is gone: `banana`
        // used to route via that fallback; now it surfaces the mismatch.
        let flow = flow_def(vec![("start", stage_def(&[("success", "_next-flow")]))]);
        let arts = artifacts(&[("start", verdict_artifact("banana"))]);
        assert_eq!(
            route_start(&flow, &arts),
            Err("unknown verdict 'banana' for stage 'start'".to_string()),
        );
    }

    #[test]
    fn declared_success_verdict_still_routes_start_case() {
        // AC-1: `verdict: "success"` routes when success is a DECLARED key —
        // removing the fallback must not break the vocabulary itself.
        let flow = flow_def(vec![("start", stage_def(&[("success", "_next-flow")]))]);
        let arts = artifacts(&[("start", verdict_artifact("success"))]);
        assert_eq!(route_start(&flow, &arts), Ok(NextStage::FlowDone));
    }

    // ---------- resolve_next defensive arms (kept per plan revision 5) ----------

    #[test]
    fn routes_less_stage_fails_instead_of_succeeding() {
        let flow = test_flow(test_db(), "run-test");
        let f = flow_def(vec![("start", stage_def(&[]))]);
        assert_eq!(
            flow.resolve_next("start", &f, &artifacts(&[])),
            Err("stage 'start' in flow 'flow-a' has no routes".into())
        );
    }

    #[test]
    fn unknown_stage_fails_instead_of_succeeding() {
        let flow = test_flow(test_db(), "run-test");
        let f = flow_def(vec![("start", stage_def(&[("success", "_next-flow")]))]);
        assert_eq!(
            flow.resolve_next("no-such-stage", &f, &artifacts(&[])),
            Err("unknown stage 'no-such-stage' in flow 'flow-a'".into())
        );
    }

    #[test]
    fn unknown_and_cross_flow_targets_are_errors() {
        let flow = flow_def(vec![("start", stage_def(&[("success", "not-a-stage")]))]);
        let arts = artifacts(&[("start", verdict_artifact("success"))]);
        assert_eq!(
            route_start(&flow, &arts),
            Err("unknown route target 'not-a-stage'".into())
        );

        let mut flow = flow_def(vec![("start", stage_def(&[("success", "_exit")]))]);
        flow.stages.get_mut("start").unwrap().routes.insert(
            "success".into(),
            RouteTarget::FlowStage {
                flow: "later".into(),
                stage: "start".into(),
            },
        );
        assert_eq!(route_start(&flow, &arts), Err("unsupported cross-flow route target {flow: 'later', stage: 'start'}; use flow-order".into()));
    }

    // ---------- resume: legacy/loud-failure semantics (Migration §5.2) ----------

    #[tokio::test]
    async fn resume_missing_verdict_fails_flow_loudly() {
        let (outcome, fx) = resumed_outcome(Some(r#"{"content":"legacy artifact"}"#)).await;
        match outcome {
            FlowOutcome::Failed { error, .. } => assert_eq!(error, MISSING_ERR),
            other => panic!("expected loud Failed, got {:?}", other),
        }
        // The failure is logged as stage_verdict evidence (AC-8)…
        let rows = stage_verdict_rows(&fx);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].content, MISSING_ERR);
        assert_eq!(rows[0].stage_id, fx.stage_id);
        // …and the flow record is marked Failed, not silently advancing.
        let flows = fx.db.load_flows_for_run(&fx.run_id).unwrap();
        assert!(flows.iter().any(|f| *f.status() == FlowStatus::Failed));
    }

    #[tokio::test]
    async fn resume_artifact_absent_fails_flow_loudly() {
        // Stage row with NO output at all: resume cannot reconstruct an
        // artifact → the same loud failure, not a silent success default.
        let (outcome, fx) = resumed_outcome(None).await;
        match outcome {
            FlowOutcome::Failed { error, .. } => assert_eq!(error, MISSING_ERR),
            other => panic!("expected loud Failed, got {:?}", other),
        }
        assert_eq!(stage_verdict_rows(&fx).len(), 1);
    }

    #[tokio::test]
    async fn resume_unknown_verdict_fails_flow_loudly() {
        let (outcome, fx) = resumed_outcome(Some(r#"{"verdict":"banana","content":"x"}"#)).await;
        match outcome {
            FlowOutcome::Failed { error, .. } => {
                assert_eq!(error, "unknown verdict 'banana' for stage 'start'");
            }
            other => panic!("expected loud Failed, got {:?}", other),
        }
        let rows = stage_verdict_rows(&fx);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].content,
            "unknown verdict 'banana' for stage 'start'"
        );
    }

    #[tokio::test]
    async fn resume_valid_verdict_routes_and_completes() {
        let (outcome, fx) =
            resumed_outcome(Some(r#"{"verdict":"success","content":"captured"}"#)).await;
        match outcome {
            FlowOutcome::Completed { artifacts, .. } => {
                assert_eq!(artifacts["start"]["verdict"], "success");
            }
            other => panic!("expected Completed, got {:?}", other),
        }
        let rows = stage_verdict_rows(&fx);
        assert_eq!(rows.len(), 1);
        let logged: serde_json::Value = serde_json::from_str(&rows[0].content).unwrap();
        assert_eq!(logged["verdict"], "success");
        assert_eq!(logged["target"], "_exit");
        assert_eq!(rows[0].stage_id, fx.stage_id);
    }

    // ---------- stage_completed event payload (FR-24) ----------

    #[test]
    fn completed_event_payload_carries_verdict() {
        let (summary, payload) = completed_event_fields(Some(
            r#"{"verdict":"proceed","content":"Analysis complete","summary":"done"}"#,
        ));
        assert_eq!(summary.as_deref(), Some("Analysis complete"));
        let p: serde_json::Value = serde_json::from_str(&payload.expect("payload")).unwrap();
        assert_eq!(p["verdict"], "proceed");
    }

    #[test]
    fn completed_event_without_verdict_has_empty_payload() {
        let (summary, payload) = completed_event_fields(Some(r#"{"content":"no verdict here"}"#));
        assert_eq!(summary.as_deref(), Some("no verdict here"));
        assert!(payload.is_none());
    }

    #[test]
    fn completed_event_non_json_output_has_empty_payload() {
        assert!(completed_event_fields(Some("plain text")).1.is_none());
        assert!(completed_event_fields(None).1.is_none());
    }

    // ---------- FR-13: failure re-execution up to max_iterations ----------

    #[test]
    fn stage_retries_until_max_iterations_then_exhausts() {
        // A stage failing on `output contract violation` re-executes from
        // scratch: attempts 1..(max-1) retry, attempt `max` exhausts.
        assert!(!stage_retry_exhausted(1, 3));
        assert!(!stage_retry_exhausted(2, 3));
        assert!(stage_retry_exhausted(3, 3));
        assert!(stage_retry_exhausted(1, 1));
    }

    #[test]
    fn budget_denial_is_terminal_without_stage_retry() {
        assert!(budget_denial_is_terminal(
            "budget exhausted: model_calls (120/120)"
        ));
        assert!(budget_denial_is_terminal(
            "output contract violation: stage 'start' cannot be corrected: budget exhausted: output_corrections (1/1)"
        ));
        assert!(!budget_denial_is_terminal(
            "output contract violation: schema violation: verdict"
        ));
    }
}
