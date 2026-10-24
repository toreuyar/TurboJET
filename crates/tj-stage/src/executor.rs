use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tj_chat::ChatSession;
use tj_config::adc::AgentDefinitionConfig;
use tj_config::bundle::{FrozenStage, resolve_model_for_adc, resolve_provider_for_adc};
use tj_config::format::title_case;
use tj_config::loader::ConfigLoader;
use tj_config::smg::StageDef;
use tj_config::system::{ProviderConfig, ProviderType};
use tj_db::{Database, FlowId, ProviderUsage, RunId, StageLog, StageRecord, StageStatus};
use tj_provider::resilience::{
    ProviderAttemptRecord, ProviderObserver, ResiliencePolicy, ResilientLlm,
};
use tj_runner::{
    ADKRunner, BudgetGuard, LlmUsage, QuestionChannel, Runner, StageResult,
    StageStatus as RunnerStatus, TaskDefinition, UsageSink,
};

use crate::question_channel::ChatQuestionChannel;

pub struct StageConfig {
    run_id: String,
    job_id: String,
    flow_id: String,
    flow_name: String,
    stage_name: String,
    previous_stage: Option<String>,
    iteration: u32,
    stage_def: StageDef,
    input_artifacts: serde_json::Value,
    description: String,
    worktree_path: Option<PathBuf>,
    interactive: bool,

    // Resolved during construction
    adc: AgentDefinitionConfig,
    prompt_text: Option<String>,
    response_schema: Option<serde_json::Value>,
    provider: ProviderConfig,
    model: String,
}

impl StageConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        run_id: String,
        job_id: String,
        flow_id: String,
        flow_name: String,
        stage_name: String,
        previous_stage: Option<String>,
        iteration: u32,
        stage_def: StageDef,
        input_artifacts: serde_json::Value,
        description: String,
        worktree_path: Option<PathBuf>,
        interactive: bool,
        config_loader: &ConfigLoader,
    ) -> Result<Self> {
        let temp = TempResolve {
            stage_name: &stage_name,
            stage_def: &stage_def,
        };
        let adc = temp.load_adc(config_loader)?;
        let prompt_text = temp.load_prompt_text(config_loader, &adc)?;
        let response_schema = if adc.schema.trim().is_empty() {
            None
        } else {
            Some(config_loader.load_schema_required(&adc.schema)?)
        };
        let contract = format!("flow '{}' stage '{}'", flow_name, stage_name);
        let provider =
            resolve_provider_for_adc(&config_loader.config().providers, &adc, &contract)?;
        let model = resolve_model_for_adc(&provider, &adc, &contract)?;

        Ok(Self {
            run_id,
            job_id,
            flow_id,
            flow_name,
            stage_name,
            previous_stage,
            iteration,
            stage_def,
            input_artifacts,
            description,
            worktree_path,
            interactive,
            adc,
            prompt_text,
            response_schema,
            provider,
            model,
        })
    }

    /// Resolve a stage from the run's immutable execution bundle. No mutable
    /// config file is consulted; provider/model/limits come from the bundle.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_frozen(
        run_id: String,
        job_id: String,
        flow_id: String,
        flow_name: String,
        stage_name: String,
        previous_stage: Option<String>,
        iteration: u32,
        stage_def: StageDef,
        input_artifacts: serde_json::Value,
        description: String,
        worktree_path: Option<PathBuf>,
        interactive: bool,
        frozen: FrozenStage,
    ) -> Result<Self> {
        anyhow::ensure!(
            frozen.flow_name == flow_name && frozen.stage_name == stage_name,
            "execution bundle stage mismatch: expected {}:{} got {}:{}",
            flow_name,
            stage_name,
            frozen.flow_name,
            frozen.stage_name
        );
        Ok(Self {
            run_id,
            job_id,
            flow_id,
            flow_name,
            stage_name,
            previous_stage,
            iteration,
            stage_def,
            input_artifacts,
            description,
            worktree_path,
            interactive,
            adc: frozen.adc,
            prompt_text: Some(frozen.prompt),
            response_schema: frozen.response_schema,
            provider: frozen.provider,
            model: frozen.model,
        })
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    pub fn job_id(&self) -> &str {
        &self.job_id
    }
    pub fn flow_id(&self) -> &str {
        &self.flow_id
    }
    pub fn flow_name(&self) -> &str {
        &self.flow_name
    }
    pub fn stage_name(&self) -> &str {
        &self.stage_name
    }
    pub fn previous_stage(&self) -> Option<&str> {
        self.previous_stage.as_deref()
    }
    pub fn iteration(&self) -> u32 {
        self.iteration
    }
    pub fn stage_def(&self) -> &StageDef {
        &self.stage_def
    }
    pub fn input_artifacts(&self) -> &serde_json::Value {
        &self.input_artifacts
    }
    pub fn description(&self) -> &str {
        &self.description
    }
    pub fn worktree_path(&self) -> Option<&PathBuf> {
        self.worktree_path.as_ref()
    }
    pub fn interactive(&self) -> bool {
        self.interactive
    }
    pub fn adc(&self) -> &AgentDefinitionConfig {
        &self.adc
    }
    pub fn prompt_text(&self) -> Option<&str> {
        self.prompt_text.as_deref()
    }
    pub fn response_schema(&self) -> Option<&serde_json::Value> {
        self.response_schema.as_ref()
    }
    pub fn provider(&self) -> &ProviderConfig {
        &self.provider
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn adapter_timeout_secs(&self) -> u64 {
        self.adc.timeout
    }
}

struct TempResolve<'a> {
    stage_name: &'a str,
    stage_def: &'a StageDef,
}

impl<'a> TempResolve<'a> {
    fn load_adc(&self, config_loader: &ConfigLoader) -> Result<AgentDefinitionConfig> {
        let adc = config_loader.load_adc_reference(self.stage_name, &self.stage_def.agent)?;
        tracing::debug!(adc_id = %adc.id, tools = ?adc.tools, "Loaded ADC");
        Ok(adc)
    }

    fn load_prompt_text(
        &self,
        config_loader: &ConfigLoader,
        adc: &AgentDefinitionConfig,
    ) -> Result<Option<String>> {
        config_loader
            .load_prompt_for_adc(self.stage_name, &self.stage_def.agent, &adc.prompt)
            .map(Some)
    }
}

pub struct Stage {
    db: Arc<Database>,
    chat_session: Option<Arc<dyn ChatSession>>,
    config: StageConfig,
    record: StageRecord,
    runner: ADKRunner,
}

impl Stage {
    pub async fn new(
        config: StageConfig,
        db: Arc<Database>,
        chat_session: Option<Arc<dyn ChatSession>>,
        _config_loader: &ConfigLoader,
    ) -> Result<Self> {
        // Create stage record
        let stage_id = format!("stage_{}", uuid::Uuid::new_v4());
        let record = StageRecord::new(
            stage_id.clone(),
            FlowId(config.flow_id().to_string()),
            config.stage_name().to_string(),
            StageStatus::Running,
            config.iteration(),
            Some(serde_json::to_string(config.input_artifacts())?),
        );
        db.save_stage(&record)?;

        // Prepare worktree directories
        if let Some(worktree) = config.worktree_path() {
            let stage_dir = worktree
                .join(".turbojet")
                .join(config.stage_name())
                .join(config.run_id());
            std::fs::create_dir_all(&stage_dir)?;
            tracing::debug!(path = %stage_dir.display(), "Created stage data directory in worktree");
        }

        // Provider/model identity is resolved once: from the immutable bundle
        // for new runs, or at stage start for explicit legacy runs.
        let provider = config.provider().clone();
        let model_name = config.model().to_string();

        let api_key = std::env::var(&provider.api_key_env)
            .with_context(|| format!("API key not found in env var: {}", provider.api_key_env))?;

        tracing::debug!(
            stage = %config.stage_name(),
            provider_id = %provider.id,
            model = %model_name,
            "Creating ADKRunner for stage"
        );

        let llm: std::sync::Arc<dyn adk_core::Llm> = match provider.provider_type {
            ProviderType::OpenAi => {
                let client = if provider.base_url.is_empty() {
                    adk_model::openai::OpenAIClient::new(adk_model::openai::OpenAIConfig::new(
                        &api_key,
                        &model_name,
                    ))
                } else {
                    adk_model::openai::OpenAIClient::compatible(
                        &api_key,
                        &provider.base_url,
                        &model_name,
                    )
                }
                .context("Failed to create OpenAI client")?;
                // TJ-024: one retry owner. ADK's built-in transport retry is
                // disabled so rate-limit/transient/credential classes and
                // budgets are observed and enforced by ResilientLlm.
                std::sync::Arc::new(client.with_retry_config(adk_model::RetryConfig::disabled()))
            }
            ProviderType::Anthropic => {
                let mut config = adk_model::anthropic::AnthropicConfig::new(&api_key, &model_name);
                if !provider.base_url.is_empty() {
                    config = config.with_base_url(provider.base_url.clone());
                }
                let client = adk_model::anthropic::AnthropicClient::new(config)
                    .context("Failed to create Anthropic client")?;
                std::sync::Arc::new(client)
            }
            ProviderType::Gemini => {
                let model = adk_model::GeminiModel::new(&api_key, &model_name)
                    .context("Failed to create Gemini model")?;
                std::sync::Arc::new(model)
            }
            ProviderType::Custom => {
                let config = adk_model::openai_compatible::OpenAICompatibleConfig::new(
                    &api_key,
                    &model_name,
                )
                .with_provider_name(&provider.id)
                .with_base_url(provider.base_url.clone());
                let client = adk_model::openai_compatible::OpenAICompatible::new(config)
                    .context("Failed to create OpenAI-compatible client")?;
                std::sync::Arc::new(client.with_retry_config(adk_model::RetryConfig::disabled()))
            }
        };

        let provider_observer = Arc::new(DbProviderObserver {
            db: Arc::clone(&db),
            run_id: config.run_id().to_string(),
            stage_id: stage_id.clone(),
            stage_name: config.stage_name().to_string(),
            provider_id: provider.id.clone(),
        });
        let llm: Arc<dyn adk_core::Llm> = Arc::new(ResilientLlm::new(
            llm,
            provider.id.clone(),
            ResiliencePolicy::from_provider(&provider),
            Some(provider_observer),
        ));

        let sink = DbStageLogSink {
            db: Arc::clone(&db),
            run_id: config.run_id().to_string(),
            flow_name: config.flow_name().to_string(),
        };
        let mut runner = ADKRunner::new(llm).with_sink(Arc::new(sink));
        runner = runner.with_usage_sink(Arc::new(DbUsageSink {
            db: Arc::clone(&db),
            run_id: config.run_id().to_string(),
            stage_id: stage_id.clone(),
            model: model_name.clone(),
            input_cost_micros_per_million: provider.input_cost_micros_per_million,
            output_cost_micros_per_million: provider.output_cost_micros_per_million,
        }));
        if db.has_run_budget(config.run_id()).unwrap_or(false) {
            runner = runner.with_budget(Arc::new(StageBudgetGuard {
                db: Arc::clone(&db),
                run_id: config.run_id().to_string(),
            }));
        }
        // Interactive stages get the blocking question channel (T-5): the
        // runner's `question` tool blocks on the run's chat session (webui
        // bridge ∥ Zulip) with durable WaitingForInput ↔ Running flips around
        // each ask. Without any chat session the stage runs unattended via
        // the runner's silent-turn nudge path and the autonomous-nudge loop.
        let runner = if config.interactive() {
            match &chat_session {
                Some(session) => runner.with_question_channel(Arc::new(ChatQuestionChannel::new(
                    Arc::clone(&db),
                    Arc::clone(session),
                    config.run_id().to_string(),
                    config.flow_name().to_string(),
                    // T-12: the channel's re-ask bound mirrors the stage's
                    // output-retry bound (D-2) — the user gets at most N
                    // clarification re-asks before the ask errors visibly.
                    config.adc().max_output_retries,
                ))),
                None => runner,
            }
        } else {
            runner
        };

        Ok(Self {
            db,
            chat_session,
            config,
            record,
            runner,
        })
    }

    /// The attached question channel, if any (wiring inspection; plan T-5).
    pub fn question_channel(&self) -> Option<Arc<dyn QuestionChannel>> {
        self.runner.question_channel()
    }

    pub async fn execute(mut self) -> Result<StageRecord> {
        let stage_name = self.config.stage_name().to_string();
        let stage_id = self.record.id().to_string();

        tracing::debug!(
            stage = %stage_name,
            stage_id = %stage_id,
            interactive = self.config.interactive(),
            has_chat_session = self.chat_session.is_some(),
            "Stage::execute starting"
        );

        if let Err(error) = self.validate_required_input_artifacts() {
            self.fail_invalid_output(&error);
            self.db.save_stage(&self.record)?;
            self.send_terminal_notification(&stage_name);
            return Ok(self.record.clone());
        }

        self.send_started_notification(&stage_name).await;

        let prompt_text = self.build_prompt();
        let task = self.build_task_definition(&stage_id, prompt_text);

        // The shell isolation boundary canonicalizes the stage output directory
        // before any tool can run, and write/edit tools target it too. Create it
        // up front so a real non-interactive Engineering stage does not fail
        // closed merely because the per-stage directory is new.
        if let Err(error) = std::fs::create_dir_all(&task.output_dir) {
            let message = format!(
                "cannot create stage output directory {}: {error}",
                task.output_dir.display()
            );
            self.record.set_status(StageStatus::Failed);
            self.record.set_error(Some(message.clone()));
            self.record
                .set_completed_at(Some(chrono::Utc::now().to_rfc3339()));
            self.db.save_stage(&self.record)?;
            self.send_terminal_notification(&stage_name);
            return Ok(self.record.clone());
        }

        tracing::info!(stage_id = %stage_id, stage = %stage_name, worktree = ?self.config.worktree_path(), "Stage executing");

        match self.execute_runner(task).await? {
            Some(record) => {
                self.send_terminal_notification(&stage_name);
                tracing::info!(stage_id = %record.id(), stage = %stage_name, status = ?record.status(), "Stage completed");
                Ok(record)
            }
            None => {
                self.run_multi_turn(&stage_name).await?;
                self.send_terminal_notification(&stage_name);
                tracing::info!(stage_id = %self.record.id(), stage = %stage_name, status = ?self.record.status(), "Stage completed");
                Ok(self.record.clone())
            }
        }
    }

    async fn send_started_notification(&self, stage_name: &str) {
        if let Some(ref session) = self.chat_session {
            let display_name = title_case(stage_name);
            let msg = format!("**{}** stage started", display_name);
            match session.send(&msg).await {
                Ok(()) => tracing::debug!(stage = %stage_name, "Stage started notification sent"),
                Err(e) => {
                    tracing::warn!(stage = %stage_name, error = %e, "Failed to send stage started notification")
                }
            }
        }
    }

    fn build_prompt(&self) -> String {
        let mut prompt_text = self.config.prompt_text().unwrap_or_default().to_string();

        if let Some(_worktree) = self.config.worktree_path() {
            let output_dir = format!(
                ".turbojet/{}/{}",
                self.config.stage_name(),
                self.config.run_id()
            );
            prompt_text.push_str(&format!(
                "\n\n## Data Paths\n- **Your output directory**: `{}/`\n  Write your results to this directory as `output.md`.",
                output_dir
            ));

            if let Some(prev) = self.config.previous_stage() {
                let input_dir = format!(".turbojet/{}/{}", prev, self.config.run_id());
                prompt_text.push_str(&format!(
                    "\n- **Previous stage output**: `{}/`\n  Read the previous stage's results from this directory.",
                    input_dir
                ));
            }

            prompt_text.push_str("\n- **Project description**: See the input artifacts below.");
        }

        prompt_text
    }

    fn build_task_definition(&self, stage_id: &str, prompt_text: String) -> TaskDefinition {
        let output_file = if self.config.interactive() {
            self.config.worktree_path().map(|worktree| {
                worktree
                    .join(format!(
                        ".turbojet/{}/{}",
                        self.config.stage_name(),
                        self.config.run_id()
                    ))
                    .join("output.md")
                    .to_str()
                    .unwrap_or_default()
                    .to_string()
            })
        } else {
            None
        };

        TaskDefinition {
            stage_id: stage_id.to_string(),
            job_id: self.config.job_id().to_string(),
            run_id: self.config.run_id().to_string(),
            prompt: String::new(),
            prompt_text: Some(prompt_text),
            input_artifacts: self.config.input_artifacts().clone(),
            output_dir: std::path::PathBuf::from(format!(
                "{}/tasks/{}",
                shellexpand::env("$TURBOJET_HOME/runs")
                    .map(|s| s.to_string())
                    .unwrap_or_else(|_| "/tmp/turbojet/runs".into()),
                stage_id
            )),
            output_file,
            timeout_secs: self.config.adc().timeout,
            runner_type: String::new(),
            adc: self.config.adc().clone(),
            worktree_path: self.config.worktree_path().cloned(),
            interactive: self.config.interactive(),
            response_schema: self.config.response_schema().cloned(),
        }
    }

    async fn execute_runner(&mut self, task: TaskDefinition) -> Result<Option<StageRecord>> {
        let stage_id = task.stage_id.clone();
        let stage_name = self.config.stage_name().to_string();

        let start = std::time::Instant::now();
        let result = self.runner.execute(task).await;
        self.refresh_active_record()?;
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(stage_id = %stage_id, error = %e, "Stage execution failed");
                self.record.set_status(StageStatus::Failed);
                self.record.set_error(Some(e.to_string()));
                self.record
                    .set_completed_at(Some(chrono::Utc::now().to_rfc3339()));
                self.db.save_stage(&self.record)?;
                return Ok(Some(self.record.clone()));
            }
        };
        let elapsed = start.elapsed();
        if elapsed.as_secs() >= 1 {
            tracing::warn!(elapsed_ms = elapsed.as_millis() as u64, stage = %stage_name, "Slow stage execution");
        } else {
            tracing::debug!(elapsed_ms = elapsed.as_millis() as u64, stage = %stage_name, "Stage execution completed");
        }

        self.process_runner_result(result);
        self.db.save_stage(&self.record)?;

        if *self.record.status() == StageStatus::WaitingForInput {
            tracing::debug!(
                stage_id = %stage_id,
                has_chat_session = self.chat_session.is_some(),
                "Stage waiting for input, entering multi-turn loop"
            );
            Ok(None)
        } else {
            Ok(Some(self.record.clone()))
        }
    }

    /// Persist one conversation turn (agent question / user reply) so the
    /// stage chat view can replay the exchange.
    fn append_stage_log(&self, kind: &str, content: &str) {
        let l = StageLog {
            id: 0,
            run_id: RunId(self.config.run_id().to_string()),
            stage_id: self.record.id().to_string(),
            stage_name: Some(self.config.stage_name().to_string()),
            flow_name: Some(self.config.flow_name().to_string()),
            kind: kind.to_string(),
            content: content.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(e) = self.db.append_stage_log(&l) {
            tracing::warn!(stage_id = %self.record.id(), kind = %kind, error = %e, "Failed to append conversation log");
        }
    }

    async fn run_multi_turn(&mut self, stage_name: &str) -> Result<()> {
        // Without a chat session the stage cannot hold a real conversation, so we
        // nudge the runner a bounded number of times to continue autonomously
        // (interactive stages finish via the `verdict` tool). The bound is the
        // ADC's `max_output_retries` (D-2 applied to nudge loops).
        let autonomous_nudges = self.config.adc().max_output_retries;
        let session_id = match self.record.connection().and_then(|c| c.session_id.clone()) {
            Some(id) => id,
            None => {
                tracing::error!(stage_id = %self.record.id(), "No session ID for multi-turn");
                self.record.set_status(StageStatus::Failed);
                self.record
                    .set_error(Some("No session ID available for multi-turn".into()));
                self.record
                    .set_completed_at(Some(chrono::Utc::now().to_rfc3339()));
                self.db.save_stage(&self.record)?;
                return Ok(());
            }
        };

        if self.chat_session.is_none() {
            tracing::info!(
                stage_id = %self.record.id(),
                stage = %stage_name,
                "No chat session configured — nudging stage to continue autonomously"
            );
            let mut last = None;
            for nudge in 0..autonomous_nudges {
                let reply = autonomous_nudge_text(nudge + 1, autonomous_nudges);
                let result = self.runner.send_followup(&session_id, &reply).await;
                self.refresh_active_record()?;
                match result {
                    Ok(r) => {
                        self.process_runner_result(r);
                        if *self.record.status() == StageStatus::Completed {
                            self.inject_worktree_output();
                            if let Err(error) = self.validate_completed_output() {
                                self.fail_invalid_output(&error);
                            }
                        }
                        self.db.save_stage(&self.record)?;
                        match self.record.status() {
                            StageStatus::WaitingForInput => {
                                last = Some(format!(
                                    "stage still waiting for input after {} autonomous nudges (no chat session configured)",
                                    autonomous_nudges
                                ))
                            }
                            _ => return Ok(()),
                        }
                    }
                    Err(e) => {
                        last = Some(format!("autonomous nudge failed: {}", e));
                        break;
                    }
                }
            }
            tracing::error!(stage_id = %self.record.id(), stage = %stage_name, "Stage did not reach completion within autonomous nudges");
            self.record.set_status(StageStatus::Failed);
            self.record.set_error(last);
            self.record
                .set_completed_at(Some(chrono::Utc::now().to_rfc3339()));
            self.db.save_stage(&self.record)?;
            return Ok(());
        }

        let Some(session) = self.chat_session.clone() else {
            unreachable!("chat session presence checked above");
        };

        tracing::debug!(
            stage_id = %self.record.id(),
            session_id = %session_id,
            "Entering multi-turn conversation loop"
        );

        const MAX_TURNS: u32 = 100;
        let mut turn = 0u32;

        loop {
            turn += 1;

            if turn > MAX_TURNS {
                tracing::error!(stage_id = %self.record.id(), max_turns = MAX_TURNS, "Multi-turn exceeded maximum turns");
                self.record.set_status(StageStatus::Failed);
                self.record.set_error(Some(format!(
                    "Multi-turn conversation exceeded maximum turns ({})",
                    MAX_TURNS
                )));
                self.record
                    .set_completed_at(Some(chrono::Utc::now().to_rfc3339()));
                self.db.save_stage(&self.record)?;
                return Ok(());
            }

            // Extract stage output message
            let stage_message = self
                .record
                .output_artifacts()
                .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
                .and_then(|v| {
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "[stage output]".to_string());

            // Send stage output to chat
            tracing::debug!(stage_id = %self.record.id(), turn, "Sending stage output to chat");
            self.append_stage_log("agent_message", &stage_message);
            session
                .send(&stage_message)
                .await
                .with_context(|| format!("Failed to send stage output to chat, turn {}", turn))?;

            // Wait for user reply
            tracing::debug!(stage_id = %self.record.id(), turn, "Waiting for user reply from chat");
            let user_reply = session.receive().await.with_context(|| {
                format!("Failed to receive user reply from chat, turn {}", turn)
            })?;
            self.append_stage_log("user_input", &user_reply);

            tracing::debug!(stage_id = %self.record.id(), turn, reply_len = user_reply.len(), "Received user reply, sending to runner");

            // Send reply to runner as follow-up
            let followup_result = self.runner.send_followup(&session_id, &user_reply).await;
            self.refresh_active_record()?;
            let followup_result = match followup_result {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(stage_id = %self.record.id(), turn, error = %e, "Followup failed");
                    self.record.set_status(StageStatus::Failed);
                    self.record
                        .set_error(Some(format!("Followup failed: {}", e)));
                    self.record
                        .set_completed_at(Some(chrono::Utc::now().to_rfc3339()));
                    self.db.save_stage(&self.record)?;
                    return Ok(());
                }
            };

            let now = chrono::Utc::now().to_rfc3339();

            match followup_result.status {
                RunnerStatus::Success => {
                    tracing::debug!(stage_id = %self.record.id(), turn, "Multi-turn completed with success");
                    self.record.set_status(StageStatus::Completed);
                    self.record.set_output_artifacts(
                        followup_result
                            .output
                            .map(|v| serde_json::to_string(&v).unwrap_or_default()),
                    );

                    // Read output file from worktree if available (multi-turn path doesn't read it via runner)
                    self.inject_worktree_output();
                    if let Err(error) = self.validate_completed_output() {
                        self.fail_invalid_output(&error);
                    }

                    self.record.set_completed_at(Some(now));
                    break;
                }
                RunnerStatus::WaitingForInput => {
                    tracing::debug!(stage_id = %self.record.id(), turn, "Runner still waiting for input");
                    self.record.set_output_artifacts(
                        followup_result
                            .output
                            .map(|v| serde_json::to_string(&v).unwrap_or_default()),
                    );
                }
                RunnerStatus::Failed { reason } => {
                    tracing::warn!(stage_id = %self.record.id(), turn, reason = %reason, "Followup failed");
                    self.record.set_status(StageStatus::Failed);
                    self.record.set_error(Some(reason));
                    self.record.set_completed_at(Some(now));
                    break;
                }
                RunnerStatus::Timeout => {
                    tracing::warn!(stage_id = %self.record.id(), turn, "Followup timed out");
                    self.record.set_status(StageStatus::TimedOut);
                    self.record
                        .set_error(Some("Stage timed out during multi-turn".into()));
                    self.record.set_completed_at(Some(now));
                    break;
                }
            }

            self.db.save_stage(&self.record)?;
        }

        self.db.save_stage(&self.record)?;
        Ok(())
    }

    /// Read the stage's output.md from the worktree (if present) and use it as
    /// the stage's "content" output. Used by paths that don't read it via the runner.
    /// Blocking question handling legitimately updates the row while the runner
    /// is awaited. Rebase only after that await, and only on an active record;
    /// the subsequent save still compares the loaded revision atomically.
    fn refresh_active_record(&mut self) -> Result<()> {
        let row = self
            .db
            .load_stage(self.record.id())?
            .ok_or_else(|| anyhow::anyhow!("Stage disappeared during execution"))?;
        anyhow::ensure!(
            matches!(
                row.status(),
                StageStatus::Running | StageStatus::WaitingForInput
            ),
            "Stage went terminal during execution"
        );
        self.record = row;
        Ok(())
    }

    fn inject_worktree_output(&mut self) {
        if let Some(worktree) = self.config.worktree_path() {
            let output_path = worktree
                .join(format!(
                    ".turbojet/{}/{}",
                    self.config.stage_name(),
                    self.config.run_id()
                ))
                .join("output.md");

            if output_path.exists() {
                match std::fs::read_to_string(&output_path) {
                    Ok(content) if !content.is_empty() => {
                        let mut output = serde_json::Map::new();
                        output.insert("content".into(), serde_json::Value::String(content));
                        self.record.set_output_artifacts(Some(
                            serde_json::to_string(&output).unwrap_or_default(),
                        ));
                        tracing::debug!(stage_id = %self.record.id(), path = %output_path.display(), "Read output file from worktree");
                    }
                    Ok(_) => {
                        tracing::debug!(stage_id = %self.record.id(), path = %output_path.display(), "Output file exists but is empty, keeping existing output");
                    }
                    Err(e) => {
                        tracing::warn!(path = %output_path.display(), error = %e, "Failed to read output file");
                    }
                }
            }
        }
    }

    fn process_runner_result(&mut self, result: StageResult) {
        let now = chrono::Utc::now().to_rfc3339();
        match result.status {
            RunnerStatus::Success => {
                self.record.set_status(StageStatus::Completed);
                self.record.set_output_artifacts(
                    result
                        .output
                        .map(|v| serde_json::to_string(&v).unwrap_or_default()),
                );
                self.record.set_completed_at(Some(now));
            }
            RunnerStatus::WaitingForInput => {
                self.record.set_status(StageStatus::WaitingForInput);
                self.record.set_output_artifacts(
                    result
                        .output
                        .map(|v| serde_json::to_string(&v).unwrap_or_default()),
                );
            }
            RunnerStatus::Failed { reason } => {
                self.record.set_status(StageStatus::Failed);
                self.record.set_error(Some(reason));
                self.record.set_completed_at(Some(now));
            }
            RunnerStatus::Timeout => {
                self.record.set_status(StageStatus::TimedOut);
                self.record.set_error(Some("Stage timed out".into()));
                self.record.set_completed_at(Some(now));
            }
        }
        self.record.set_connection(result.connection);
        if *self.record.status() == StageStatus::Completed
            && let Err(error) = self.validate_completed_output()
        {
            self.fail_invalid_output(&error);
        }
    }

    /// Validate required input artifacts that are explicitly sourced from an
    /// earlier stage. Missing/empty sources fail before the model is called.
    fn validate_required_input_artifacts(&self) -> Result<()> {
        let completed = self
            .config
            .input_artifacts()
            .get("completed_artifacts")
            .and_then(|value| value.as_object());
        for artifact in &self.config.adc().input.artifacts {
            if !artifact.required {
                continue;
            }
            let Some(source) = artifact.from.as_deref() else {
                continue;
            };
            let value = completed.and_then(|map| map.get(source)).ok_or_else(|| anyhow::anyhow!(
                "required input artifact '{}' is missing: source stage '{}' produced no artifact for stage '{}'",
                artifact.name, source, self.config.stage_name()
            ))?;
            anyhow::ensure!(
                !json_is_empty(value),
                "required input artifact '{}' from stage '{}' is empty (stage '{}')",
                artifact.name,
                source,
                self.config.stage_name()
            );
        }
        Ok(())
    }

    /// Local completion gate for every stage, both interactive and
    /// non-interactive: the structured output must parse, satisfy the frozen
    /// schema when one exists, and publish every required output artifact.
    fn validate_completed_output(&self) -> Result<()> {
        let raw = self.record.output_artifacts().ok_or_else(|| {
            anyhow::anyhow!(
                "output contract violation: stage '{}' completed without structured output",
                self.config.stage_name()
            )
        })?;
        let output: serde_json::Value = serde_json::from_str(raw).with_context(|| {
            format!(
                "output contract violation: stage '{}' produced unparseable output JSON",
                self.config.stage_name()
            )
        })?;
        if let Some(schema) = self.config.response_schema() {
            tj_config::loader::validate_json_value(&output, schema, "").with_context(|| {
                format!(
                    "output contract violation: stage '{}' schema violation",
                    self.config.stage_name()
                )
            })?;
        }
        self.verify_required_output_artifacts(&output)
    }

    fn verify_required_output_artifacts(&self, output: &serde_json::Value) -> Result<()> {
        let mirrored = output
            .get("content")
            .and_then(|v| v.as_str())
            .is_some_and(|v| !v.trim().is_empty())
            || output
                .get("summary")
                .and_then(|v| v.as_str())
                .is_some_and(|v| !v.trim().is_empty());
        let roots = self.artifact_roots();
        for artifact in &self.config.adc().output.artifacts {
            verify_output_artifact(
                artifact,
                &roots,
                self.config.interactive(),
                mirrored,
                self.config.stage_name(),
            )?;
        }
        Ok(())
    }

    fn artifact_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(worktree) = self.config.worktree_path() {
            roots.push(
                worktree
                    .join(".turbojet")
                    .join(self.config.stage_name())
                    .join(self.config.run_id()),
            );
            roots.push(worktree.to_path_buf());
        }
        let task_output = std::env::var("TURBOJET_HOME")
            .map(|home| PathBuf::from(home).join("runs").join(self.record.id()))
            .unwrap_or_else(|_| PathBuf::from("/tmp/turbojet/runs").join(self.record.id()));
        roots.push(task_output);
        roots
    }

    fn fail_invalid_output(&mut self, error: &anyhow::Error) {
        let message = format!("{error:#}");
        tracing::warn!(stage_id = %self.record.id(), error = %message, "Stage output contract failed");
        self.record.set_status(StageStatus::Failed);
        self.record.set_error(Some(message));
        self.record
            .set_completed_at(Some(chrono::Utc::now().to_rfc3339()));
    }

    fn send_terminal_notification(&self, stage_name: &str) {
        if let Some(ref session) = self.chat_session {
            let display_name = title_case(stage_name);
            let stage_name_owned = stage_name.to_string();
            match self.record.status() {
                StageStatus::Completed => {
                    let msg = format!("**{}** stage completed", display_name);
                    let session = session.clone();
                    tokio::spawn(async move {
                        match session.send(&msg).await {
                            Ok(()) => {
                                tracing::debug!(stage = %stage_name_owned, "Stage completed notification sent")
                            }
                            Err(e) => {
                                tracing::warn!(stage = %stage_name_owned, error = %e, "Failed to send stage completed notification")
                            }
                        }
                    });
                }
                StageStatus::Failed | StageStatus::TimedOut => {
                    let status_word = match self.record.status() {
                        StageStatus::TimedOut => "timed out",
                        _ => "failed",
                    };
                    let detail = self.record.error().unwrap_or("unknown error");
                    let msg = format!("**{}** stage {}: {}", display_name, status_word, detail);
                    let session = session.clone();
                    tokio::spawn(async move {
                        match session.send(&msg).await {
                            Ok(()) => {
                                tracing::debug!(stage = %stage_name_owned, "Stage failure notification sent")
                            }
                            Err(e) => {
                                tracing::warn!(stage = %stage_name_owned, error = %e, "Failed to send stage failure notification")
                            }
                        }
                    });
                }
                StageStatus::WaitingForInput | StageStatus::Running | StageStatus::Pending => {}
            }
        }
    }
}

/// Text of one autonomous nudge for interactive stages running with no chat
/// session. Migrated to the tool contract (plan T-5): the stage finishes via
/// the `verdict` tool — the `::TURBOJET:DONE::` marker is retired (FR-9) and
/// must never be referenced.
fn autonomous_nudge_text(nudge: u32, bound: u32) -> String {
    format!(
        "You are running unattended: no user is available to answer questions. \
         Decide using the information you have, state your assumption in one line, and \
         finish the task now (finish by calling the `verdict` tool). Nudge {}/{}.",
        nudge, bound
    )
}

/// Durable run-wide budget backed by SQLite. Denials surface as ordinary
/// stage failures with an explicit `budget exhausted:` reason; no fallback
/// path can continue uncharged work.
struct StageBudgetGuard {
    db: Arc<Database>,
    run_id: String,
}

impl BudgetGuard for StageBudgetGuard {
    fn charge(&self, kind: &str, scope: &str, operation_key: &str) -> Result<()> {
        self.db
            .charge_budget(&self.run_id, kind, Some(scope), operation_key)?;
        Ok(())
    }
}

/// Persists every bounded provider transport attempt. Retries are visible with
/// their class and attempt number; no tool call is part of this boundary.
struct DbProviderObserver {
    db: Arc<Database>,
    run_id: String,
    stage_id: String,
    stage_name: String,
    provider_id: String,
}

impl ProviderObserver for DbProviderObserver {
    fn record(&self, attempt: &ProviderAttemptRecord) {
        if let Err(error) = self.db.record_provider_attempt(
            &self.run_id,
            &self.stage_id,
            Some(&self.stage_name),
            &self.provider_id,
            &attempt.model,
            &attempt.outcome,
            attempt.error_class.as_deref(),
            attempt.http_status,
            attempt.retry_index,
            attempt.duration_ms,
        ) {
            tracing::warn!(run_id = %self.run_id, %error, "Failed to persist provider attempt");
        }
    }
}

/// Persists token/cost accounting for one successful model call. Missing usage
/// remains unknown; configured pricing is never invented.
struct DbUsageSink {
    db: Arc<Database>,
    run_id: String,
    stage_id: String,
    model: String,
    input_cost_micros_per_million: u64,
    output_cost_micros_per_million: u64,
}

impl UsageSink for DbUsageSink {
    fn record(&self, stage_id: &str, operation_key: &str, usage: &LlmUsage) {
        let cost_micros = match (usage.reported, usage.input, usage.output) {
            (true, Some(input), Some(output))
                if self.input_cost_micros_per_million > 0
                    || self.output_cost_micros_per_million > 0 =>
            {
                Some(
                    input
                        .saturating_mul(self.input_cost_micros_per_million)
                        .saturating_add(output.saturating_mul(self.output_cost_micros_per_million))
                        / 1_000_000,
                )
            }
            _ => None,
        };
        let record = ProviderUsage {
            run_id: self.run_id.clone(),
            operation_key: operation_key.to_string(),
            stage_id: if stage_id.is_empty() {
                self.stage_id.clone()
            } else {
                stage_id.to_string()
            },
            model: self.model.clone(),
            input_tokens: usage.input,
            output_tokens: usage.output,
            cost_micros,
            usage_known: usage.reported,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(error) = self.db.record_provider_usage(&record) {
            tracing::warn!(run_id = %self.run_id, %error, "Failed to persist provider usage");
        }
    }
}

/// Persists stage activity (LLM traffic, tool calls) for replay and live streaming.
struct DbStageLogSink {
    db: Arc<Database>,
    run_id: String,
    flow_name: String,
}

impl tj_runner::StageLogSink for DbStageLogSink {
    fn log(&self, stage_id: &str, stage_name: &str, kind: &str, content: &str) {
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
            tracing::warn!(run_id = %self.run_id, stage_id = %stage_id, error = %e, "Failed to append stage log");
        }
    }
}

fn verify_output_artifact(
    artifact: &tj_config::adc::ArtifactDef,
    roots: &[PathBuf],
    interactive: bool,
    mirrored: bool,
    stage_name: &str,
) -> Result<()> {
    if !artifact.required {
        return Ok(());
    }
    validate_artifact_name(&artifact.name)?;
    let found = roots
        .iter()
        .map(|root| root.join(&artifact.name))
        .find(|path| path.is_file());
    match found {
        Some(path) => {
            let metadata = std::fs::metadata(&path)?;
            if artifact.verify.as_deref() == Some("non-empty") {
                anyhow::ensure!(
                    metadata.len() > 0,
                    "output contract violation: required output artifact '{}' is empty at {}",
                    artifact.name,
                    path.display()
                );
            }
            Ok(())
        }
        None if !interactive && mirrored => {
            // Non-interactive stages publish their validated structured
            // artifact (there is no stage file-writing turn). The conventional
            // output.md declaration is satisfied by the non-empty
            // content/summary mirror; explicit TJ-013 semantics.
            Ok(())
        }
        None => anyhow::bail!(
            "output contract violation: required output artifact '{}' is missing for stage '{}'",
            artifact.name,
            stage_name
        ),
    }
}

fn json_is_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(text) => text.trim().is_empty(),
        serde_json::Value::Array(items) => items.is_empty(),
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn validate_artifact_name(name: &str) -> Result<()> {
    anyhow::ensure!(!name.trim().is_empty(), "artifact name must not be empty");
    let path = std::path::Path::new(name);
    anyhow::ensure!(
        !path.is_absolute(),
        "artifact '{}' must be a relative path",
        name
    );
    anyhow::ensure!(
        !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir)),
        "artifact '{}' must not escape the output directory",
        name
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autonomous_nudge_text_points_at_verdict_tool_not_marker() {
        let text = autonomous_nudge_text(2, 3);
        assert!(
            text.contains("`verdict` tool"),
            "nudge must teach the verdict tool: {}",
            text
        );
        assert!(
            text.contains("2/3"),
            "nudge must name the turn and bound: {}",
            text
        );
        assert!(
            !text.contains("::TURBOJET:DONE::") && !text.contains("completion marker"),
            "the retired marker must not be referenced (FR-9): {}",
            text
        );
    }

    fn artifact(name: &str) -> tj_config::adc::ArtifactDef {
        tj_config::adc::ArtifactDef {
            name: name.into(),
            from: None,
            required: true,
            verify: Some("non-empty".into()),
        }
    }

    #[test]
    fn required_interactive_artifact_must_exist_and_be_non_empty() {
        let dir = std::env::temp_dir().join(format!("tj-stage-artifact-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let roots = vec![dir.clone()];
        let error = verify_output_artifact(&artifact("output.md"), &roots, true, true, "intake")
            .unwrap_err()
            .to_string();
        assert!(error.contains("is missing"), "{error}");

        std::fs::write(dir.join("output.md"), "").unwrap();
        let error = verify_output_artifact(&artifact("output.md"), &roots, true, true, "intake")
            .unwrap_err()
            .to_string();
        assert!(error.contains("is empty"), "{error}");

        std::fs::write(dir.join("output.md"), "report").unwrap();
        verify_output_artifact(&artifact("output.md"), &roots, true, false, "intake").unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn non_interactive_structured_mirror_satisfies_output_declaration() {
        let dir = std::env::temp_dir().join(format!("tj-stage-artifact-{}", uuid::Uuid::new_v4()));
        let roots = vec![dir];
        verify_output_artifact(&artifact("output.md"), &roots, false, true, "plan").unwrap();
        let error = verify_output_artifact(&artifact("output.md"), &roots, false, false, "plan")
            .unwrap_err()
            .to_string();
        assert!(error.contains("is missing"), "{error}");
    }

    #[test]
    fn artifact_paths_cannot_escape_the_output_directory() {
        assert!(validate_artifact_name("../secret").is_err());
        assert!(validate_artifact_name("/etc/passwd").is_err());
        assert!(validate_artifact_name("nested/output.md").is_ok());
    }
}
