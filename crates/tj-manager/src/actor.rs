use anyhow::{Context as _, Result};
use dashmap::DashMap;
use futures::FutureExt;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use tj_agent::{Agent, Chief, ChiefConfig, SystemOperations};
use tj_chat::{ChatProvider, ConversationContext};
use tj_chat_zulip::{ZulipChat, ZulipConfig};
use tj_config::bundle::ExecutionBundle;
use tj_config::format::title_case;
use tj_config::loader::ConfigLoader;
use tj_config::smg::{FlowType, StateMachineGraph};
use tj_db::{Database, FlowStatus, PipelineNotification, RunStatus};
use tj_flow::{Flow, FlowConfig, FlowOutcome};
use tj_git::GitProvider;
use tj_git::slug::derive_slug;
use tj_git::worktree::{WorktreeInfo, WorktreeManager};
use tj_secrets::SecretsStore;
use tokio::sync::{broadcast, mpsc};

use crate::capabilities::CapabilityRuntime;
use crate::contract::RunContract;
use crate::db_secrets::DbSecretsStore;
use crate::legacy_v1::{self, LegacyV1Policy};

use crate::events::ManagerEvent;
use crate::notifier::RunNotifier;
use crate::ssh_guard::SshKeyGuard;

/// Authoritative receipt for POST /input. A question-bound answer returns the
/// durable acceptance; legacy plain-text replies return the in-memory sequence.
#[derive(Debug, Clone, Serialize)]
pub struct WebInputReceipt {
    pub seq: Option<u64>,
    pub question_id: Option<String>,
    pub status: String,
    pub replayed: bool,
    pub answer_source: Option<String>,
}

struct SpawnFlowParams {
    run_id: String,
    flow_name: String,
    description: String,
    existing_artifacts: HashMap<String, serde_json::Value>,
    workspace_path: Option<std::path::PathBuf>,
    branch_name: Option<String>,
    slug: String,
    /// Artifact attempt lineage offset (rework re-entry increments it).
    attempt_offset: u32,
}

pub struct ManagerHandle {
    db: Arc<Database>,
    tx: mpsc::UnboundedSender<ManagerEvent>,
    pub pipeline_events: broadcast::Sender<PipelineNotification>,
    pub agent: Arc<tj_agent::Agent>,
    pub agent_sessions: Arc<DashMap<String, tj_agent::AgentSession>>,
    web_bridges: Arc<DashMap<String, Arc<tj_chat::WebInputBridge>>>,
    workspace_base: String,
    config_loader: Arc<ConfigLoader>,
    accepting_runs: Arc<std::sync::Mutex<bool>>,
    run_timeout_secs: Option<u64>,
    operation_token: Option<String>,
    secrets_store: Option<Arc<dyn SecretsStore>>,
}

impl Clone for ManagerHandle {
    fn clone(&self) -> Self {
        Self {
            db: Arc::clone(&self.db),
            tx: self.tx.clone(),
            pipeline_events: self.pipeline_events.clone(),
            agent: self.agent.clone(),
            agent_sessions: self.agent_sessions.clone(),
            web_bridges: Arc::clone(&self.web_bridges),
            workspace_base: self.workspace_base.clone(),
            config_loader: Arc::clone(&self.config_loader),
            accepting_runs: self.accepting_runs.clone(),
            run_timeout_secs: self.run_timeout_secs,
            operation_token: self.operation_token.clone(),
            secrets_store: self.secrets_store.clone(),
        }
    }
}

impl ManagerHandle {
    /// User tools revalidate the credential for every call, including calls
    /// after a long model turn. Internal runtime methods remain separate.
    pub fn operations_for_token(&self, token: String) -> Self {
        let mut scoped = self.clone();
        scoped.operation_token = Some(token);
        scoped
    }

    fn operation_user(&self) -> Result<tj_db::User> {
        let token = self
            .operation_token
            .as_deref()
            .context("Authenticated user context required")?;
        self.db
            .user_for_token(token)?
            .context("Session expired or revoked")
    }

    fn authorize_job_operation(&self, run_id: &str) -> Result<tj_db::User> {
        let user = self.operation_user()?;
        anyhow::ensure!(
            self.db.can_access_run(&user.id, run_id)?,
            "Project access denied"
        );
        Ok(user)
    }
    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    pub fn sender(&self) -> &mpsc::UnboundedSender<ManagerEvent> {
        &self.tx
    }

    pub fn workspace_base(&self) -> &str {
        &self.workspace_base
    }

    pub fn config_loader(&self) -> &Arc<ConfigLoader> {
        &self.config_loader
    }

    pub fn create_run(
        &self,
        description: &str,
        project_id: Option<&str>,
        lane_id: Option<&str>,
        repo_url: Option<&str>,
        base_branch: Option<&str>,
        actor: Option<&str>,
    ) -> Result<tj_db::PipelineRun> {
        // Runs always target a workflow: a project is required, and the lane
        // defaults to the project's "Default" lane (or its only lane).
        let project_id = project_id.ok_or_else(|| {
            anyhow::anyhow!(
                "a project is required — runs always execute on a workflow (project + lane)"
            )
        })?;
        let lane_id = match lane_id {
            Some(l) => l.to_string(),
            None => {
                let lanes = self.db.list_lanes_for_project(project_id)?;
                let default = lanes
                    .iter()
                    .find(|l| l.name == "Default")
                    .or_else(|| lanes.first());
                default
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "project '{}' has no workflows — create a lane first",
                            project_id
                        )
                    })?
                    .id
                    .to_string()
            }
        };
        self.validate_run_target(project_id, &lane_id)
            .map_err(anyhow::Error::msg)?;
        self.validated_graph_for_lane(&lane_id)?;
        let trace_id = uuid::Uuid::new_v4().to_string();
        self.create_run_with_trace(
            description,
            Some(project_id),
            Some(&lane_id),
            repo_url,
            base_branch,
            &trace_id,
            actor,
        )
    }

    pub fn create_run_with_trace(
        &self,
        description: &str,
        project_id: Option<&str>,
        lane_id: Option<&str>,
        repo_url: Option<&str>,
        base_branch: Option<&str>,
        trace_id: &str,
        actor: Option<&str>,
    ) -> Result<tj_db::PipelineRun> {
        // Hold admission through persistence and enqueue so shutdown cannot
        // overtake a newly accepted run between those two operations.
        let accepting = self
            .accepting_runs
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        anyhow::ensure!(*accepting, "Manager is shutting down");
        let project_id = project_id.context("a project is required")?;
        let lane_id = lane_id.context("a workflow lane is required")?;
        self.validate_run_target(project_id, lane_id)
            .map_err(anyhow::Error::msg)?;
        let project = self
            .db
            .load_project(project_id)?
            .with_context(|| format!("project '{}' does not exist", project_id))?;
        if project_workspace_kind(&project) == "none" {
            anyhow::ensure!(
                repo_url.is_none(),
                "workflow '{}' declares no repository workspace; repository URLs are refused, not silently ignored",
                project.profile
            );
        }
        let (graph, workflow_assets) = self.validated_graph_for_lane(lane_id)?;
        let resolved_repo = repo_url.map(|s| s.into()).or_else(|| {
            self.db
                .load_project(project_id)
                .ok()
                .flatten()
                .and_then(|p| p.repo_url)
        });
        let resolved_branch = base_branch
            .map(|s| s.into())
            .or_else(|| {
                self.db
                    .load_project(project_id)
                    .ok()
                    .flatten()
                    .map(|p| p.base_branch)
            })
            .unwrap_or_else(|| "main".into());
        let run_id = format!("run_{}", uuid::Uuid::new_v4());
        // TJ-012: freeze every referenced file, provider/model and limit before
        // the run is persisted. A missing/invalid reference fails admission.
        let workflow_id = self
            .db
            .load_lane(lane_id)?
            .and_then(|lane| lane.workflow_id);
        let bundle = match workflow_assets.as_ref() {
            Some(assets) => ExecutionBundle::build_from_assets(
                &self.config_loader,
                &run_id,
                workflow_id.as_deref(),
                &graph,
                assets,
            )?,
            None => ExecutionBundle::build(
                &self.config_loader,
                &run_id,
                workflow_id.as_deref(),
                &graph,
            )?,
        };
        let contract = RunContract::from_bundle(&bundle)?;
        CapabilityRuntime::ensure_admissible(&contract)?;
        let bundle_json = serde_json::to_string(&bundle).context("serialize execution bundle")?;
        let bundle_record = tj_db::ExecutionBundleRecord {
            id: format!("bundle_{}", uuid::Uuid::new_v4()),
            run_id: tj_db::RunId(run_id.clone()),
            workflow_id: workflow_id.clone(),
            bundle_hash: bundle.bundle_hash.clone(),
            bundle_json,
            legacy: false,
            created_at: bundle.created_at.clone(),
        };
        let bundle_record = self.db.save_execution_bundle(&bundle_record)?;
        // TJ-016: limits are frozen with the bundle and counters live for the
        // whole run (attempts/rework/retries never reset them).
        self.db.init_run_budget(
            &run_id,
            &tj_db::RunBudgetLimits {
                question_rounds_per_stage: bundle.budgets.question_rounds_per_stage,
                question_rounds_per_run: bundle.budgets.question_rounds_per_run,
                model_calls: bundle.budgets.model_calls,
                tool_calls: bundle.budgets.tool_calls,
                output_corrections: bundle.budgets.output_corrections,
                interaction_nudges: bundle.budgets.interaction_nudges,
                active_ms: bundle.budgets.active_secs.saturating_mul(1000),
                cost_micros: bundle.budgets.cost_micros,
            },
        )?;
        let mut run = tj_db::PipelineRun::new(
            run_id,
            Some(tj_db::ProjectId::from(project_id)),
            Some(tj_db::LaneId::from(lane_id)),
            description.into(),
            RunStatus::Running,
            resolved_repo,
            Some(resolved_branch),
            Some(trace_id.to_string()),
            None,
        );
        if let Some(seconds) = self.run_timeout_secs {
            let duration = i64::try_from(seconds)
                .ok()
                .and_then(chrono::Duration::try_seconds)
                .context("run_timeout_secs is too large")?;
            let deadline = chrono::Utc::now()
                .checked_add_signed(duration)
                .context("Run deadline overflows timestamp")?;
            run.set_deadline_at(Some(deadline.to_rfc3339()));
        }
        run.set_workflow_id(workflow_id);
        run.set_execution_bundle(
            Some(bundle_record.id.clone()),
            Some(bundle_record.bundle_hash.clone()),
        );
        if let Err(error) = self.db.save_run(&run) {
            let _ = self.db.delete_execution_bundle_for_run(run.id());
            return Err(error);
        }
        self.emit_event(
            run.id(),
            "run_started",
            actor,
            None,
            None,
            Some(description),
        );
        tracing::debug!(trace_id = %trace_id, run_id = %run.id(), "Run created with trace_id");
        let _ = self.tx.send(ManagerEvent::RunRequested {
            run_id: run.id().to_string(),
            trace_id: trace_id.to_string(),
        });
        Ok(run)
    }

    /// Runs must target a real workflow: the project and lane must exist and the
    /// lane must belong to the project. Returns Err(message) when invalid.
    pub fn validate_run_target(&self, project_id: &str, lane_id: &str) -> Result<(), String> {
        let project = self
            .db
            .load_project(project_id)
            .ok()
            .flatten()
            .ok_or_else(|| format!("project '{}' does not exist", project_id))?;
        let _ = project;
        let lane = self
            .db
            .load_lane(lane_id)
            .ok()
            .flatten()
            .ok_or_else(|| format!("workflow '{}' does not exist", lane_id))?;
        if lane.project_id.as_str() != project_id {
            return Err(format!(
                "workflow '{}' does not belong to project '{}'",
                lane_id, project_id
            ));
        }
        Ok(())
    }

    /// Resolve exactly one immutable registry workflow for a lane. New
    /// admission never reads a mutable `smg_path` and never falls back to a
    /// process-wide graph.
    fn pinned_workflow_for_lane(&self, lane_id: &str) -> Result<tj_db::WorkflowVersion> {
        let lane = self
            .db
            .load_lane(lane_id)?
            .context("workflow lane does not exist")?;
        let workflow_id = lane.workflow_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "lane '{}' has no pinned workflow; import a workflow and pin the lane with PUT /v1/lanes/{}/workflow before starting a run",
                lane_id,
                lane_id
            )
        })?;
        let workflow = self.db.get_workflow(workflow_id)?.with_context(|| {
            format!(
                "lane '{}' pins missing workflow '{}'; re-import the source and pin an existing version",
                lane_id, workflow_id
            )
        })?;
        anyhow::ensure!(
            workflow.capability_schema_version > 0,
            "workflow '{}' ({}) predates explicit catalog/capability declarations; re-import it with a catalog block and pin the new version",
            workflow.name,
            workflow_id
        );
        Ok(workflow)
    }

    fn validated_graph_for_lane(
        &self,
        lane_id: &str,
    ) -> Result<(StateMachineGraph, Option<tj_config::WorkflowAssets>)> {
        let workflow = self.pinned_workflow_for_lane(lane_id)?;
        let (smg, _) = validated_workflow_contract(&workflow)?;
        let assets = self.registry_assets_for_workflow(&workflow)?;
        if let Some(assets) = assets.as_ref() {
            self.config_loader
                .validate_smg_with_assets(&smg, assets)
                .with_context(|| format!("lane '{}' workflow contract is invalid", lane_id))?;
        } else {
            self.config_loader
                .validate_smg(&smg)
                .with_context(|| format!("lane '{}' workflow contract is invalid", lane_id))?;
        }
        Ok((smg, assets))
    }

    /// Package workflows are executed from immutable registry assets only.
    /// Legacy file/directory workflows keep their existing disk resolution.
    fn registry_assets_for_workflow(
        &self,
        workflow: &tj_db::WorkflowVersion,
    ) -> Result<Option<tj_config::WorkflowAssets>> {
        if workflow.source_kind.as_deref() != Some(tj_config::WorkflowSourceKind::Package.as_str())
        {
            return Ok(None);
        }
        let stored = self.db.workflow_assets(&workflow.id).with_context(|| {
            format!(
                "workflow '{}' ({}) is a package but its assets could not be read",
                workflow.name, workflow.id
            )
        })?;
        anyhow::ensure!(
            !stored.is_empty(),
            "workflow '{}' ({}) is a package but has no stored assets; re-import the package source",
            workflow.name,
            workflow.id
        );
        let assets = tj_config::WorkflowAssets::from_entries(
            stored
                .into_iter()
                .map(|asset| (asset.logical_path, asset.content)),
        )
        .with_context(|| {
            format!(
                "workflow '{}' ({}) has invalid stored package assets; re-import the package source",
                workflow.name, workflow.id
            )
        })?;
        Ok(Some(assets))
    }

    pub fn list_runs(&self) -> Result<Vec<tj_db::PipelineRun>> {
        self.db.list_runs()
    }

    pub fn get_run(&self, id: &str) -> Result<Option<tj_db::PipelineRun>> {
        self.db.load_run(id)
    }

    pub fn budget(&self, run_id: &str) -> Result<Option<tj_db::BudgetSnapshot>> {
        self.db.budget_snapshot(run_id)
    }

    pub fn recovery(&self, run_id: &str) -> Result<Option<tj_db::RecoveryState>> {
        self.db.recovery_state(run_id)
    }

    pub fn recovery_effects(&self, run_id: &str) -> Result<Vec<tj_db::PendingEffect>> {
        self.db.list_pending_effects(run_id, true)
    }

    pub fn publications(
        &self,
        run_id: &str,
    ) -> Result<Vec<tj_db::publications::RemotePublication>> {
        self.db.publications_for_run(run_id)
    }

    /// Read-only delivery provenance bundle shared by the API, browser and Chief.
    /// No external effect is possible from this method.
    pub fn delivery_bundle(&self, run_id: &str) -> Result<serde_json::Value> {
        let releases = self.db.delivery_releases_for_run(run_id)?;
        let decisions = self.db.delivery_authorizations_for_run(run_id)?;
        let events = self.db.delivery_events_for_run(run_id)?;
        Ok(serde_json::json!({
            "run_id": run_id,
            "releases": releases,
            "decisions": decisions,
            "callbacks": events,
            "execution_authority": "external_ci_cd",
            "note": "TurboJet dispatches and records external CI/CD evidence; it never deploys, merges or releases on its own.",
        }))
    }

    /// Explicit safe retry: durable answers/artifacts stay; a new attempt runs
    /// the interrupted (or last completed) flow from its entry. No automatic
    /// replay happens anywhere else.
    pub async fn retry_run(
        &self,
        run_id: &str,
        acknowledge_unknown_effects: bool,
    ) -> Result<tj_db::RetryPlan> {
        {
            let accepting = self
                .accepting_runs
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            anyhow::ensure!(*accepting, "Manager is shutting down");
        }
        let plan = self
            .db
            .retry_interrupted_run(run_id, acknowledge_unknown_effects)?;
        let trace_id = self
            .db
            .load_run(run_id)
            .ok()
            .flatten()
            .and_then(|run| run.trace_id().map(str::to_string))
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.tx
            .send(ManagerEvent::RetryRequested {
                run_id: run_id.to_string(),
                flow_name: plan.flow_name.clone(),
                attempt_offset: plan.attempt_offset,
                trace_id,
            })
            .map_err(|_| anyhow::anyhow!("Manager is unavailable"))?;
        Ok(plan)
    }

    /// Called by the API after a durable task selection; the Manager revalidates
    /// identity and only then releases a waiting task run.
    pub fn notify_task_selected(&self, run_id: &str, selection_id: &str) -> Result<()> {
        self.tx
            .send(ManagerEvent::TaskSelected {
                run_id: run_id.to_string(),
                selection_id: selection_id.to_string(),
            })
            .map_err(|_| anyhow::anyhow!("Manager is unavailable"))
    }

    pub async fn cancel_run(&self, id: &str) -> Result<()> {
        let (reply, received) = tokio::sync::oneshot::channel();
        self.tx
            .send(ManagerEvent::CancelRun {
                run_id: id.into(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Manager is unavailable"))?;
        received
            .await
            .context("Manager stopped before cancellation acknowledgment")?
            .map_err(anyhow::Error::msg)
    }

    pub async fn shutdown(&self) -> Result<()> {
        let (reply, received) = tokio::sync::oneshot::channel();
        {
            let mut accepting = self
                .accepting_runs
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            *accepting = false;
            self.tx
                .send(ManagerEvent::Shutdown { reply })
                .map_err(|_| anyhow::anyhow!("Manager is unavailable"))?;
        }
        received
            .await
            .context("Manager stopped before shutdown acknowledgment")?
            .map_err(anyhow::Error::msg)
    }

    pub async fn submit_gate_decision(
        &self,
        decision: tj_db::GateDecision,
        expected_state_version: i64,
    ) -> Result<tj_db::GateDecisionReceipt> {
        let (reply, received) = tokio::sync::oneshot::channel();
        self.tx
            .send(ManagerEvent::GateDecision {
                decision,
                expected_state_version,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Manager is unavailable"))?;
        received
            .await
            .context("Manager stopped before gate-decision acknowledgment")?
            .map_err(anyhow::Error::msg)
    }

    pub fn send_stage_message(&self, run_id: &str, _flow_name: &str, message: &str) {
        tracing::debug!(run_id = %run_id, "send_stage_message received (no-op until chat wiring is restored)");
        let _ = message;
    }

    /// Resolve a project's workflow from the registry. Resolution order is
    /// exact `workflow_id` → `catalog_id` (highest revision) → legacy profile
    /// alias when it names a registered catalog → explicit catalog default.
    /// No filename, graph name or order is ever used.
    pub fn resolve_project_workflow(
        &self,
        workflow_id: Option<&str>,
        catalog_id: Option<&str>,
        legacy_profile: Option<&str>,
    ) -> Result<tj_db::WorkflowVersion> {
        let catalog = self.db.list_catalog()?;
        if catalog.is_empty() && self.db.list_workflows()?.is_empty() {
            anyhow::bail!(
                "No workflow installed; configure a workflow source or import a workflow before creating a project."
            );
        }
        let selected = if let Some(workflow_id) = workflow_id.filter(|id| !id.trim().is_empty()) {
            self.db.get_workflow(workflow_id)?.with_context(|| {
                format!(
                    "workflow '{}' does not exist in the registry; import it or choose a registered workflow",
                    workflow_id
                )
            })?
        } else if let Some(catalog_id) = catalog_id.filter(|id| !id.trim().is_empty()) {
            self.db
                .latest_workflow_for_catalog(catalog_id)?
                .with_context(|| {
                    format!(
                        "catalog '{}' is not registered; import a workflow with that catalog.id first",
                        catalog_id
                    )
                })?
        } else if let Some(alias) = legacy_profile.filter(|id| !id.trim().is_empty()) {
            self.db
                .latest_workflow_for_catalog(alias)?
                .with_context(|| {
                    format!(
                        "legacy profile '{}' is not a registered catalog id; choose a workflow_id or catalog_id instead",
                        alias
                    )
                })?
        } else {
            let defaults: Vec<_> = catalog.iter().filter(|wf| wf.catalog_default).collect();
            match defaults.as_slice() {
                [default] => (*default).clone(),
                [] => anyhow::bail!(
                    "choose a workflow: provide workflow_id or catalog_id (no catalog.default is registered)"
                ),
                _ => anyhow::bail!(
                    "registry has multiple catalog defaults; mark exactly one catalog.default or choose a workflow_id"
                ),
            }
        };
        anyhow::ensure!(
            selected.capability_schema_version > 0,
            "workflow '{}' ({}) predates explicit catalog/capability declarations; re-import it with a catalog block and select the new version",
            selected.name,
            selected.id
        );
        validated_workflow_contract(&selected)?;
        Ok(selected)
    }

    pub fn create_project_with_workflow(
        &self,
        name: &str,
        repo_url: Option<&str>,
        base_branch: Option<&str>,
        git_provider_json: Option<&str>,
        workflow: &tj_db::WorkflowVersion,
    ) -> Result<tj_db::Project> {
        let (_, contract) = validated_workflow_contract(workflow)?;
        let workspace_kind = match contract.workspace_mode() {
            tj_config::capability::WorkspaceMode::None => "none",
            tj_config::capability::WorkspaceMode::Repository => "repository",
        };
        if workspace_kind == "repository" {
            let provisioning = contract.workspace.provisioning.expect("validated");
            anyhow::ensure!(
                repo_url.is_some()
                    || provisioning == tj_config::capability::RepositoryProvisioning::Optional,
                "workflow '{}' declares required repository provisioning but no repository URL was supplied",
                workflow.name
            );
        }
        if workspace_kind == "none" {
            anyhow::ensure!(
                repo_url.is_none(),
                "workflow '{}' declares no repository workspace; remove repo_url or import a workflow that explicitly declares a repository capability",
                workflow.name
            );
            anyhow::ensure!(
                git_provider_json.is_none(),
                "workflow '{}' declares no repository workspace; git_provider is not applicable",
                workflow.name
            );
        }
        // Validate every caller-supplied value before creating a workspace so a
        // rejected project cannot leave an orphan directory or local Git repo.
        let git_provider: Option<String> = match git_provider_json {
            Some(json) => {
                let _: tj_git::GitProviderConfig = serde_json::from_str(json).with_context(
                    || "Invalid git_provider_json: failed to parse as GitProviderConfig",
                )?;
                Some(json.to_string())
            }
            None => None,
        };
        let project_id = format!("proj_{}", uuid::Uuid::new_v4());
        let workspace_path = format!(
            "{}/{}",
            self.workspace_base.trim_end_matches('/'),
            project_id
        );
        std::fs::create_dir_all(&workspace_path)?;
        let locally_initialized_repository = if workspace_kind == "repository" && repo_url.is_none()
        {
            if let Err(error) = tj_git::worktree::init_local_repository(Path::new(&workspace_path))
            {
                let _ = std::fs::remove_dir_all(&workspace_path);
                return Err(error).with_context(|| {
                    format!(
                        "workflow '{}' declares a repository workspace, but local Git initialization failed",
                        workflow.name
                    )
                });
            }
            true
        } else {
            false
        };

        let now = chrono::Utc::now().to_rfc3339();
        let project = tj_db::Project {
            id: project_id.clone(),
            name: name.into(),
            repo_url: repo_url.map(|s| s.into()),
            base_branch: base_branch.unwrap_or("main").into(),
            workspace_path: Some(workspace_path.clone()),
            git_provider,
            profile: workflow
                .catalog_id
                .clone()
                .unwrap_or_else(|| workflow.name.clone()),
            workspace_kind: Some(workspace_kind.into()),
            catalog_id: workflow.catalog_id.clone(),
            provisioning_state: Some(
                if locally_initialized_repository {
                    "ready"
                } else if workspace_kind == "repository" {
                    "provisioning"
                } else {
                    "not_required"
                }
                .into(),
            ),
            provisioning_error: None,
            created_at: now.clone(),
            updated_at: now,
        };
        if let Err(error) = self.db.save_project(&project) {
            let _ = std::fs::remove_dir_all(&workspace_path);
            return Err(error).with_context(|| {
                format!("cannot persist project for workflow '{}'", workflow.name)
            });
        }
        tracing::info!(
            project_id = %project.id,
            workflow_id = %workflow.id,
            catalog_id = ?project.catalog_id,
            workspace_kind,
            workspace = %workspace_path,
            "Created project from registered workflow"
        );
        Ok(project)
    }

    /// Create a project and its pinned default lane atomically from the API's
    /// perspective: a lane failure removes the project row and empty workspace.
    pub fn create_project_with_workflow_and_lane(
        &self,
        name: &str,
        repo_url: Option<&str>,
        base_branch: Option<&str>,
        git_provider_json: Option<&str>,
        workflow: &tj_db::WorkflowVersion,
    ) -> Result<(tj_db::Project, tj_db::Lane)> {
        let project = self.create_project_with_workflow(
            name,
            repo_url,
            base_branch,
            git_provider_json,
            workflow,
        )?;
        let description = workflow
            .catalog_label
            .clone()
            .or_else(|| workflow.catalog_id.clone())
            .unwrap_or_else(|| workflow.name.clone());
        match self.create_lane(&project.id, "Default", &workflow.id, Some(&description)) {
            Ok(lane) => Ok((project, lane)),
            Err(error) => {
                let _ = self.db.delete_project(&project.id);
                if let Some(path) = project.workspace_path.as_deref() {
                    let _ = std::fs::remove_dir_all(path);
                }
                Err(error)
                    .context("Default workflow lane could not be created; project was removed")
            }
        }
    }

    /// Explicit provisioning retry. Only a project in `failed` state may be
    /// retried; a ready repository is never re-cloned over existing work.
    pub async fn reprovision_project(&self, project_id: &str) -> Result<tj_db::Project> {
        let mut project = self
            .db
            .load_project(project_id)?
            .context("project does not exist")?;
        anyhow::ensure!(
            project_workspace_kind(&project) == "repository",
            "workflow '{}' declares no repository workspace; Git provisioning is not applicable",
            project.profile
        );
        anyhow::ensure!(
            project.provisioning_state.as_deref() != Some("ready"),
            "project is already provisioned; refusing to overwrite its workspace"
        );
        let url = project
            .repo_url
            .clone()
            .context("project has no repository URL")?;
        let workspace = project
            .workspace_path
            .clone()
            .context("project has no workspace path")?;
        let branch = project.base_branch.clone();
        let ssh_guard = if crate::ssh_guard::is_ssh_remote(&url) {
            let secrets = self
                .secrets_store
                .as_ref()
                .context("SSH provisioning requires the secrets store, which is not configured")?;
            let home = PathBuf::from(&self.workspace_base);
            Some(
                crate::ssh_guard::SshKeyGuard::new(secrets.as_ref(), project_id, &home)
                    .await?
                    .context("SSH remote requires a project key; generate one first")?,
            )
        } else {
            None
        };
        let env = ssh_guard.as_ref().map(|guard| guard.ssh_env());
        let workspace_path = PathBuf::from(&workspace);
        if workspace_path.exists() {
            // Failed state means there is no published work to preserve.
            let entries = std::fs::read_dir(&workspace_path)?;
            if entries.into_iter().next().is_some() {
                std::fs::remove_dir_all(&workspace_path)?;
            }
        }
        std::fs::create_dir_all(&workspace_path)?;
        let url_owned = url.clone();
        let workspace_owned = workspace_path.clone();
        let branch_owned = branch.clone();
        tokio::task::spawn_blocking(move || {
            tj_git::worktree::clone_repository(
                &url_owned,
                &workspace_owned,
                &branch_owned,
                env.as_ref(),
            )
        })
        .await
        .context("provisioning task panicked")??;
        project.provisioning_state = Some("ready".into());
        project.provisioning_error = None;
        project.updated_at = chrono::Utc::now().to_rfc3339();
        self.db.save_project(&project)?;
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<tj_db::Project>> {
        self.db.list_projects()
    }

    pub fn get_project(&self, id: &str) -> Result<Option<tj_db::Project>> {
        self.db.load_project(id)
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        tracing::debug!(project_id = %id, "Deleting project");
        self.db.delete_secrets_for_project(id)?;
        self.db.delete_project(id)?;
        let _ = self.tx.send(ManagerEvent::ProjectDeleted {
            project_id: id.to_string(),
        });
        Ok(())
    }

    pub fn list_lanes(&self) -> Result<Vec<tj_db::Lane>> {
        self.db.list_lanes()
    }

    pub fn list_lanes_for_project(&self, project_id: &str) -> Result<Vec<tj_db::Lane>> {
        self.db.list_lanes_for_project(project_id)
    }

    pub fn create_user(&self, user: &tj_db::User) -> Result<()> {
        self.db.create_user(user)
    }
    pub fn count_users(&self) -> Result<u64> {
        self.db.count_users()
    }
    pub fn get_user_by_username(&self, username: &str) -> Result<Option<tj_db::User>> {
        self.db.get_user_by_username(username)
    }
    pub fn list_users(&self) -> Result<Vec<tj_db::User>> {
        self.db.list_users()
    }
    pub fn issue_token(&self, token: &str, user_id: &str) -> Result<()> {
        self.db.issue_token(token, user_id)
    }
    pub fn user_for_token(&self, token: &str) -> Result<Option<tj_db::User>> {
        self.db.user_for_token(token)
    }
    pub fn revoke_token(&self, token: &str) -> Result<()> {
        self.db.revoke_token(token)
    }
    pub fn list_gate_decisions(&self, run_id: &str) -> Result<Vec<tj_db::GateDecision>> {
        self.db.list_gate_decisions(run_id)
    }
    pub fn list_events(&self, run_id: &str) -> Result<Vec<tj_db::RunEvent>> {
        self.db.list_events(run_id)
    }
    pub fn list_artifacts(&self, run_id: &str) -> Result<Vec<tj_db::RunArtifact>> {
        self.db.list_artifacts(run_id)
    }

    pub fn execution_bundle_record(
        &self,
        run_id: &str,
    ) -> Result<Option<tj_db::ExecutionBundleRecord>> {
        let run = self
            .db
            .load_run(run_id)?
            .with_context(|| format!("run '{}' does not exist", run_id))?;
        let record = self.db.execution_bundle_for_run(run_id)?;
        if run.execution_bundle_id().is_some() && record.is_none() {
            anyhow::bail!(
                "run '{}' references missing execution bundle '{}'",
                run_id,
                run.execution_bundle_id().unwrap_or_default()
            );
        }
        Ok(record)
    }

    pub fn execution_bundle(&self, run_id: &str) -> Result<Option<ExecutionBundle>> {
        let Some(record) = self.execution_bundle_record(run_id)? else {
            return Ok(None);
        };
        let bundle = ExecutionBundle::from_json(&record.bundle_json)
            .with_context(|| format!("run '{}' execution bundle is invalid", run_id))?;
        anyhow::ensure!(
            bundle.run_id == run_id,
            "execution bundle belongs to another run"
        );
        anyhow::ensure!(
            bundle.bundle_hash == record.bundle_hash,
            "execution bundle hash does not match its stored record"
        );
        Ok(Some(bundle))
    }

    /// Read-only frozen contract for API enforcement (for example the explicit
    /// external-delivery gate). Returns `None` for historical v1 rows, which
    /// have no capability declaration.
    pub fn run_contract(&self, run_id: &str) -> Result<Option<RunContract>> {
        let Some(bundle) = self.execution_bundle(run_id)? else {
            return Ok(None);
        };
        if bundle.schema_version != 2 {
            return Ok(None);
        }
        Ok(Some(RunContract::from_bundle(&bundle)?))
    }

    pub fn list_stage_logs(
        &self,
        run_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<tj_db::StageLog>> {
        self.db.list_stage_logs(run_id, after_seq, limit)
    }

    /// Feed webui user input to the run's waiting stage.
    ///
    /// Structured questions are addressed by `question_id`; the durable record
    /// is the acceptance gate, so an answer cannot leak to another stage or a
    /// later question. Legacy plain-text input still requires an explicitly
    /// waiting stage and is refused while any durable question is open.
    #[allow(clippy::too_many_arguments)]
    pub fn send_web_input(
        &self,
        run_id: &str,
        text: &str,
        stage_id: Option<&str>,
        question_id: Option<&str>,
        attempt: Option<u32>,
        idempotency_key: Option<&str>,
    ) -> Result<WebInputReceipt, String> {
        if let Some(qid) = question_id {
            let sid = stage_id
                .ok_or_else(|| "stage_id is required for a question-bound answer".to_string())?;
            let attempt = attempt
                .ok_or_else(|| "attempt is required for a question-bound answer".to_string())?;
            let key = idempotency_key
                .filter(|key| !key.trim().is_empty())
                .ok_or_else(|| {
                    "idempotency_key is required for a question-bound answer".to_string()
                })?;
            let acceptance = self
                .db
                .answer_question(qid, run_id, sid, Some(attempt), text, "webui", key)
                .map_err(|error| error.to_string())?;
            return Ok(WebInputReceipt {
                seq: None,
                question_id: Some(qid.to_string()),
                status: if acceptance.replayed {
                    "replayed".into()
                } else {
                    "accepted".into()
                },
                replayed: acceptance.replayed,
                answer_source: acceptance.question.answer_source,
            });
        }

        // A pending durable question cannot be bypassed with an unaddressed
        // legacy reply: that is exactly the stale-input path TJ-014 closes.
        if self
            .db
            .run_has_open_question(run_id)
            .map_err(|e| e.to_string())?
        {
            return Err("this run has a pending question; question_id, stage_id, attempt and idempotency_key are required".into());
        }

        if let Some(sid) = stage_id {
            let stage = self
                .db
                .load_stage(sid)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("stage '{}' does not exist", sid))?;
            let belongs = self
                .db
                .load_flows_for_run(run_id)
                .map(|flows| flows.iter().any(|f| f.id() == stage.flow_id().as_str()))
                .unwrap_or(false);
            if !belongs {
                return Err(format!("stage '{}' does not belong to this run", sid));
            }
            if stage.status().to_string() != "waiting_for_input" {
                return Err(format!(
                    "agent '{}' is not waiting for input",
                    stage.stage_name()
                ));
            }
            if let Some(expected) = attempt
                && expected != stage.iteration()
            {
                return Err(format!(
                    "stale attempt reply: stage '{}' is attempt {}, not {}",
                    stage.stage_name(),
                    stage.iteration(),
                    expected
                ));
            }
        }
        let waiting = self.db.run_has_waiting_stage(run_id).unwrap_or(false);
        if !waiting {
            return Err("no stage of this run is currently waiting for input".into());
        }
        match self.web_bridges.get(run_id) {
            Some(b) => Ok(WebInputReceipt {
                seq: Some(b.send_user_input(text)),
                question_id: None,
                status: "delivered".into(),
                replayed: false,
                answer_source: Some("webui".into()),
            }),
            None => Err("no stage waiting for input (chat session not active for this run)".into()),
        }
    }

    pub fn list_questions(
        &self,
        run_id: &str,
        open_only: bool,
    ) -> Result<Vec<tj_db::QuestionRecord>> {
        self.db.list_questions_for_run(run_id, open_only)
    }

    /// Delete a terminal run: workspace files, leftover git worktree and DB
    /// records. Running or gate-paused runs cannot be deleted.
    pub async fn delete_run(&self, run_id: &str) -> Result<(), String> {
        let run = self
            .db
            .load_run(run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("run '{}' does not exist", run_id))?;
        if !matches!(
            run.status(),
            RunStatus::Completed
                | RunStatus::Failed
                | RunStatus::Cancelled
                | RunStatus::Interrupted
        ) {
            return Err(format!(
                "run '{}' is {} — only completed, failed, cancelled or interrupted runs can be deleted",
                run_id,
                run.status()
            ));
        }
        if self
            .db
            .list_events(run_id)
            .map_err(|error| error.to_string())?
            .iter()
            .any(|event| event.kind == "execution_shutdown_failed")
        {
            return Err(
                "Execution shutdown was not confirmed; workspace retained for recovery".into(),
            );
        }
        // Terminal state fences writes before shutdown finishes. Deletion must
        // obtain the manager's joined acknowledgment before touching directories.
        self.cancel_run(run_id)
            .await
            .map_err(|error| error.to_string())?;

        // 1. Ephemeral workspace directories.
        let mut removed = Vec::new();
        if let Some(project_id) = run.project_id() {
            if let Ok(Some(project)) = self.db.load_project(project_id.as_str()) {
                // Repo-backed projects keep their workspace (only the worktree is ephemeral).
                if project.repo_url.is_none() {
                    if let Some(ws) = project.workspace_path {
                        removed.push(PathBuf::from(format!(
                            "{}/runs/{}",
                            ws.trim_end_matches('/'),
                            run_id
                        )));
                    }
                }
            }
        }
        if let Some(ws) = fallback_run_workspace(&self.workspace_base, run_id) {
            removed.push(PathBuf::from(ws));
        }
        for dir in &removed {
            if dir.exists() {
                match std::fs::remove_dir_all(dir) {
                    Ok(()) => {
                        tracing::info!(run_id = %run_id, dir = %dir.display(), "Removed run workspace")
                    }
                    Err(e) => {
                        tracing::warn!(run_id = %run_id, dir = %dir.display(), error = %e, "Could not remove run workspace")
                    }
                }
            }
        }

        // 2. Leftover git worktree — flow records know its path; the per-run
        //    bare repo clone sits next to it ({branch_safe}_repo).
        let flows = self.db.load_flows_for_run(run_id).unwrap_or_default();
        for flow in &flows {
            let Some(wp) = flow.worktree_path() else {
                continue;
            };
            let worktree_path = PathBuf::from(wp);
            if !worktree_path.exists() {
                continue;
            }
            let _ = std::fs::remove_dir_all(&worktree_path);
            if let Some(branch) = flow.branch_name() {
                if let Ok(wm) = WorktreeManager::new(&self.workspace_base) {
                    let repo_dir = wm
                        .worktrees_base()
                        .join(format!("{}_repo", branch.replace('/', "_")));
                    if repo_dir.exists() {
                        let _ = std::fs::remove_dir_all(&repo_dir);
                    }
                }
            }
            tracing::info!(run_id = %run_id, path = %worktree_path.display(), "Removed run worktree");
            break;
        }

        // 3. DB cascade.
        self.db
            .delete_run_cascade(run_id)
            .map_err(|e| e.to_string())?;

        // 4. Shared + actor-owned state.
        self.web_bridges.remove(run_id);
        let _ = self.tx.send(ManagerEvent::RunDeleted {
            run_id: run_id.to_string(),
        });
        tracing::info!(run_id = %run_id, status = %run.status(), "Run deleted");
        Ok(())
    }

    /// Cancel a queued (not yet consumed) webui input message.
    pub fn cancel_web_input(&self, run_id: &str, seq: u64) -> Result<bool, String> {
        match self.web_bridges.get(run_id) {
            Some(b) => Ok(b.cancel_input(seq)),
            None => Err("no active input channel for this run".into()),
        }
    }

    pub fn web_input_queued(&self, run_id: &str) -> Vec<(u64, String)> {
        self.web_bridges
            .get(run_id)
            .map(|b| b.queued())
            .unwrap_or_default()
    }

    pub fn web_transcript(&self, run_id: &str) -> Option<Vec<String>> {
        self.web_bridges.get(run_id).map(|b| b.transcript())
    }

    /// Resolve the run's workspace directory (project workspace, else per-run fallback).
    pub fn run_workspace(&self, run_id: &str) -> Option<PathBuf> {
        let project_ws = (|| {
            let run = self.db.load_run(run_id).ok()??;
            let project_id = run.project_id()?;
            let project = self.db.load_project(project_id.as_str()).ok()??;
            project.workspace_path
        })();
        project_ws.map(PathBuf::from).or_else(|| {
            Some(PathBuf::from(format!(
                "{}/{}",
                self.workspace_base.trim_end_matches('/'),
                run_id
            )))
        })
    }

    /// List files in the run workspace (relative paths, files only, capped).
    pub fn list_run_files(&self, run_id: &str) -> Result<Vec<String>, String> {
        let root = self
            .run_workspace(run_id)
            .ok_or_else(|| "run has no workspace".to_string())?;
        let mut out = Vec::new();
        walk_files(&root, &root, &mut out, 0);
        out.sort();
        Ok(out)
    }

    /// Read one file from the run workspace; path must stay inside it.
    pub fn read_run_file_bytes(&self, run_id: &str, rel: &str) -> Result<Vec<u8>, String> {
        let root = self
            .run_workspace(run_id)
            .ok_or_else(|| "run has no workspace".to_string())?;
        let full = root.join(rel);
        let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
        let canonical = full.canonicalize().map_err(|e| format!("{}: {}", rel, e))?;
        if !canonical.starts_with(&canonical_root) {
            return Err("path escapes workspace".into());
        }
        let meta = std::fs::metadata(&canonical).map_err(|e| e.to_string())?;
        if meta.len() > 8_000_000 {
            return Err("file too large to preview".into());
        }
        std::fs::read(&canonical).map_err(|e| format!("{}: {}", rel, e))
    }

    pub fn read_run_file(&self, run_id: &str, rel: &str) -> Result<String, String> {
        let root = self
            .run_workspace(run_id)
            .ok_or_else(|| "run has no workspace".to_string())?;
        let full = root.join(rel);
        let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
        let canonical = full.canonicalize().map_err(|e| format!("{}: {}", rel, e))?;
        if !canonical.starts_with(&canonical_root) {
            return Err("path escapes workspace".into());
        }
        let meta = std::fs::metadata(&canonical).map_err(|e| e.to_string())?;
        if meta.len() > 2_000_000 {
            return Err("file too large to display".into());
        }
        std::fs::read_to_string(&canonical).map_err(|e| e.to_string())
    }
    pub fn fork_artifact(&self, source_id: &str) -> Result<tj_db::RunArtifact> {
        let new_id = format!("art_{}", uuid::Uuid::new_v4());
        let forked_at = chrono::Utc::now().to_rfc3339();
        let f = self.db.fork_artifact(source_id, &new_id, &forked_at)?;
        Ok(f)
    }
    pub fn emit_event(
        &self,
        run_id: &str,
        kind: &str,
        actor: Option<&str>,
        flow_name: Option<&str>,
        stage_name: Option<&str>,
        summary: Option<&str>,
    ) {
        let e = tj_db::RunEvent {
            id: format!("evt_{}", uuid::Uuid::new_v4()),
            run_id: tj_db::RunId(run_id.to_string()),
            kind: kind.to_string(),
            actor: actor.map(|s| s.to_string()),
            flow_name: flow_name.map(|s| s.to_string()),
            stage_name: stage_name.map(|s| s.to_string()),
            summary: summary.map(|s| s.to_string()),
            payload: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(err) = self.db.append_event(&e) {
            tracing::warn!(run_id = %run_id, kind = %kind, error = %err, "Failed to append run event");
        }
    }

    /// Create a lane pinned to one exact immutable workflow version. No new
    /// lane may be left to follow a mutable configured graph.
    pub fn create_lane(
        &self,
        project_id: &str,
        name: &str,
        workflow_id: &str,
        description: Option<&str>,
    ) -> Result<tj_db::Lane> {
        let project = self
            .db
            .load_project(project_id)?
            .with_context(|| format!("project '{}' does not exist", project_id))?;
        let workflow = self
            .db
            .get_workflow(workflow_id)?
            .with_context(|| format!("workflow '{}' does not exist", workflow_id))?;
        anyhow::ensure!(
            workflow.capability_schema_version > 0,
            "workflow '{}' ('{}') predates explicit catalog/capability declarations; re-import it before pinning a lane",
            workflow.name,
            workflow.id
        );
        let (_, contract) = validated_workflow_contract(&workflow)?;
        let workflow_kind = match contract.workspace_mode() {
            tj_config::capability::WorkspaceMode::None => "none",
            tj_config::capability::WorkspaceMode::Repository => "repository",
        };
        anyhow::ensure!(
            project_workspace_kind(&project) == workflow_kind,
            "workflow '{}' uses workspace '{}' but project '{}' uses '{}'; lanes in one project must agree on workspace kind",
            workflow.name,
            workflow_kind,
            project.id,
            project_workspace_kind(&project)
        );
        let lane = tj_db::Lane {
            id: format!("lane_{}", uuid::Uuid::new_v4()),
            project_id: tj_db::ProjectId::from(project_id),
            name: name.into(),
            smg_path: workflow
                .source_rel_path
                .clone()
                .unwrap_or_else(|| workflow.smg_path.clone()),
            description: description.map(|s| s.into()),
            workflow_id: Some(workflow.id.clone()),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.db.save_lane(&lane)?;
        Ok(lane)
    }

    // ---- Workflow registry ----

    pub fn list_workflows(&self) -> Result<Vec<tj_db::WorkflowVersion>> {
        self.db.list_workflows()
    }

    pub fn get_workflow(&self, id: &str) -> Result<Option<tj_db::WorkflowVersion>> {
        self.db.get_workflow(id)
    }

    pub fn lanes_using_workflow(&self, workflow_id: &str) -> Result<Vec<tj_db::Lane>> {
        self.db.lanes_using_workflow(workflow_id)
    }

    /// Pin a lane to an existing registry version. Unpinning is refused: a new
    /// run must always resolve one exact frozen workflow.
    pub fn set_lane_workflow(&self, lane_id: &str, workflow_id: Option<&str>) -> Result<()> {
        let workflow_id = workflow_id.context(
            "a lane must stay pinned to a registry workflow; provide workflow_id (unpinning is refused)",
        )?;
        let lane = self
            .db
            .load_lane(lane_id)?
            .with_context(|| format!("lane '{}' does not exist", lane_id))?;
        let project = self
            .db
            .load_project(lane.project_id.as_str())?
            .with_context(|| format!("project '{}' does not exist", lane.project_id))?;
        let workflow = self
            .db
            .get_workflow(workflow_id)?
            .with_context(|| format!("workflow '{}' does not exist", workflow_id))?;
        anyhow::ensure!(
            workflow.capability_schema_version > 0,
            "workflow '{}' ('{}') predates explicit catalog/capability declarations; re-import it before pinning",
            workflow.name,
            workflow.id
        );
        let (smg, contract) = validated_workflow_contract(&workflow)
            .with_context(|| format!("workflow '{}' cannot be pinned", workflow_id))?;
        let workflow_kind = match contract.workspace_mode() {
            tj_config::capability::WorkspaceMode::None => "none",
            tj_config::capability::WorkspaceMode::Repository => "repository",
        };
        anyhow::ensure!(
            project_workspace_kind(&project) == workflow_kind,
            "workflow '{}' workspace policy is incompatible with project '{}'",
            workflow.name,
            project.id
        );
        if let Some(assets) = self.registry_assets_for_workflow(&workflow)? {
            self.config_loader
                .validate_smg_with_assets(&smg, &assets)
                .with_context(|| format!("workflow '{}' cannot be pinned", workflow_id))?;
        } else {
            self.config_loader
                .validate_smg(&smg)
                .with_context(|| format!("workflow '{}' cannot be pinned", workflow_id))?;
        }
        self.db.set_lane_workflow(lane_id, Some(workflow_id))
    }

    /// Re-scan only the already configured explicit sources and register unseen
    /// immutable versions. Returns the full registry after import.
    pub fn import_workflows(&self) -> Result<Vec<tj_db::WorkflowVersion>> {
        let loaded = self.config_loader.load_workflow_sources()?;
        register_loaded_workflows(&self.db, &loaded)?;
        self.db.list_workflows()
    }
}

/// Parse and validate one immutable registry version's actual declaration. A
/// missing capability hash is tolerated only for historical Batch 2 rows; when
/// present it must match the bytes, so a tampered row cannot select a different
/// workspace or gate policy.
fn validated_workflow_contract(
    workflow: &tj_db::WorkflowVersion,
) -> Result<(
    StateMachineGraph,
    tj_config::capability::WorkflowCapabilities,
)> {
    anyhow::ensure!(
        workflow.capability_schema_version
            == i64::from(tj_config::capability::CAPABILITY_SCHEMA_VERSION),
        "workflow '{}' ({}) declares unsupported capability schema version {}; re-import it with capabilities.schema_version {}",
        workflow.name,
        workflow.id,
        workflow.capability_schema_version,
        tj_config::capability::CAPABILITY_SCHEMA_VERSION
    );
    let graph: StateMachineGraph = serde_json::from_str(&workflow.content_json)
        .with_context(|| format!("workflow '{}' contains invalid SMG JSON", workflow.id))?;
    let contract = tj_config::capability::effective_contract(&graph);
    contract.validate(&graph).with_context(|| {
        format!(
            "workflow '{}' has an invalid capability declaration",
            workflow.id
        )
    })?;
    if let Some(expected) = workflow.capability_hash.as_deref() {
        let actual = tj_config::capability::contract_hash(&contract)?;
        anyhow::ensure!(
            expected == actual,
            "workflow '{}' capability hash mismatch: registry {} recomputed {}; re-import the source",
            workflow.id,
            expected,
            actual
        );
    }
    Ok((graph, contract))
}

/// Register validated loaded workflows. Duplicate content is idempotent; a
/// second catalog default fails before the transaction accepts it.
fn register_loaded_workflows(
    db: &tj_db::Database,
    loaded: &[tj_config::LoadedWorkflow],
) -> Result<usize> {
    let mut imported = 0;
    for item in loaded {
        let graph = &item.graph;
        let catalog = graph.catalog.as_ref().with_context(|| {
            format!(
                "workflow '{}' from {} has no catalog declaration",
                graph.name,
                item.source_path.display()
            )
        })?;
        let capabilities = tj_config::capability::effective_contract(graph);
        capabilities.validate(graph).with_context(|| {
            format!(
                "workflow '{}' from {} has an invalid capability declaration",
                graph.name,
                item.source_path.display()
            )
        })?;
        let capability_hash = tj_config::capability::contract_hash(&capabilities)?;
        let flow_count = graph.flows.len() as i64;
        let gate_count = graph
            .flows
            .values()
            .filter(|flow| flow.flow_type == tj_config::smg::FlowType::Gate)
            .count() as i64;
        let stage_count: i64 = graph
            .flows
            .values()
            .map(|flow| flow.stages.len() as i64)
            .sum();
        let wf = tj_db::WorkflowVersion {
            id: format!("wf_{}", uuid::Uuid::new_v4()),
            name: graph.name.clone(),
            smg_path: item.source_rel_path.clone(),
            version: graph.version as i64,
            revision: 0,
            description: if graph.description.is_empty() {
                None
            } else {
                Some(graph.description.clone())
            },
            content_hash: item.content_hash.clone(),
            content_json: item.content_json.clone(),
            entry: Some(graph.entry.clone()),
            exit: Some(graph.exit.clone()),
            flow_count,
            gate_count,
            stage_count,
            catalog_id: Some(catalog.id.clone()),
            catalog_label: Some(catalog.label_or(&graph.name).to_string()),
            catalog_description: Some(catalog.description_or(&graph.description).to_string()),
            catalog_default: catalog.default,
            capability_schema_version: i64::from(capabilities.schema_version),
            capability_hash: Some(capability_hash),
            source_id: Some(item.source_id.clone()),
            source_rel_path: Some(item.source_rel_path.clone()),
            source_kind: Some(item.source_kind.as_str().to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let registered = if item.assets.is_empty() {
            db.register_workflow(&wf)?
        } else {
            let assets = item
                .assets
                .iter()
                .map(|asset| tj_db::WorkflowAsset {
                    logical_path: asset.logical_path.clone(),
                    content_hash: asset.content_hash.clone(),
                    content: asset.content.clone(),
                })
                .collect::<Vec<_>>();
            db.register_workflow_with_assets(&wf, &assets)?
        };
        if registered.id == wf.id {
            imported += 1;
        }
    }
    Ok(imported)
}

impl SystemOperations for ManagerHandle {
    fn get_job<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            self.authorize_job_operation(id).ok()?;
            self.db
                .load_run(id)
                .ok()
                .flatten()
                .map(|r| serde_json::to_value(&r).unwrap_or_default())
        })
    }

    fn list_jobs<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            let Ok(user) = self.operation_user() else {
                return Vec::new();
            };
            self.db
                .list_runs()
                .map(|runs| {
                    runs.into_iter()
                        .filter(|r| self.db.can_access_run(&user.id, r.id()).unwrap_or(false))
                        .map(|r| serde_json::to_value(&r).unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    fn create_job<'a>(
        &'a self,
        description: &'a str,
        project_id: Option<&'a str>,
        lane_id: Option<&'a str>,
        repo_url: Option<&'a str>,
        base_branch: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            let user = match self.operation_user() {
                Ok(user) => user,
                Err(error) => return Some(serde_json::json!({"error":error.to_string()})),
            };
            if !project_id
                .is_some_and(|id| self.db.can_access_project(&user.id, id).unwrap_or(false))
            {
                return Some(serde_json::json!({"error":"Project access denied"}));
            }
            match self.create_run(
                description,
                project_id,
                lane_id,
                repo_url,
                base_branch,
                Some(&user.username),
            ) {
                Ok(r) => serde_json::to_value(&r).ok(),
                Err(e) => Some(serde_json::json!({ "error": e.to_string() })),
            }
        })
    }

    fn cancel_job<'a>(
        &'a self,
        job_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.authorize_job_operation(job_id)
                .map_err(|error| error.to_string())?;
            self.cancel_run(job_id)
                .await
                .map_err(|error| error.to_string())?;
            match self
                .db
                .load_run(job_id)
                .map_err(|error| error.to_string())?
            {
                Some(run) if *run.status() == RunStatus::Cancelled => Ok(()),
                Some(run) => Err(format!("Job already ended with status {}", run.status())),
                None => Err("Job no longer exists".into()),
            }
        })
    }

    fn list_flows<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            if self.operation_user().is_err() {
                return Vec::new();
            }
            let mut rows = Vec::new();
            let Ok(workflows) = self.db.list_workflows() else {
                return rows;
            };
            let lanes = self.db.list_lanes().unwrap_or_default();
            let mut seen = std::collections::HashSet::new();
            for workflow in workflows {
                let catalog_key = workflow
                    .catalog_id
                    .clone()
                    .unwrap_or_else(|| workflow.name.clone());
                if !seen.insert(catalog_key) {
                    continue;
                }
                let graph: Option<StateMachineGraph> =
                    serde_json::from_str(&workflow.content_json).ok();
                let stages: Vec<String> = graph
                    .as_ref()
                    .map(|graph| {
                        graph
                            .flow_order
                            .iter()
                            .filter_map(|flow| graph.flows.get(flow))
                            .flat_map(|flow| flow.stages.keys().cloned())
                            .collect()
                    })
                    .unwrap_or_default();
                let gate = graph.as_ref().is_some_and(|graph| {
                    graph
                        .flows
                        .values()
                        .any(|flow| flow.flow_type == FlowType::Gate)
                });
                let workflow_lanes: Vec<serde_json::Value> = lanes
                    .iter()
                    .filter(|lane| lane.workflow_id.as_deref() == Some(workflow.id.as_str()))
                    .map(|lane| {
                        serde_json::json!({
                            "id": lane.id, "project_id": lane.project_id, "name": lane.name,
                        })
                    })
                    .collect();
                rows.push(serde_json::json!({
                    "name": workflow.name,
                    "workflow_id": workflow.id,
                    "catalog_id": workflow.catalog_id,
                    "catalog_label": workflow.catalog_label,
                    "maturity": graph.as_ref().map(|graph| graph.maturity.clone())
                        .unwrap_or(tj_config::smg::WorkflowMaturity::Experimental),
                    "repository_free": graph.as_ref().is_none_or(|graph| {
                        tj_config::capability::effective_contract(graph).workspace.kind
                            == tj_config::capability::WorkspaceMode::None
                    }),
                    "description": workflow.description,
                    "stages": stages,
                    "gate": gate,
                    "lanes": workflow_lanes,
                    "lane_hint": "Pass one of these lane id values as lane_id when starting the Job.",
                }));
            }
            rows
        })
    }

    fn approve_gate<'a>(
        &'a self,
        job_id: &'a str,
        gate_instance_id: &'a str,
        expected_state_version: i64,
        idempotency_key: &'a str,
        approved: bool,
        reason: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<serde_json::Value, String>> + Send + 'a>>
    {
        Box::pin(async move {
            let user = self
                .authorize_job_operation(job_id)
                .map_err(|error| error.to_string())?;
            let run = self
                .db
                .load_run(job_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Job no longer exists".to_string())?;
            let gate_name = run
                .gate_name()
                .ok_or_else(|| "Job is not waiting at an identified gate".to_string())?;
            let binding = self
                .db
                .gate_packet_binding(gate_instance_id)
                .map_err(|error| error.to_string())?;
            let plan_binding = self
                .db
                .gate_plan_binding(gate_instance_id)
                .map_err(|error| error.to_string())?;
            let output_binding = self
                .db
                .gate_output_binding(gate_instance_id)
                .map_err(|error| error.to_string())?;
            let decision = tj_db::GateDecision {
                id: String::new(),
                run_id: tj_db::RunId(job_id.to_string()),
                gate_name: gate_name.to_string(),
                actor_id: user.id.clone(),
                actor_name: user.display_name.clone().unwrap_or(user.username.clone()),
                approved,
                reason: reason.map(str::to_string),
                note: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                gate_instance_id: Some(gate_instance_id.to_string()),
                idempotency_key: Some(idempotency_key.to_string()),
                expected_state_version: Some(expected_state_version),
                state_version: None,
                disposition: None,
                packet_revision_id: binding.as_ref().map(|item| item.revision_id.clone()),
                packet_content_hash: binding.as_ref().map(|item| item.content_hash.clone()),
                plan_revision_id: plan_binding.as_ref().map(|item| item.revision_id.clone()),
                plan_content_hash: plan_binding.as_ref().map(|item| item.content_hash.clone()),
                output_artifact_id: output_binding.as_ref().map(|item| item.artifact_id.clone()),
                output_content_hash: output_binding
                    .as_ref()
                    .map(|item| item.content_hash.clone()),
                output_stage_name: output_binding.as_ref().map(|item| item.stage_name.clone()),
                acknowledge_incomplete: false,
            };
            let receipt = self
                .submit_gate_decision(decision, expected_state_version)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_value(receipt).map_err(|error| error.to_string())
        })
    }

    fn create_project<'a>(
        &'a self,
        name: &'a str,
        repo_url: Option<&'a str>,
        base_branch: Option<&'a str>,
        git_provider_json: Option<&'a str>,
        workflow_id: Option<&'a str>,
        catalog_id: Option<&'a str>,
        legacy_profile: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            let user = self.operation_user().ok()?;
            if !self.db.is_admin_user(&user.id).ok()? {
                return Some(serde_json::json!({"error":"Administrator access required"}));
            }
            let workflow =
                match self.resolve_project_workflow(workflow_id, catalog_id, legacy_profile) {
                    Ok(workflow) => workflow,
                    Err(error) => {
                        return Some(serde_json::json!({"error": error.to_string()}));
                    }
                };
            match self.create_project_with_workflow_and_lane(
                name,
                repo_url,
                base_branch,
                git_provider_json,
                &workflow,
            ) {
                Ok((project, lane)) => Some(serde_json::json!({
                    "project": project,
                    "lane": lane,
                    "workflow_id": workflow.id,
                })),
                Err(error) => Some(serde_json::json!({"error": error.to_string()})),
            }
        })
    }

    fn list_projects<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            let Ok(user) = self.operation_user() else {
                return Vec::new();
            };
            self.db.list_projects()
                .map(|projects| projects.into_iter().filter(|p| self.db.can_access_project(&user.id, &p.id).unwrap_or(false)).map(|p| serde_json::json!({
                    "id":p.id, "name":p.name, "repo_url":p.repo_url, "base_branch":p.base_branch,
                    "workspace_path":p.workspace_path, "profile":p.profile,
                    "catalog_id":p.catalog_id, "workspace_kind":p.workspace_kind, "created_at":p.created_at,
                })).collect())
                .unwrap_or_default()
        })
    }

    fn get_delivery<'a>(
        &'a self,
        job_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            self.authorize_job_operation(job_id).ok()?;
            self.delivery_bundle(job_id).ok()
        })
    }

    fn list_lanes<'a>(
        &'a self,
        project_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            let Ok(user) = self.operation_user() else {
                return Vec::new();
            };
            match project_id {
                Some(pid) => self.db.list_lanes_for_project(pid),
                None => self.db.list_lanes(),
            }
            .map(|lanes| {
                lanes
                    .into_iter()
                    .filter(|l| {
                        self.db
                            .can_access_project(&user.id, l.project_id.as_str())
                            .unwrap_or(false)
                    })
                    .map(|l| serde_json::to_value(&l).unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default()
        })
    }
}

pub struct SpawnResult {
    pub handle: ManagerHandle,
    pub secrets_store: Option<Arc<dyn SecretsStore>>,
    /// Resolved configuration is shared read-only with readiness/diagnostics.
    pub config_loader: Arc<ConfigLoader>,
}

pub async fn spawn(config_loader: ConfigLoader) -> Result<SpawnResult> {
    let config_loader = Arc::new(config_loader);
    // Parse and validate every explicitly configured source before opening the
    // database. Empty sources are valid: the registry stays empty.
    let loaded_sources = config_loader.load_workflow_sources()?;
    let sys = config_loader.config();
    anyhow::ensure!(
        sys.chat.is_none(),
        "Project-scoped access requires authenticated chat identity mapping; the legacy chat integration is unsupported. Remove the chat configuration and use webui/Chief until a mapped adapter is available."
    );
    if let Some(seconds) = sys.run_timeout_secs {
        anyhow::ensure!(
            seconds > 0,
            "run_timeout_secs must be positive when configured"
        );
        let duration = i64::try_from(seconds)
            .ok()
            .and_then(chrono::Duration::try_seconds)
            .context("run_timeout_secs is too large")?;
        anyhow::ensure!(
            chrono::Utc::now().checked_add_signed(duration).is_some(),
            "run_timeout_secs overflows timestamps"
        );
    }
    let workspace_base = expand_env_vars(&sys.workspace_base);

    let db = Arc::new(Database::open(&sys.database_path)?);
    match db.reconcile_delivery_startup() {
        Ok(0) => {}
        Ok(count) => tracing::warn!(
            count,
            "Restart reconciliation marked in-flight delivery operations unknown; no success inferred"
        ),
        Err(error) => {
            return Err(error).context(
                "delivery restart reconciliation failed; refusing to invent external state",
            );
        }
    }

    // TJ-024 bounded telemetry retention. This only removes replayable stage
    // activity/provider attempts; runs, artifacts, decisions, packets and
    // encrypted secrets are never touched.
    if sys.activity_retention_days > 0 {
        let days = i64::try_from(sys.activity_retention_days).unwrap_or(i64::MAX);
        if let Some(duration) = chrono::Duration::try_days(days) {
            let before = (chrono::Utc::now() - duration).to_rfc3339();
            match db.prune_activity_before(&before) {
                Ok((logs, attempts)) if logs > 0 || attempts > 0 => {
                    tracing::info!(
                        stage_logs = logs,
                        provider_attempts = attempts,
                        before,
                        "Pruned durable activity by retention policy"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "Activity retention cleanup failed; startup continues")
                }
            }
        }
    }

    // Register already-validated sources. Empty sources intentionally leave the
    // registry empty; readiness reports degraded and project creation returns
    // actionable guidance instead of inventing a default workflow.
    match register_loaded_workflows(&db, &loaded_sources) {
        Ok(0) if loaded_sources.is_empty() => {
            tracing::info!("No workflow sources configured; starting with an empty registry");
        }
        Ok(n) if n > 0 => tracing::info!(imported = n, "Workflow registry populated"),
        Ok(_) => {}
        Err(error) => {
            return Err(error).context("Workflow registry import failed; refusing startup");
        }
    }

    let chat: Option<Arc<dyn ChatProvider>> =
        sys.chat
            .as_ref()
            .map(|chat_config| match chat_config.provider.as_str() {
                "zulip" => {
                    let zulip_cfg = chat_config
                        .zulip
                        .as_ref()
                        .expect("zulip chat provider requires zulip config");
                    let api_key = std::env::var(&zulip_cfg.api_key_env)
                        .unwrap_or_else(|_| panic!("Missing env var: {}", zulip_cfg.api_key_env));
                    let config = ZulipConfig {
                        server: zulip_cfg.server.clone(),
                        bot_email: zulip_cfg.bot_email.clone(),
                        api_key,
                        default_stream: zulip_cfg.default_stream.clone(),
                    };
                    Arc::new(ZulipChat::new(config)) as Arc<dyn ChatProvider>
                }
                other => panic!("Unknown chat provider: {}", other),
            });

    let agent_config = sys
        .agents
        .iter()
        .find(|a| a.default && a.enabled)
        .or_else(|| sys.agents.iter().find(|a| a.enabled))
        .ok_or_else(|| anyhow::anyhow!("No enabled agent found in config"))?;

    let provider_config = sys
        .providers
        .iter()
        .find(|p| p.id == agent_config.provider_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Agent '{}' provider '{}' not found",
                agent_config.id,
                agent_config.provider_id
            )
        })?;
    let provider_config_clone = provider_config.clone();

    // Extract agent config values before config_loader is moved
    let agent_id = agent_config.id.clone();
    let agent_model = agent_config.model.clone();
    let agent_system_prompt = agent_config.system_prompt.clone();
    let agent_tools = agent_config.tools.clone();

    let provider = tj_provider::OpenAiProvider::from_config(provider_config)?;
    let agent = Arc::new(Agent::new(
        agent_id.clone(),
        Box::new(provider),
        agent_model.clone(),
        agent_system_prompt.clone(),
        &agent_tools,
    ));

    let agent_sessions = Arc::new(DashMap::new());

    let (tx, rx) = mpsc::unbounded_channel();
    let (pipeline_tx, _) = broadcast::channel::<PipelineNotification>(256);

    let web_bridges: Arc<DashMap<String, Arc<tj_chat::WebInputBridge>>> = Arc::new(DashMap::new());

    let handle = ManagerHandle {
        db: Arc::clone(&db),
        tx: tx.clone(),
        pipeline_events: pipeline_tx.clone(),
        agent: agent.clone(),
        agent_sessions: agent_sessions.clone(),
        web_bridges: Arc::clone(&web_bridges),
        workspace_base: workspace_base.clone(),
        config_loader: Arc::clone(&config_loader),
        accepting_runs: Arc::new(std::sync::Mutex::new(true)),
        run_timeout_secs: sys.run_timeout_secs,
        operation_token: None,
        secrets_store: DbSecretsStore::open(Arc::clone(&db))
            .ok()
            .map(|store| Arc::new(store) as Arc<dyn SecretsStore>),
    };

    let chat_for_chief = chat.clone();

    // Construct WorktreeManager from workspace_base
    let worktree_manager = match WorktreeManager::new(&workspace_base) {
        Ok(wm) => {
            tracing::info!(worktrees_base = %workspace_base, "WorktreeManager created");
            Some(wm)
        }
        Err(e) => {
            tracing::warn!(error = %e, base = %workspace_base, "Failed to create WorktreeManager, worktree support disabled");
            None
        }
    };

    // Construct secrets store for per-project git provider resolution
    let secrets_store: Option<Arc<dyn SecretsStore>> = match DbSecretsStore::open(Arc::clone(&db)) {
        Ok(store) => {
            tracing::info!("Secrets store initialized");
            Some(Arc::new(store))
        }
        Err(e) => {
            tracing::warn!(error = %e, "Secrets store disabled — TURBOJET_MASTER_KEY not set or invalid");
            None
        }
    };

    // Construct RunNotifier from chat provider if available
    let notifier = chat.as_ref().map(|cp| {
        let default_stream = sys
            .chat
            .as_ref()
            .and_then(|c| c.zulip.as_ref())
            .map(|z| z.default_stream.clone())
            .unwrap_or_else(|| "turbojet".into());
        RunNotifier::new(cp.clone(), Arc::clone(&db), default_stream)
    });

    let secrets_store_for_result = secrets_store.clone();

    let mut actor = ManagerActor {
        db,
        smg_cache: std::sync::Mutex::new(HashMap::new()),
        contract_cache: std::sync::Mutex::new(HashMap::new()),
        legacy_policy_cache: std::sync::Mutex::new(HashMap::new()),
        config_loader: Arc::clone(&config_loader),
        tx,
        watchdogs: HashMap::new(),
        executions: HashMap::new(),
        cleanup_blocked: HashSet::new(),
        pipeline_events: pipeline_tx,
        chat,
        worktrees: HashMap::new(),
        worktree_manager,
        git_providers: HashMap::new(),
        secrets_store,
        notifier,
        git_provider_warned: HashSet::new(),
        workspace_base,
        web_bridges,
    };

    for run in handle.db.list_runs()? {
        if *run.status() == RunStatus::WaitingForGate {
            actor.start_watchdog(run.id(), None);
        }
    }
    let actor_handle = actor.tx.clone();
    tokio::spawn(async move {
        actor.run(rx).await;
    });

    if let Ok(runs) = handle.db.list_runs() {
        for run in runs {
            if *run.status() != RunStatus::Running {
                continue;
            }
            // Close any active-time interval at the run's last durable
            // activity, never at "now" (which would charge downtime).
            let ended_at = handle
                .db
                .last_run_activity(run.id())
                .ok()
                .flatten()
                .and_then(|ts| chrono::DateTime::parse_from_rfc3339(&ts).ok())
                .map(|ts| ts.with_timezone(&chrono::Utc))
                .unwrap_or_else(chrono::Utc::now);
            if handle.db.has_run_budget(run.id()).unwrap_or(false) {
                let _ = handle.db.reconcile_budget_intervals(
                    run.id(),
                    ended_at,
                    "process restart; active interval fenced",
                );
            }
            let mut pure_gate_effect = false;
            let mut uncertain_effect = false;
            if let Ok(effects) = handle.db.list_pending_effects(run.id(), true) {
                for effect in effects {
                    if effect.kind == "gate_route"
                        && effect.status != tj_db::EffectStatus::Ambiguous
                    {
                        pure_gate_effect = true;
                    } else {
                        uncertain_effect = true;
                    }
                }
            }
            if pure_gate_effect && !uncertain_effect {
                tracing::info!(run_id = %run.id(), "Reconciling durable pending gate route without replaying execution");
                let _ = actor_handle.send(ManagerEvent::ReconcilePendingEffects {
                    run_id: run.id().to_string(),
                });
                continue;
            }
            if uncertain_effect && let Ok(effects) = handle.db.list_pending_effects(run.id(), true)
            {
                for effect in effects.iter().filter(|effect| effect.kind != "gate_route") {
                    let _ = handle.db.mark_pending_effect_ambiguous(
                        &effect.id,
                        "process restart; effect may have run but routing/publication is unproven",
                    );
                }
            }
            match handle.db.mark_run_interrupted(
                run.id(),
                "process restart while execution was in flight; explicit retry required",
                uncertain_effect,
            ) {
                Ok(Some(_)) => {
                    tracing::warn!(run_id = %run.id(), "Run marked interrupted; no automatic model/tool/publication replay");
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::error!(run_id = %run.id(), %error, "Cannot mark interrupted run; refusing automatic resume");
                }
            }
        }
    }

    if let Some(chat_provider) = chat_for_chief.clone() {
        let system_ops: Arc<dyn SystemOperations> = Arc::new(handle.clone());
        let chief_config = ChiefConfig {
            agent_id,
            provider: Box::new(tj_provider::OpenAiProvider::from_config(
                &provider_config_clone,
            )?),
            model: agent_model,
            system_prompt: agent_system_prompt,
            enabled_tools: agent_tools,
        };
        let chief = Chief::new(chief_config, system_ops);
        tokio::spawn(async move {
            chief.run(chat_provider).await;
        });
    }

    // Periodic state snapshot
    {
        let db_for_snapshot = Arc::clone(&handle.db);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;

                // Flag stuck runs: no observable progress (events/stage updates) for 15 minutes.
                // Long LLM stages emit nothing for a while, so an absolute age check would
                // false-positive on healthy runs.
                if let Ok(runs) = db_for_snapshot.list_runs() {
                    let active_runs = runs
                        .iter()
                        .filter(|r| *r.status() == RunStatus::Running)
                        .count();
                    let now = chrono::Utc::now();
                    for run in runs.iter().filter(|r| *r.status() == RunStatus::Running) {
                        let activity = db_for_snapshot.last_run_activity(run.id()).ok().flatten();
                        let ts = match activity {
                            Some(ts) => ts,
                            None => run.created_at().to_string(),
                        };
                        let Ok(last) = chrono::DateTime::parse_from_rfc3339(&ts) else {
                            continue;
                        };
                        let idle = now.signed_duration_since(last.with_timezone(&chrono::Utc));
                        if idle.num_minutes() >= 15 {
                            tracing::warn!(
                                run_id = %run.id(),
                                trace_id = ?run.trace_id(),
                                idle_minutes = idle.num_minutes(),
                                "Run appears stuck: no progress in 15 minutes"
                            );
                        }
                    }
                    tracing::debug!(active_runs, "State snapshot");
                }
            }
        });
    }

    Ok(SpawnResult {
        secrets_store: secrets_store_for_result,
        handle,
        config_loader,
    })
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;

struct ManagerActor {
    db: Arc<Database>,
    smg_cache: std::sync::Mutex<HashMap<String, StateMachineGraph>>,
    contract_cache: std::sync::Mutex<HashMap<String, Arc<RunContract>>>,
    legacy_policy_cache: std::sync::Mutex<HashMap<String, LegacyV1Policy>>,
    #[allow(dead_code)]
    config_loader: Arc<ConfigLoader>,
    tx: mpsc::UnboundedSender<ManagerEvent>,
    watchdogs: HashMap<String, Watchdog>,
    executions: HashMap<String, ActiveExecution>,
    cleanup_blocked: HashSet<String>,
    pipeline_events: broadcast::Sender<PipelineNotification>,
    chat: Option<Arc<dyn ChatProvider>>,
    worktrees: HashMap<String, WorktreeInfo>,
    worktree_manager: Option<WorktreeManager>,
    git_providers: HashMap<String, Arc<dyn GitProvider>>,
    secrets_store: Option<Arc<dyn SecretsStore>>,
    notifier: Option<RunNotifier>,
    git_provider_warned: HashSet<String>,
    workspace_base: String,
    web_bridges: Arc<DashMap<String, Arc<tj_chat::WebInputBridge>>>,
}

struct ActiveExecution {
    task: tokio::task::JoinHandle<()>,
    processes: tj_tools::processes::ProcessSupervisor,
}

struct Watchdog {
    generation: String,
    task: tokio::task::JoinHandle<()>,
    reason: &'static str,
}

/// Runtime policy source. New runs are always schema 2; v1/no-bundle rows are
/// handled only by the isolated legacy adapter.
enum RunPolicy {
    V2(Arc<RunContract>),
    V1(LegacyV1Policy),
}

/// Derived workspace policy. Legacy rows with no explicit kind fall back to
/// their frozen profile only through the isolated v1 adapter.
fn project_workspace_kind(project: &tj_db::Project) -> &str {
    legacy_v1::project_workspace_kind(project)
}

/// Expand environment variables in a path string
fn expand_env_vars(path: &str) -> String {
    let mut result = path.to_string();

    // Expand ~ to HOME directory
    if let Ok(home) = std::env::var("HOME") {
        result = result.replace("~", &home);
    }

    // Expand $TURBOJET_HOME if set (legacy support)
    if let Ok(turbojet_home) = std::env::var("TURBOJET_HOME") {
        result = result.replace("$TURBOJET_HOME", &turbojet_home);
    }

    // Expand $TURBOJET_WORKSPACE if set (new workspace-specific variable)
    if let Ok(turbojet_workspace) = std::env::var("TURBOJET_WORKSPACE") {
        result = result.replace("$TURBOJET_WORKSPACE", &turbojet_workspace);
    }

    // If path is relative (doesn't start with / or ~), make it relative to current directory
    if !result.starts_with('/') && !result.starts_with('~') && !result.starts_with('$') {
        // Keep relative paths as-is (they'll be relative to current working directory)
        // This allows using .turbojet in the project directory
    }

    result
}

impl ManagerActor {
    fn resolve_trace_id(&self, run_id: &str) -> String {
        self.db
            .load_run(run_id)
            .ok()
            .flatten()
            .and_then(|r| r.trace_id().map(|s| s.to_string()))
            .unwrap_or_default()
    }

    /// Resolve the run's workspace directory: the project workspace when the run
    /// belongs to a project, otherwise a per-run fallback directory under
    /// workspace_base (created on demand). The fallback guarantees stage tools
    /// always get a real workspace root even when no worktree/repo is configured
    /// (runs created without a project would otherwise end up with an empty path).
    fn project_for_run(&self, run_id: &str) -> Option<tj_db::Project> {
        self.db
            .load_run(run_id)
            .ok()
            .flatten()
            .and_then(|run| run.project_id().map(|id| id.as_str().to_string()))
            .and_then(|project_id| self.db.load_project(&project_id).ok().flatten())
    }

    fn project_profile_for_run(&self, run_id: &str) -> String {
        self.project_for_run(run_id)
            .map(|project| project.profile)
            .unwrap_or_else(|| LegacyV1Policy::default_profile().to_string())
    }

    /// Immutable v2 contract for a new run. Never falls back to a project
    /// profile or a mutable registry row.
    fn contract_for_run(&self, run_id: &str) -> Result<Arc<RunContract>> {
        if let Some(contract) = self
            .contract_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(run_id)
            .cloned()
        {
            return Ok(contract);
        }
        let run = self
            .db
            .load_run(run_id)?
            .with_context(|| format!("run '{}' does not exist", run_id))?;
        let bundle = load_execution_bundle(&self.db, &run)?.with_context(|| {
            format!(
                "run '{}' has no v2 execution bundle; only frozen v1 rows may use the legacy adapter",
                run_id
            )
        })?;
        let contract = Arc::new(RunContract::from_bundle(&bundle).with_context(|| {
            format!(
                "run '{}' execution contract is invalid or predates explicit capabilities",
                run_id
            )
        })?);
        self.contract_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(run_id.to_string(), Arc::clone(&contract));
        Ok(contract)
    }

    /// Resolve whether this run is executed by the frozen v2 contract or by the
    /// isolated v1 adapter. No project catalog/profile field is consulted for a
    /// v2 run.
    fn run_policy(&self, run_id: &str) -> Result<RunPolicy> {
        let run = self
            .db
            .load_run(run_id)?
            .with_context(|| format!("run '{}' does not exist", run_id))?;
        match load_execution_bundle(&self.db, &run)? {
            Some(bundle) if bundle.schema_version == 2 => {
                Ok(RunPolicy::V2(self.contract_for_run(run_id)?))
            }
            _ => {
                let policy = if let Some(policy) = self
                    .legacy_policy_cache
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(run_id)
                    .copied()
                {
                    policy
                } else {
                    let policy =
                        LegacyV1Policy::from_profile(&self.project_profile_for_run(run_id));
                    self.legacy_policy_cache
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(run_id.to_string(), policy);
                    policy
                };
                Ok(RunPolicy::V1(policy))
            }
        }
    }

    fn legacy_policy_for_run(&self, run_id: &str) -> Result<Option<LegacyV1Policy>> {
        match self.run_policy(run_id)? {
            RunPolicy::V1(policy) => Ok(Some(policy)),
            RunPolicy::V2(_) => Ok(None),
        }
    }

    fn run_workspace_is_repository(&self, run_id: &str) -> Result<bool> {
        match self.run_policy(run_id)? {
            RunPolicy::V2(contract) => {
                Ok(contract.workspace_mode() == tj_config::capability::WorkspaceMode::Repository)
            }
            RunPolicy::V1(policy) => Ok(!policy.workspace_is_repository_free()),
        }
    }

    /// Resolve the actual binding for one gate. V2 gates come only from the
    /// frozen contract; v1 gates come only from the isolated adapter.
    fn gate_binding_for(
        &self,
        run_id: &str,
        gate_name: &str,
    ) -> Result<Option<tj_config::capability::GateBindingKind>> {
        match self.run_policy(run_id)? {
            RunPolicy::V2(contract) => Ok(Some(contract.gate_binding(gate_name))),
            RunPolicy::V1(policy) => {
                let has_packet = self.db.packet_bundle(run_id)?.latest.is_some();
                Ok(policy.gate_binding(gate_name, has_packet))
            }
        }
    }

    /// Turn the newest stage artifact that carries a requirements `packet`
    /// object into an immutable packet revision. Source references are checked
    /// against durable artifacts/answers of this run, so a generation stage
    /// cannot invent evidence. Identical content is a no-op (retry safe).
    fn sync_generated_packet(
        &self,
        run_id: &str,
        flow_name: &str,
    ) -> Result<Option<tj_db::packets::PacketRevision>> {
        let artifacts = self.db.list_artifacts(run_id)?;
        let candidate = artifacts
            .into_iter()
            .filter(|artifact| artifact.flow_name == flow_name)
            .filter_map(|artifact| {
                let value: serde_json::Value = serde_json::from_str(&artifact.content).ok()?;
                let packet = value.get("packet")?.clone();
                Some((artifact, packet))
            })
            .max_by(|(left, _), (right, _)| {
                left.created_at
                    .cmp(&right.created_at)
                    .then(left.version.cmp(&right.version))
            });
        let Some((artifact, packet_value)) = candidate else {
            return Ok(None);
        };
        let packet_json = serde_json::to_string(&packet_value)?;
        // Validate before persisting for a precise error; the DB repeats the
        // full structural validation inside its transaction.
        let packet = tj_packet::RequirementsPacket::from_json(&packet_json)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let allowed = self.db.packet_allowed_sources(run_id)?;
        packet
            .validate_known_sources(&allowed)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let draft = tj_db::packets::NewPacketRevision {
            run_id: run_id.to_string(),
            packet_id: None,
            content_json: packet_json,
            parent_revision_id: None,
            source_artifact_id: Some(artifact.id.clone()),
            source_artifact_hash: Some(artifact.content_hash.clone()),
            flow_name: Some(artifact.flow_name.clone()),
            stage_name: Some(artifact.stage_name.clone()),
            change_request: None,
            created_by: "system:packet-generation".to_string(),
            allowed_sources: Some(allowed),
        };
        let (revision, replayed) = self.db.insert_packet_revision(&draft)?;
        tracing::info!(
            run_id, packet_id = %revision.packet_id, revision = revision.revision,
            hash = %revision.content_hash, completeness = %revision.completeness, replayed,
            "Requirements packet revision recorded from stage output"
        );
        Ok(Some(revision))
    }

    /// Turn the newest planning stage artifact carrying a structured `plan`
    /// object into an immutable plan revision. The DB binds it to the exact
    /// approved packet revision; a malformed plan, unknown requirement,
    /// dependency cycle or mismatching packet claim fails before a gate opens.
    fn sync_generated_plan(
        &self,
        run_id: &str,
        flow_name: &str,
    ) -> Result<Option<tj_db::plans::PlanRevision>> {
        let approved = self
            .db
            .approved_packet_revision(run_id)?
            .context("Plan production requires the exact approved requirements packet revision")?;
        let artifacts = self.db.list_artifacts(run_id)?;
        let candidate = artifacts
            .into_iter()
            .filter(|artifact| artifact.flow_name == flow_name)
            .filter_map(|artifact| {
                let value: serde_json::Value = serde_json::from_str(&artifact.content).ok()?;
                let plan = value.get("plan")?.clone();
                Some((artifact, plan))
            })
            .max_by(|(left, _), (right, _)| {
                left.created_at
                    .cmp(&right.created_at)
                    .then(left.version.cmp(&right.version))
            });
        let Some((artifact, plan_value)) = candidate else {
            return Ok(None);
        };
        let plan_json = serde_json::to_string(&plan_value)?;
        let draft = tj_db::plans::NewPlanRevision {
            run_id: run_id.to_string(),
            plan_id: None,
            content_json: plan_json,
            parent_revision_id: None,
            source_packet_revision_id: Some(approved.id.clone()),
            source_artifact_id: Some(artifact.id.clone()),
            source_artifact_hash: Some(artifact.content_hash.clone()),
            flow_name: Some(artifact.flow_name.clone()),
            stage_name: Some(artifact.stage_name.clone()),
            change_request: None,
            created_by: "system:plan-generation".to_string(),
        };
        let (revision, replayed) = self.db.insert_plan_revision(&draft)?;
        tracing::info!(
            run_id, plan_id = %revision.plan_id, revision = revision.revision,
            hash = %revision.content_hash, packet_revision = %revision.packet_revision_id,
            replayed, "Structured task plan revision recorded from stage output"
        );
        Ok(Some(revision))
    }

    /// Copy user-supplied local input documents from `<project>/inputs/` into
    /// the isolated run workspace. Only regular files are copied (no symlinks,
    /// directories or path traversal), and existing run files are never
    /// overwritten. Research stages can then read `inputs/` with the ordinary
    /// confined read tool.
    fn sync_project_inputs(&self, run_id: &str, workspace: Option<&std::path::Path>) {
        let Some(workspace) = workspace else { return };
        let project = self
            .db
            .load_run(run_id)
            .ok()
            .flatten()
            .and_then(|run| run.project_id().map(|id| id.as_str().to_string()))
            .and_then(|id| self.db.load_project(&id).ok().flatten());
        let Some(project) = project else { return };
        let Some(project_ws) = project.workspace_path.as_deref() else {
            return;
        };
        let source = std::path::Path::new(project_ws).join("inputs");
        let Ok(entries) = std::fs::read_dir(&source) else {
            return;
        };
        let dest = workspace.join("inputs");
        if let Err(error) = std::fs::create_dir_all(&dest) {
            tracing::warn!(run_id, %error, "Cannot create run input directory");
            return;
        }
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_file() {
                continue;
            }
            let name = entry.file_name();
            let target = dest.join(&name);
            if target.exists() {
                continue;
            }
            match std::fs::copy(entry.path(), &target) {
                Ok(bytes) => {
                    tracing::debug!(run_id, file = %name.to_string_lossy(), bytes, "Copied local input document")
                }
                Err(error) => {
                    tracing::warn!(run_id, file = %name.to_string_lossy(), %error, "Failed to copy local input document")
                }
            }
        }
    }

    fn resolve_workspace_path(&self, run_id: &str) -> Option<String> {
        let project: Option<tj_db::Project> = (|| {
            let run = self.db.load_run(run_id).ok()??;
            let project_id = run.project_id()?;
            self.db.load_project(project_id.as_str()).ok()?
        })();
        if let Some(project) = project {
            // Repo-backed projects get per-run git worktrees, so the project
            // workspace is only their base. No-repo projects would otherwise
            // share one workspace across concurrent runs — give each run its own.
            let ws = match project.workspace_path {
                Some(path) if project.repo_url.is_some() => path,
                Some(path) => format!("{}/runs/{}", path.trim_end_matches('/'), run_id),
                None => format!("{}/{}", self.workspace_base.trim_end_matches('/'), run_id),
            };
            if std::fs::create_dir_all(&ws).is_ok() {
                return Some(ws);
            }
            tracing::warn!(run_id = %run_id, workspace = %ws, "Project workspace unavailable, falling back to per-run workspace");
        }
        fallback_run_workspace(&self.workspace_base, run_id)
    }

    async fn get_git_provider(&mut self, run: &tj_db::PipelineRun) -> Option<Arc<dyn GitProvider>> {
        let project_id = match run.project_id() {
            Some(id) => id.to_string(),
            None => return None,
        };

        // Cache hit
        if let Some(provider) = self.git_providers.get(&project_id) {
            tracing::debug!(project_id = %project_id, "Git provider cache hit");
            return Some(Arc::clone(provider));
        }

        // Cache miss -- load from DB (wrap in spawn_blocking)
        let db = Arc::clone(&self.db);
        let pid = project_id.clone();
        let project = match tokio::task::spawn_blocking(move || db.load_project(&pid)).await {
            Ok(Ok(Some(p))) => p,
            Ok(Ok(None)) => {
                tracing::debug!(project_id = %project_id, "Project not found in DB");
                return None;
            }
            Ok(Err(e)) => {
                tracing::error!(project_id = %project_id, error = %e, "Failed to load project from DB");
                return None;
            }
            Err(e) => {
                tracing::error!(project_id = %project_id, error = %e, "spawn_blocking panicked in load_project");
                return None;
            }
        };

        let config_json = match project.git_provider {
            Some(ref json) => json.clone(),
            None => {
                if !self.git_provider_warned.contains(&project_id) {
                    tracing::debug!(project_id = %project_id, "No git_provider config for project");
                    self.git_provider_warned.insert(project_id.clone());
                }
                return None;
            }
        };

        let config: tj_git::GitProviderConfig = match serde_json::from_str(&config_json) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(project_id = %project_id, error = %e, "Failed to parse git_provider config");
                return None;
            }
        };

        let secrets_store = match self.secrets_store.as_ref() {
            Some(ss) => ss,
            None => {
                if !self.git_provider_warned.contains(&project_id) {
                    tracing::warn!(project_id = %project_id, "Secrets store not available, cannot construct git provider");
                    self.git_provider_warned.insert(project_id.clone());
                }
                return None;
            }
        };

        // Construct provider via factory
        match tj_git::create_git_provider(&config, secrets_store.as_ref(), &project_id).await {
            Ok(provider) => {
                let arc = Arc::from(provider);
                tracing::debug!(project_id = %project_id, "Git provider constructed and cached");
                self.git_providers.insert(project_id, Arc::clone(&arc));
                Some(arc)
            }
            Err(e) => {
                tracing::error!(project_id = %project_id, error = %e, "Failed to construct git provider");
                None // NOT cached -- retry on next call
            }
        }
    }

    fn invalidate_project_provider(&mut self, project_id: &str) {
        let removed_provider = self.git_providers.remove(project_id);
        let removed_warned = self.git_provider_warned.remove(project_id);
        tracing::debug!(
            project_id = %project_id,
            had_provider = removed_provider.is_some(),
            had_warning = removed_warned,
            "Invalidated project provider cache"
        );
    }

    async fn resolve_ssh_guard(&self, run_id: &str) -> Result<Option<SshKeyGuard>> {
        let run = self.db.load_run(run_id)?.context("run does not exist")?;
        let repo_url = run.repo_url().map(str::to_string).or_else(|| {
            run.project_id()
                .and_then(|pid| self.db.load_project(pid.as_str()).ok().flatten())
                .and_then(|project| project.repo_url)
        });
        let Some(repo_url) = repo_url else {
            return Ok(None);
        };
        if !crate::ssh_guard::is_ssh_remote(&repo_url) {
            return Ok(None);
        }
        let project_id = run
            .project_id()
            .map(|p| p.as_str().to_string())
            .context("SSH-backed run has no project identity")?;
        let secrets_store = self
            .secrets_store
            .as_ref()
            .context("SSH-backed repository requires the secrets store, which is not configured")?;
        let home = self
            .worktree_manager
            .as_ref()
            .map(|wm| wm.worktrees_base().clone())
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let guard = SshKeyGuard::new(secrets_store.as_ref(), &project_id, &home)
            .await?
            .context(
                "SSH-backed repository requires a project key; generate one before cloning/pushing",
            )?;
        tracing::debug!(run_id, project_id = %project_id, "SSH key guard created with verified host identity");
        Ok(Some(guard))
    }

    fn collect_artifacts(&self, run_id: &str) -> HashMap<String, serde_json::Value> {
        let mut artifacts = HashMap::new();
        // Immutable artifact store wins per stage; inline stage output fills the gaps
        // (pre-store legacy runs, or stages whose store write warn-failed).
        if let Ok(rows) = self.db.list_artifacts(run_id) {
            for a in rows {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&a.content) {
                    artifacts.insert(a.stage_name.clone(), parsed);
                }
            }
        }
        let flows = match self.db.load_flows_for_run(run_id) {
            Ok(f) => f,
            Err(_) => return artifacts,
        };
        for flow in flows {
            let stages = match self.db.load_stages_for_flow(flow.id()) {
                Ok(s) => s,
                Err(_) => continue,
            };
            for stage in stages {
                if let Some(output) = stage.output_artifacts()
                    && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output)
                {
                    artifacts.insert(stage.stage_name().to_string(), parsed);
                }
            }
        }
        // Plan stages receive the exact approved packet identity/content
        // the runtime accepted, not a mutable "latest packet" lookup.
        if let Ok(Some(packet)) = self.db.approved_packet_revision(run_id)
            && let Ok(content) = serde_json::from_str::<serde_json::Value>(&packet.content_json)
        {
            artifacts.insert(
                "__approved_packet".to_string(),
                serde_json::json!({
                    "packet_id": packet.packet_id,
                    "revision_id": packet.id,
                    "revision": packet.revision,
                    "content_hash": packet.content_hash,
                    "completeness": packet.completeness,
                    "approved": true,
                    "packet": content,
                }),
            );
        }
        artifacts
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<ManagerEvent>) {
        tracing::info!("Manager actor started");

        while let Some(event) = rx.recv().await {
            let shutting_down = matches!(event, ManagerEvent::Shutdown { .. });
            tracing::debug!(event = ?event, "Manager received event");
            self.handle(event).await;
            if shutting_down {
                break;
            }
        }

        tracing::info!("Manager actor stopped");
    }

    async fn handle(&mut self, event: ManagerEvent) {
        match event {
            ManagerEvent::RunRequested { run_id, trace_id } => {
                tracing::debug!(trace_id = %trace_id, run_id = %run_id, "Manager received RunRequested");
                self.handle_run_requested(&run_id, &trace_id).await;
            }
            ManagerEvent::ReconcilePendingEffects { run_id } => {
                self.handle_reconcile_pending_effects(&run_id).await;
            }
            ManagerEvent::RetryRequested {
                run_id,
                flow_name,
                attempt_offset,
                trace_id,
            } => {
                self.handle_retry_requested(&run_id, &flow_name, attempt_offset, &trace_id)
                    .await;
            }
            ManagerEvent::FlowDone {
                run_id,
                flow_name,
                outcome,
                trace_id,
                attempt_id,
            } => {
                match self
                    .db
                    .accept_flow_completion(&run_id, &flow_name, &attempt_id)
                {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::debug!(
                            run_id,
                            attempt_id,
                            "Ignoring stale or duplicate flow completion"
                        );
                        return;
                    }
                    Err(error) => {
                        tracing::error!(run_id, %error, "Cannot persist flow completion; refusing to advance");
                        return;
                    }
                }
                if let Err(error) = self.stop_execution(&run_id, false).await {
                    tracing::error!(run_id, %error, "Execution cleanup failed; refusing completion effects");
                    if let Ok(Some(effect)) = self
                        .db
                        .pending_effect_by_key(&format!("flow_completion:{attempt_id}"))
                    {
                        let _ = self.db.mark_pending_effect_ambiguous(&effect.id,
                            "completion accepted but execution shutdown did not complete; routing/publication unproven");
                    }
                    if self.db.has_run_budget(&run_id).unwrap_or(false) {
                        let _ = self.db.reconcile_budget_intervals(
                            &run_id,
                            chrono::Utc::now(),
                            "execution shutdown unconfirmed; active interval fenced",
                        );
                    }
                    self.update_run_status(&run_id, RunStatus::Failed);
                    return;
                }
                if self.db.has_run_budget(&run_id).unwrap_or(false)
                    && let Err(error) = self.db.finish_budget_interval(&run_id, &attempt_id)
                {
                    tracing::warn!(run_id, attempt_id, %error, "Could not close budget interval");
                }
                tracing::debug!(trace_id = %trace_id, run_id = %run_id, "Manager received FlowDone");
                self.handle_flow_done(&run_id, &flow_name, outcome, &trace_id, &attempt_id)
                    .await;
                self.settle_flow_completion_effect(&run_id, &attempt_id)
                    .await;
            }
            ManagerEvent::GateReached {
                run_id,
                flow_name,
                summary,
                trace_id,
                ..
            } => {
                tracing::debug!(trace_id = %trace_id, run_id = %run_id, "Manager received GateReached");
                if !self.handle_gate_reached(&run_id, &flow_name, &summary) {
                    return;
                }
                let _ = self.db.finalize_stale_waiting_stages(
                    &run_id,
                    "run moved on while the stage was waiting for input",
                );
                let _ = self.db.finalize_stale_flow_rows(&run_id);
            }
            ManagerEvent::ResumeFlow {
                run_id,
                completed_stage_id,
                trace_id,
            } => {
                tracing::debug!(trace_id = %trace_id, run_id = %run_id, "Manager received ResumeFlow");
                self.handle_resume_flow(&run_id, &completed_stage_id).await;
            }
            ManagerEvent::GateDecision {
                decision,
                expected_state_version,
                reply,
            } => {
                tracing::debug!(run_id = %decision.run_id.as_str(), gate = ?decision.gate_instance_id, "Manager received GateDecision");
                let result = self
                    .handle_gate_decision(decision, expected_state_version)
                    .await
                    .map_err(|error| error.to_string());
                let _ = reply.send(result);
            }
            ManagerEvent::TaskSelected {
                run_id,
                selection_id,
            } => {
                self.handle_task_selected(&run_id, &selection_id).await;
            }
            ManagerEvent::RunDeleted { run_id } => {
                if let Err(error) = self.stop_execution(&run_id, true).await {
                    tracing::error!(run_id, %error, "Deleted run still has active work; preserving workspace");
                    return;
                }
                self.smg_cache
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&run_id);
                self.worktrees.remove(&run_id);
                self.cancel_watchdog(&run_id);
            }
            ManagerEvent::CancelRun { run_id, reply } => {
                let result = self
                    .handle_cancel_run(&run_id)
                    .await
                    .map_err(|error| error.to_string());
                let _ = reply.send(result);
            }
            ManagerEvent::Shutdown { reply } => {
                let runs: Vec<_> = self.executions.keys().cloned().collect();
                let mut errors = Vec::new();
                for run_id in runs {
                    if let Err(error) = self.handle_cancel_run(&run_id).await {
                        errors.push(format!("{run_id}: {error}"));
                    }
                }
                for (_, watchdog) in self.watchdogs.drain() {
                    watchdog.task.abort();
                }
                let result = if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors.join("; "))
                };
                let _ = reply.send(result);
            }
            ManagerEvent::WatchdogTick { run_id, generation } => {
                self.handle_watchdog_tick(&run_id, &generation).await;
            }
            ManagerEvent::UserInput { message } => {
                tracing::info!(message = %message, "User input received");
            }
            ManagerEvent::ProjectDeleted { project_id } => {
                tracing::debug!(project_id = %project_id, "Manager received ProjectDeleted");
                self.invalidate_project_provider(&project_id);
            }
            ManagerEvent::SecretUpdated { project_id, key } => {
                tracing::debug!(project_id = %project_id, key = %key, "Manager received SecretUpdated");
                self.invalidate_project_provider(&project_id);
            }
        }
    }

    async fn handle_run_requested(&mut self, run_id: &str, trace_id: &str) {
        self.handle_run_requested_with(run_id, trace_id, None, 0)
            .await;
    }

    async fn handle_run_requested_with(
        &mut self,
        run_id: &str,
        trace_id: &str,
        flow_override: Option<&str>,
        attempt_offset: u32,
    ) {
        tracing::debug!(run_id, trace_id = %trace_id, "handle_run_requested entered");

        let mut run = match self.db.load_run(run_id) {
            Ok(Some(r)) if *r.status() == RunStatus::Running => {
                tracing::debug!(run_id, project_id = ?r.project_id(), lane_id = ?r.lane_id(), status = ?r.status(), "Run loaded from DB");
                r
            }
            Ok(Some(r)) => {
                tracing::debug!(run_id, status = ?r.status(), "Run found but not in Running status, skipping");
                return;
            }
            Ok(None) => {
                tracing::debug!(run_id, "Run not found in DB, skipping");
                return;
            }
            Err(e) => {
                tracing::debug!(run_id, error = %e, "Failed to load run from DB");
                return;
            }
        };

        let execution_smg = match self.smg_for_run(run_id) {
            Ok(smg) => smg,
            Err(error) => {
                self.handle_flow_failed(run_id, "workflow", &error.to_string())
                    .await;
                return;
            }
        };

        if self.executions.contains_key(run_id) {
            return;
        }
        // Cover setup failures too; successful spawn replaces this generation.
        self.start_watchdog(run_id, self.attempt_timeout_for_run(run_id));

        // Derive slug from description if not already set
        if run.slug().is_none() || run.slug() == Some("task") {
            let slug = derive_slug(run.description());
            tracing::debug!(run_id, slug = %slug, "Derived slug from description");
            run.set_slug(Some(slug));
            if let Err(e) = self.db.save_run(&run) {
                tracing::error!(run_id, error = %e, "Failed to save run slug");
                return;
            }
        }

        let slug = run.slug().unwrap_or("task").to_string();
        let base_branch = run.base_branch().unwrap_or("main").to_string();

        tracing::debug!(run_id, has_repo_url = run.repo_url().is_some(), base_branch = %base_branch, slug = %slug, "Run details resolved");

        // Notify task started before worktree creation so the user always gets this notification
        let branch_name = format!("tj/{}-{}", run_id, slug);
        if let Some(ref notifier) = self.notifier {
            notifier
                .notify_task_started(run_id, run.description(), &branch_name)
                .await;
        }

        // Try to reconstruct existing worktree from DB flow records before creating a new one
        let fallback_workspace = self.resolve_workspace_path(run_id).map(PathBuf::from);
        let fallback_branch = run.base_branch().map(|s| s.to_string());
        let repository_free = match self.run_workspace_is_repository(run_id) {
            Ok(repository) => !repository,
            Err(error) => {
                self.handle_flow_failed(
                    run_id,
                    "workspace",
                    &format!("cannot resolve the frozen workspace contract: {error:#}"),
                )
                .await;
                return;
            }
        };
        let (workspace_path, branch_name) = if repository_free {
            // A workflow with no repository capability never creates a git
            // worktree or invokes a forge/SSH helper. Each run gets an isolated
            // artifact workspace.
            (fallback_workspace, fallback_branch)
        } else if self.worktree_manager.is_some() {
            let existing_wt = self.reconstruct_existing_worktree(run_id);
            if let Some(wt) = existing_wt {
                tracing::info!(run_id, "Reconstructed existing worktree on restart");
                let ws = Some(wt.worktree_path.clone());
                let br = Some(wt.branch_name.clone());
                self.worktrees.insert(run_id.to_string(), wt);
                (ws, br)
            } else {
                let wt_result = self
                    .create_worktree_for_run(run_id, &run, &slug, &base_branch)
                    .await;
                match wt_result {
                    Some((ws, br)) => (ws, br),
                    None => {
                        // create_worktree_for_run returns None in two cases:
                        // 1. Benign: no repo_url/project_id (run still Running) — use fallback
                        // 2. Failure: worktree creation failed (run already marked Failed) — abort
                        let current_status = self
                            .db
                            .load_run(run_id)
                            .ok()
                            .flatten()
                            .map(|r| r.status().clone());
                        if current_status == Some(RunStatus::Failed) {
                            tracing::warn!(run_id = %run_id, "Worktree creation failed, aborting pipeline");
                            return;
                        }
                        (fallback_workspace, fallback_branch)
                    }
                }
            }
        } else {
            tracing::debug!(
                run_id,
                "No WorktreeManager available, using project workspace path"
            );
            (fallback_workspace, fallback_branch)
        };

        match self.run_policy(run_id) {
            Ok(RunPolicy::V2(contract)) => {
                if let Some(destination) = contract.input_destination() {
                    let Some(workspace) = workspace_path.as_deref() else {
                        self.handle_flow_failed(
                            run_id,
                            "inputs",
                            "workflow declares inputs but the run has no workspace",
                        )
                        .await;
                        return;
                    };
                    if let Err(error) = crate::capabilities::inputs::copy_project_inputs(
                        &self.db,
                        run_id,
                        workspace,
                        destination,
                    ) {
                        self.handle_flow_failed(
                            run_id,
                            "inputs",
                            &format!("declared project input copy failed: {error:#}"),
                        )
                        .await;
                        return;
                    }
                }
            }
            Ok(RunPolicy::V1(policy)) if policy.workspace_is_repository_free() => {
                // Frozen v1 only: historical repository-free profiles copied
                // project inputs implicitly. New workflows declare `inputs`.
                self.sync_project_inputs(run_id, workspace_path.as_deref());
            }
            Ok(RunPolicy::V1(_)) => {}
            Err(error) => {
                self.handle_flow_failed(
                    run_id,
                    "inputs",
                    &format!("cannot resolve the frozen execution policy: {error:#}"),
                )
                .await;
                return;
            }
        }

        let (flow_name, artifacts) = match flow_override {
            Some(flow) => {
                let arts = self.collect_artifacts(run_id);
                tracing::info!(run_id, flow = %flow, artifact_count = arts.len(), "Explicit retry from durable flow");
                (flow.to_string(), arts)
            }
            None => match self.find_current_flow(run_id) {
                Some(flow) => {
                    let arts = self.collect_artifacts(run_id);
                    tracing::info!(run_id, flow = %flow, artifact_count = arts.len(), "Resuming from existing flow");
                    (flow, arts)
                }
                None => {
                    tracing::info!(run_id, entry_flow = %execution_smg.entry, "Starting from entry flow");
                    (execution_smg.entry.clone(), HashMap::new())
                }
            },
        };

        tracing::debug!(run_id, flow = %flow_name, "About to spawn initial flow");

        self.spawn_flow(SpawnFlowParams {
            run_id: run_id.to_string(),
            flow_name,
            description: run.description().to_string(),
            existing_artifacts: artifacts,
            workspace_path,
            branch_name,
            slug,
            attempt_offset,
        })
        .await;
    }

    async fn handle_reconcile_pending_effects(&mut self, run_id: &str) {
        let effects = match self.db.list_pending_effects(run_id, true) {
            Ok(effects) => effects,
            Err(error) => {
                tracing::error!(run_id, %error, "Cannot inspect durable effects for recovery");
                let _ = self.db.mark_run_interrupted(
                    run_id,
                    "effect inspection failed during startup",
                    true,
                );
                return;
            }
        };
        if effects.is_empty() {
            return;
        }
        for effect in effects.iter().filter(|effect| effect.kind == "gate_route") {
            let payload: serde_json::Value = effect
                .payload
                .as_deref()
                .and_then(|payload| serde_json::from_str(payload).ok())
                .unwrap_or_default();
            let decision: tj_db::GateDecision = match serde_json::from_value(
                payload.get("decision").cloned().unwrap_or_default(),
            ) {
                Ok(decision) => decision,
                Err(error) => {
                    tracing::error!(run_id, effect_id = %effect.id, %error, "Gate routing effect has an unreadable decision");
                    let _ = self.db.mark_pending_effect_ambiguous(
                        &effect.id,
                        "unreadable gate decision payload",
                    );
                    continue;
                }
            };
            let expected = decision.expected_state_version.unwrap_or(0);
            self.emit(
                run_id,
                "effect_reconciled",
                None,
                Some("resuming durable gate route"),
            );
            if let Err(error) = self.handle_gate_decision(decision, expected).await {
                tracing::error!(run_id, effect_id = %effect.id, %error, "Durable gate route reconciliation failed");
                let _ = self
                    .db
                    .mark_pending_effect_ambiguous(&effect.id, &error.to_string());
            }
        }

        // External/completion effects are never replayed automatically.
        let unresolved = self
            .db
            .list_pending_effects(run_id, true)
            .unwrap_or_default();
        let mut uncertain = false;
        for effect in unresolved {
            if effect.kind != "gate_route" {
                uncertain = true;
                let _ = self.db.mark_pending_effect_ambiguous(
                    &effect.id,
                    "process restarted after durable acceptance; external effect is unknown",
                );
                self.emit(
                    run_id,
                    "effect_reconciliation_required",
                    None,
                    Some(&format!(
                        "{} effect requires explicit reconciliation",
                        effect.kind
                    )),
                );
            } else if effect.status == tj_db::EffectStatus::Ambiguous {
                uncertain = true;
            }
        }
        let still_running = self
            .db
            .load_run(run_id)
            .ok()
            .flatten()
            .map(|run| *run.status() == RunStatus::Running)
            .unwrap_or(false);
        if uncertain && still_running {
            let _ = self.db.mark_run_interrupted(
                run_id,
                "durable effects could not be reconciled automatically",
                true,
            );
        } else if still_running && !self.executions.contains_key(run_id) {
            // The effect was already consumed by a dead process (a durable
            // attempt exists) or no route started a worker. Never leave a
            // running row that no process is executing.
            let _ = self.db.mark_run_interrupted(
                run_id,
                "reconciled effect has no live execution worker; explicit retry required",
                true,
            );
        }
    }

    async fn handle_retry_requested(
        &mut self,
        run_id: &str,
        flow_name: &str,
        attempt_offset: u32,
        trace_id: &str,
    ) {
        if self.executions.contains_key(run_id) {
            tracing::warn!(run_id, "Retry ignored: an execution is already active");
            return;
        }
        tracing::info!(
            run_id,
            flow = flow_name,
            attempt_offset,
            "Starting explicit retry attempt"
        );
        self.emit(
            run_id,
            "flow_started",
            Some(flow_name),
            Some("explicit retry"),
        );
        self.handle_run_requested_with(run_id, trace_id, Some(flow_name), attempt_offset)
            .await;
    }

    async fn handle_task_selected(&mut self, run_id: &str, selection_id: &str) {
        if self.executions.contains_key(run_id) {
            tracing::warn!(
                run_id,
                "Task selection ignored: an execution is already active"
            );
            return;
        }
        let policy = match self.run_policy(run_id) {
            Ok(policy) => policy,
            Err(error) => {
                tracing::error!(run_id, %error, "Cannot resolve execution policy for task selection");
                return;
            }
        };
        let (expected_state, target_flow) = match &policy {
            RunPolicy::V2(contract) => {
                let Some(target) = contract.selection_target_flow() else {
                    self.emit(
                        run_id,
                        "task_selection_rejected",
                        None,
                        Some("workflow does not declare a task-selection target flow"),
                    );
                    return;
                };
                ("task_selection_required".to_string(), target.to_string())
            }
            RunPolicy::V1(legacy) => (
                legacy.task_selection_gate_state().to_string(),
                legacy.task_selection_flow().to_string(),
            ),
        };
        let mut run = match self.db.load_run(run_id) {
            Ok(Some(run)) => run,
            _ => return,
        };
        if *run.status() != RunStatus::WaitingForGate
            || run.gate_state() != Some(expected_state.as_str())
        {
            tracing::debug!(run_id, status = %run.status(), "Task selection ignored: run is not waiting for task selection");
            return;
        }
        let selection = match self.db.plan_task_selections(run_id) {
            Ok(selections) => selections
                .into_iter()
                .find(|selection| selection.id == selection_id),
            Err(error) => {
                tracing::error!(run_id, %error, "Cannot load task selection");
                return;
            }
        };
        let Some(selection) = selection else {
            tracing::error!(
                run_id,
                selection_id,
                "Selected task identity no longer exists"
            );
            return;
        };
        match crate::capabilities::plan::valid_task_selection(&self.db, run_id) {
            Ok(Some(valid)) if valid.id == selection.id => {}
            Ok(_) => {
                self.emit(run_id, "task_selection_rejected", None,
                    Some("selection does not match the approved plan revision and ready task identity"));
                return;
            }
            Err(error) => {
                self.emit(
                    run_id,
                    "task_selection_rejected",
                    None,
                    Some(&error.to_string()),
                );
                return;
            }
        }
        run.set_status(RunStatus::Running);
        run.set_gate_state(None);
        run.set_updated_at(chrono::Utc::now().to_rfc3339());
        if let Err(error) = self.db.save_run(&run) {
            tracing::error!(run_id, %error, "Cannot release run after task selection");
            return;
        }
        self.emit(
            run_id,
            "task_selected",
            None,
            Some(&format!(
                "task {} ({}) released to '{}'",
                selection.task_id, selection.id, target_flow
            )),
        );
        let artifacts = self.collect_artifacts(run_id);
        // The selected flow must run in the run's existing task worktree.
        // Falling back to the project's base clone would let the model write
        // outside the branch that publication commits/pushes (and the base
        // SHA/scope evidence would describe a different tree).
        let wt_info = self.worktrees.get(run_id).cloned();
        let workspace_path = wt_info
            .as_ref()
            .map(|wt| wt.worktree_path.clone())
            .or_else(|| self.resolve_workspace_path(run_id).map(PathBuf::from));
        let branch_name = wt_info
            .as_ref()
            .map(|wt| wt.branch_name.clone())
            .or_else(|| run.base_branch().map(str::to_string));
        self.spawn_flow(SpawnFlowParams {
            run_id: run_id.to_string(),
            flow_name: target_flow,
            description: run.description().to_string(),
            existing_artifacts: artifacts,
            workspace_path,
            branch_name,
            slug: run.slug().unwrap_or("task").to_string(),
            attempt_offset: 0,
        })
        .await;
    }

    async fn handle_flow_done(
        &mut self,
        run_id: &str,
        flow_name: &str,
        outcome: FlowOutcome,
        _trace_id: &str,
        attempt_id: &str,
    ) {
        match outcome {
            FlowOutcome::Completed { artifacts, .. } => {
                self.emit(run_id, "flow_completed", Some(flow_name), None);
                self.handle_flow_completed(run_id, flow_name, artifacts, attempt_id)
                    .await;
            }
            FlowOutcome::WaitingForInput {
                stage_name,
                message,
                ..
            } => {
                self.handle_flow_waiting_for_input(run_id, flow_name, &stage_name, &message)
                    .await;
            }
            FlowOutcome::Failed { error, .. } => {
                self.handle_flow_failed(run_id, flow_name, &error).await;
            }
            FlowOutcome::TimedOut { stage_name, .. } => {
                self.handle_flow_timed_out(run_id, flow_name, &stage_name)
                    .await;
            }
        }
    }

    async fn handle_flow_completed(
        &mut self,
        run_id: &str,
        flow_name: &str,
        artifacts: HashMap<String, serde_json::Value>,
        attempt_id: &str,
    ) {
        tracing::info!(run_id = %run_id, flow = %flow_name, attempt = %attempt_id, "Flow completed");
        self.cancel_watchdog(run_id);

        let policy = match self.run_policy(run_id) {
            Ok(policy) => policy,
            Err(error) => {
                self.handle_flow_failed(
                    run_id,
                    flow_name,
                    &format!("cannot resolve the frozen execution policy: {error:#}"),
                )
                .await;
                return;
            }
        };
        let repository_free = match &policy {
            RunPolicy::V2(contract) => {
                contract.workspace_mode() == tj_config::capability::WorkspaceMode::None
            }
            RunPolicy::V1(legacy) => legacy.workspace_is_repository_free(),
        };
        let legacy_policy = match &policy {
            RunPolicy::V1(legacy) => Some(*legacy),
            RunPolicy::V2(_) => None,
        };
        let mut run = match self.db.load_run(run_id) {
            Ok(Some(r)) => r,
            _ => return,
        };

        // Declared capability dispatch. v2 reads only its frozen contract; the
        // isolated v1 adapter keeps its historical behavior.
        let completion = match &policy {
            RunPolicy::V2(contract) => Some(CapabilityRuntime::new(contract).after_flow(flow_name)),
            RunPolicy::V1(_) => None,
        };
        if let Some(verification) = completion.as_ref().and_then(|flow| flow.verification) {
            let workspace = self
                .worktrees
                .get(run_id)
                .map(|wt| wt.worktree_path.clone())
                .or_else(|| self.resolve_workspace_path(run_id).map(PathBuf::from));
            let Some(workspace) = workspace else {
                self.handle_flow_failed(
                    run_id,
                    flow_name,
                    "citation verification failed: the run has no accessible workspace",
                )
                .await;
                return;
            };
            if let Err(error) = crate::capabilities::verification::validate_local_citations(
                verification,
                &workspace,
                &artifacts,
            ) {
                self.handle_flow_failed(
                    run_id,
                    flow_name,
                    &format!("citation verification failed: {error:#}"),
                )
                .await;
                return;
            }
        }

        if legacy_policy.is_some_and(|policy| policy.verifies_local_citations(flow_name)) {
            let workspace = self
                .worktrees
                .get(run_id)
                .map(|wt| wt.worktree_path.clone())
                .or_else(|| self.resolve_workspace_path(run_id).map(PathBuf::from));
            let Some(workspace) = workspace else {
                self.handle_flow_failed(
                    run_id,
                    flow_name,
                    "citation verification failed: the run has no accessible workspace",
                )
                .await;
                return;
            };
            if let Err(error) =
                crate::legacy_v1::validate_research_citations(&workspace, &artifacts)
            {
                self.handle_flow_failed(
                    run_id,
                    flow_name,
                    &format!("citation verification failed: {error:#}"),
                )
                .await;
                return;
            }
        }

        let publishes = match &policy {
            RunPolicy::V2(_) => completion
                .as_ref()
                .is_some_and(|flow| flow.publication.is_some()),
            RunPolicy::V1(legacy) => legacy.publishes_on_flow(flow_name),
        };

        if repository_free {
            tracing::debug!(
                run_id,
                "Workflow declares no repository workspace: skipping commit/push/PR operations"
            );
        } else if !publishes {
            tracing::debug!(
                run_id,
                flow = %flow_name,
                "Flow has no declared publication capability: no repository publication occurs"
            );
        } else {
            let Some(worktree) = self.worktrees.get(run_id).cloned() else {
                self.handle_flow_failed(run_id, flow_name,
                    "publication flow completed without a task worktree; refusing to claim publication success").await;
                return;
            };
            let ssh_guard = match self.resolve_ssh_guard(run_id).await {
                Ok(guard) => guard,
                Err(error) => {
                    self.handle_flow_failed(
                        run_id,
                        flow_name,
                        &format!("repository publication credentials are invalid: {error:#}"),
                    )
                    .await;
                    return;
                }
            };
            let base_branch = run.base_branch().unwrap_or("main").to_string();
            let workspace = worktree.worktree_path.clone();
            let base_branch_for_task = base_branch.clone();
            let base_sha = match tokio::task::spawn_blocking(move || {
                WorktreeManager::resolve_base_sha(&workspace, &base_branch_for_task)
            })
            .await
            .context("base SHA task panicked")
            .and_then(|result| result)
            {
                Ok(sha) => sha,
                Err(error) => {
                    self.handle_flow_failed(
                        run_id,
                        flow_name,
                        &format!("cannot establish the approved task's base commit: {error}"),
                    )
                    .await;
                    return;
                }
            };
            let workspace = worktree.worktree_path.clone();
            let base_for_diff = base_sha.clone();
            let changed_files = match tokio::task::spawn_blocking(move || {
                WorktreeManager::changed_files(&workspace, &base_for_diff)
            })
            .await
            .context("changed-file task panicked")
            .and_then(|result| result)
            {
                Ok(files) => files,
                Err(error) => {
                    self.handle_flow_failed(
                        run_id,
                        flow_name,
                        &format!("cannot determine the approved task's changed files: {error}"),
                    )
                    .await;
                    return;
                }
            };
            let publication_capability = match &policy {
                RunPolicy::V2(contract) => contract.publication_for(flow_name).cloned(),
                RunPolicy::V1(legacy) => Some(legacy.publication_capability(flow_name)),
            };
            let Some(publication_capability) = publication_capability else {
                self.handle_flow_failed(
                    run_id,
                    flow_name,
                    "publication dispatch has no declared evidence contract",
                )
                .await;
                return;
            };
            let evidence = match crate::publication::load_publication_evidence(
                &self.db,
                run_id,
                flow_name,
                &publication_capability,
                changed_files,
            ) {
                Ok(evidence) => evidence,
                Err(error) => {
                    self.handle_flow_failed(
                        run_id,
                        flow_name,
                        &format!("publication evidence contract failed: {error:#}"),
                    )
                    .await;
                    return;
                }
            };
            let provider = match self.get_git_provider(&run).await {
                Some(provider) => provider,
                None => {
                    self.handle_flow_failed(run_id, flow_name,
                        "Draft PR publication requires a configured git provider; workspace and local commit are retained").await;
                    return;
                }
            };
            let ssh_env = ssh_guard.as_ref().map(|guard| guard.ssh_env());
            let publish = crate::publication::publish_draft_pr(
                &self.db,
                Some(provider.as_ref()),
                crate::publication::PublishRequest {
                    run_id,
                    flow_name,
                    attempt_id,
                    workspace: &worktree.worktree_path,
                    branch: &worktree.branch_name,
                    base_branch: &base_branch,
                    base_sha: &base_sha,
                    evidence: &evidence,
                    ssh_env: ssh_env.as_ref(),
                },
            )
            .await;
            match publish {
                Ok(publication)
                    if publication.push_state
                        == tj_db::publications::PublicationState::Succeeded
                        && publication.pr_state
                            == tj_db::publications::PublicationState::Succeeded =>
                {
                    run.set_pr_url(publication.pr_url.clone());
                    run.set_pr_number(publication.pr_number);
                    if let Err(error) = self.db.save_run(&run) {
                        tracing::error!(run_id, %error, "Cannot persist draft PR identity; refusing to continue");
                        self.handle_flow_failed(
                            run_id,
                            flow_name,
                            "draft PR identity could not be persisted; workspace retained",
                        )
                        .await;
                        return;
                    }
                    self.emit(
                        run_id,
                        "draft_pr_published",
                        Some(flow_name),
                        Some(&format!(
                            "task {} pushed to {} and draft PR opened",
                            publication.task_id, publication.branch
                        )),
                    );
                    self.emit(run_id, "delivery_external_required", Some(flow_name), Some(
                        "Draft PR is durable. External CI/CD delivery is a separate explicit release request; no deployment, merge or release was started."
                    ));
                    if let Some(ref notifier) = self.notifier
                        && let Some(url) = publication.pr_url.as_deref()
                    {
                        notifier.notify_draft_pr(run_id, url).await;
                    }
                }
                Ok(_) => {
                    self.handle_flow_failed(
                        run_id,
                        flow_name,
                        "publication did not reach durable pushed-branch + draft-PR success",
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    self.handle_flow_failed(
                        run_id,
                        flow_name,
                        &format!("draft PR publication failed: {error:#}"),
                    )
                    .await;
                    return;
                }
            }
        }

        let artifacts_value = serde_json::to_value(&artifacts).unwrap_or_default();
        self.route_next_flow(run_id, flow_name, &run, &artifacts_value)
            .await;
    }

    async fn handle_flow_waiting_for_input(
        &mut self,
        run_id: &str,
        flow_name: &str,
        stage_name: &str,
        _message: &str,
    ) {
        // Live questions stay inside the executing stage. A returned wait has
        // no worker to consume the answer and must never be routed as success.
        self.handle_flow_failed(
            run_id,
            flow_name,
            &format!("Flow execution ended while stage '{stage_name}' still required input"),
        )
        .await;
    }

    async fn handle_flow_failed(&mut self, run_id: &str, flow_name: &str, error: &str) {
        if !self.update_run_status(run_id, RunStatus::Failed) {
            return;
        }
        self.emit(run_id, "flow_failed", Some(flow_name), Some(error));
        self.emit(run_id, "run_failed", Some(flow_name), Some(error));
        tracing::error!(run_id = %run_id, flow = %flow_name, error = %error, "Flow failed");
        self.cancel_watchdog(run_id);
        if let Some(ref notifier) = self.notifier {
            notifier
                .notify_error(run_id, &format!("{} flow", flow_name), error)
                .await;
            notifier.notify_cleanup(run_id).await;
        }

        // Cleanup worktree on failure
        self.cleanup_task_worktree(run_id).await;

        let user_message = format!(
            "Job Failed\n\nYour job has encountered an error and cannot continue.\n\nError: {}\n\nJob ID: `{}`\n\nFailed Phase: {}\n\nThis usually means there's a technical problem that needs to be resolved before the job can continue.",
            error, run_id, flow_name
        );

        let _ = self
            .pipeline_events
            .send(PipelineNotification::PipelineFailed {
                run_id: run_id.into(),
                reason: error.to_string(),
                user_message: Some(user_message),
            });
    }

    async fn handle_flow_timed_out(&mut self, run_id: &str, flow_name: &str, stage_name: &str) {
        tracing::error!(run_id = %run_id, flow = %flow_name, stage = %stage_name, "Flow timed out");
        self.emit(
            run_id,
            "flow_failed",
            Some(flow_name),
            Some(&format!("timeout at stage '{}'", stage_name)),
        );
        self.emit(
            run_id,
            "run_failed",
            Some(flow_name),
            Some(&format!("timeout at stage '{}'", stage_name)),
        );
        self.cancel_watchdog(run_id);
        if !self.update_run_status(run_id, RunStatus::Failed) {
            return;
        }
        if let Some(ref notifier) = self.notifier {
            notifier
                .notify_error(run_id, &format!("{} flow", flow_name), "Timeout exceeded")
                .await;
            notifier.notify_cleanup(run_id).await;
        }

        // Cleanup worktree on timeout
        self.cleanup_task_worktree(run_id).await;

        let user_message = format!(
            "Job Timed Out\n\nYour job has taken too long to complete and has been stopped.\n\nLast Active Stage: {}\n\nJob ID: `{}`\n\nThe job exceeded the maximum allowed time.",
            stage_name, run_id
        );

        let _ = self
            .pipeline_events
            .send(PipelineNotification::PipelineFailed {
                run_id: run_id.into(),
                reason: format!("Flow timed out at stage '{}'", stage_name),
                user_message: Some(user_message),
            });
    }

    async fn route_next_flow(
        &mut self,
        run_id: &str,
        flow_name: &str,
        run: &tj_db::PipelineRun,
        artifacts: &serde_json::Value,
    ) {
        let mut art_map: HashMap<String, serde_json::Value> = HashMap::new();
        if let serde_json::Value::Object(obj) = artifacts {
            for (k, v) in obj {
                art_map.insert(k.clone(), v.clone());
            }
        }

        // Use worktree info if available, otherwise fall back to project workspace
        let wt_info = self.worktrees.get(run_id).cloned();
        let workspace_path = wt_info
            .as_ref()
            .map(|wt| wt.worktree_path.clone())
            .or_else(|| self.resolve_workspace_path(run_id).map(PathBuf::from));
        let branch_name = wt_info
            .as_ref()
            .map(|wt| wt.branch_name.clone())
            .or_else(|| run.base_branch().map(|s| s.to_string()));

        let next = match self.next_flow(run_id, flow_name) {
            Ok(next) => next,
            Err(error) => {
                self.handle_flow_failed(run_id, flow_name, &error.to_string())
                    .await;
                return;
            }
        };
        match next {
            Some(next_flow) => {
                tracing::info!(run_id = %run_id, next_flow = %next_flow, "Routing to next flow");
                let _ = self
                    .pipeline_events
                    .send(PipelineNotification::FlowCompleted {
                        run_id: run_id.into(),
                        flow: flow_name.into(),
                        summary: format!(
                            "Flow '{}' completed, routing to '{}'",
                            flow_name, next_flow
                        ),
                    });

                // Notify flow completed
                if let Some(ref notifier) = self.notifier {
                    notifier
                        .notify_flow_completed(
                            run_id,
                            flow_name,
                            &format!("routing to {}", next_flow),
                        )
                        .await;
                }

                // Check if next flow is a gate type — pause pipeline for user approval
                let is_gate = match self.smg_for_run(run_id) {
                    Ok(smg) => smg
                        .flows
                        .get(&next_flow)
                        .is_some_and(|f| f.flow_type == FlowType::Gate),
                    Err(error) => {
                        self.handle_flow_failed(run_id, flow_name, &error.to_string())
                            .await;
                        return;
                    }
                };
                if is_gate {
                    let policy = match self.run_policy(run_id) {
                        Ok(policy) => policy,
                        Err(error) => {
                            self.handle_flow_failed(
                                run_id,
                                flow_name,
                                &format!("cannot resolve the frozen execution policy: {error:#}"),
                            )
                            .await;
                            return;
                        }
                    };
                    // Resolve the declared producers before the gate binds to
                    // exact packet/plan identity. v2 reads only its contract;
                    // v1 keeps its frozen behavior.
                    let (produce_packet, produce_plan, packet_producer, plan_producer) =
                        match &policy {
                            RunPolicy::V2(contract) => {
                                let gate = CapabilityRuntime::new(contract)
                                    .before_gate(&next_flow, flow_name);
                                (
                                    gate.produce_packet,
                                    gate.produce_plan,
                                    contract.packet_producer().cloned(),
                                    contract.plan_producer().cloned(),
                                )
                            }
                            RunPolicy::V1(legacy) => {
                                let packet_gate =
                                    self.gate_binding_for(run_id, &next_flow).ok().flatten()
                                        == Some(tj_config::capability::GateBindingKind::Packet);
                                (
                                    legacy.produces_packet(flow_name) && packet_gate,
                                    legacy.validates_plan(flow_name),
                                    None,
                                    None,
                                )
                            }
                        };

                    let mut plan = None;
                    if produce_packet {
                        let result = match &packet_producer {
                            Some(producer) => crate::capabilities::packet::record_generated_packet(
                                &self.db, run_id, producer,
                            )
                            .map(|revision| revision.is_some()),
                            None => self
                                .sync_generated_packet(run_id, flow_name)
                                .map(|revision| revision.is_some()),
                        };
                        match result {
                            Ok(true) => {}
                            Ok(false) => {
                                self.handle_flow_failed(run_id, flow_name,
                                    "declared packet producer stage produced no requirements packet; the human gate was not opened").await;
                                return;
                            }
                            Err(error) => {
                                self.handle_flow_failed(
                                    run_id,
                                    flow_name,
                                    &format!("requirements packet generation failed: {error:#}"),
                                )
                                .await;
                                return;
                            }
                        }
                    }
                    if produce_plan {
                        let result = match &plan_producer {
                            Some(producer) => crate::capabilities::plan::record_generated_plan(
                                &self.db, run_id, producer,
                            ),
                            None => self.sync_generated_plan(run_id, flow_name),
                        };
                        match result {
                            Ok(Some(plan_revision)) => plan = Some(plan_revision),
                            Ok(None) => {
                                self.handle_flow_failed(run_id, flow_name, "declared plan producer stage produced no structured plan; the human gate was not opened").await;
                                return;
                            }
                            Err(error) => {
                                self.handle_flow_failed(
                                    run_id,
                                    flow_name,
                                    &format!("deterministic plan validation failed: {error:#}"),
                                )
                                .await;
                                return;
                            }
                        }
                    }
                    tracing::info!(run_id = %run_id, flow = %next_flow, "Next flow is a gate type, pausing for approval");
                    self.cancel_watchdog(run_id);
                    let last_stage_summary = match self.db.list_events(run_id) {
                        Ok(events) => events
                            .into_iter()
                            .rev()
                            .find(|e| {
                                e.kind == "stage_completed"
                                    && e.flow_name.as_deref() == Some(flow_name)
                            })
                            .and_then(|e| e.summary),
                        Err(e) => {
                            tracing::warn!(run_id = %run_id, error = %e, "Failed to list events for gate summary");
                            None
                        }
                    };
                    let artifact_count = match self.db.count_artifact_stages(run_id, flow_name) {
                        Ok(n) => n,
                        Err(e) => {
                            tracing::warn!(run_id = %run_id, error = %e, "Failed to count artifacts for gate summary");
                            0
                        }
                    };
                    let summary = build_gate_summary(
                        flow_name,
                        &next_flow,
                        last_stage_summary,
                        artifact_count,
                    );
                    if !self.handle_gate_reached_with_plan(
                        run_id,
                        &next_flow,
                        &summary,
                        plan.as_ref(),
                    ) {
                        return;
                    }
                    if let Some(ref notifier) = self.notifier {
                        notifier
                            .notify_gate_reached(run_id, &next_flow, &summary)
                            .await;
                    }
                    return;
                }

                self.spawn_flow(SpawnFlowParams {
                    run_id: run_id.to_string(),
                    flow_name: next_flow,
                    description: run.description().to_string(),
                    existing_artifacts: art_map,
                    workspace_path: workspace_path.clone(),
                    branch_name: branch_name.clone(),
                    slug: run.slug().unwrap_or("task").to_string(),
                    attempt_offset: 0,
                })
                .await;
            }
            None => {
                tracing::info!(run_id = %run_id, "All flows completed");
                if !self.update_run_status(run_id, RunStatus::Completed) {
                    return;
                }
                self.emit(run_id, "run_completed", None, None);
                let _ = self
                    .pipeline_events
                    .send(PipelineNotification::PipelineCompleted {
                        run_id: run_id.into(),
                        summary: format!("All flows completed for run {}", run_id),
                    });

                // Cleanup worktree after all flows complete
                self.cleanup_task_worktree(run_id).await;
                if let Some(ref notifier) = self.notifier {
                    notifier.notify_cleanup(run_id).await;
                }
            }
        }
    }

    fn handle_gate_reached(&mut self, run_id: &str, gate_name: &str, summary: &str) -> bool {
        self.handle_gate_reached_with_plan(run_id, gate_name, summary, None)
    }

    fn handle_gate_reached_with_plan(
        &mut self,
        run_id: &str,
        gate_name: &str,
        summary: &str,
        plan: Option<&tj_db::plans::PlanRevision>,
    ) -> bool {
        let run = match self.db.load_run(run_id) {
            Ok(Some(run)) => run,
            Ok(None) => {
                tracing::error!(run_id = %run_id, "Cannot enter gate: run not found");
                return false;
            }
            Err(error) => {
                tracing::error!(run_id = %run_id, %error, "Cannot load run before gate entry");
                return false;
            }
        };
        let gate_instance_id = format!("gate_{}", uuid::Uuid::new_v4());
        if let Err(error) = self.run_policy(run_id) {
            tracing::error!(run_id = %run_id, gate = %gate_name, %error, "Cannot resolve gate execution policy");
            return false;
        }
        let binding_kind = match self.gate_binding_for(run_id, gate_name) {
            Ok(binding) => binding,
            Err(error) => {
                tracing::error!(run_id = %run_id, gate = %gate_name, %error, "Cannot resolve gate binding");
                return false;
            }
        };
        let packet = if binding_kind == Some(tj_config::capability::GateBindingKind::Packet) {
            match self.db.packet_bundle(run_id) {
                Ok(bundle) => bundle.latest,
                Err(error) => {
                    tracing::error!(run_id = %run_id, %error, "Cannot load requirements packet before gate entry");
                    return false;
                }
            }
        } else {
            None
        };
        let output_artifact = if binding_kind
            == Some(tj_config::capability::GateBindingKind::Artifact)
        {
            // Exactly one reviewed flow, and exactly one latest non-empty
            // immutable artifact from it. There is no cross-flow fallback: an
            // approval must name the output the reviewer could actually see.
            let reviewed_flow = match self.find_current_flow(run_id) {
                Some(flow) => flow,
                None => {
                    tracing::error!(run_id = %run_id, gate = %gate_name, "Generic gate has no completed reviewed flow");
                    self.emit(
                        run_id,
                        "output_missing",
                        Some(gate_name),
                        Some("Generic approval requires the exact produced output artifact of the reviewed flow"),
                    );
                    return false;
                }
            };
            let artifact = match self.db.latest_artifact_for_flow(run_id, &reviewed_flow) {
                Ok(artifact) => artifact,
                Err(error) => {
                    tracing::error!(run_id = %run_id, %reviewed_flow, %error, "Cannot load reviewed flow artifact");
                    return false;
                }
            };
            match artifact {
                Some(artifact) if !artifact.content.trim().is_empty() => Some(artifact),
                _ => {
                    tracing::error!(run_id = %run_id, gate = %gate_name, "Generic gate requires a non-empty durable output artifact");
                    self.emit(
                        run_id,
                        "output_missing",
                        Some(gate_name),
                        Some("Generic approval requires the exact produced output artifact"),
                    );
                    return false;
                }
            }
        } else {
            None
        };
        if binding_kind == Some(tj_config::capability::GateBindingKind::Packet) && packet.is_none()
        {
            tracing::error!(run_id = %run_id, gate = %gate_name, "Packet-bound gate requires a durable requirements packet");
            self.emit(
                run_id,
                "packet_missing",
                Some(gate_name),
                Some("Packet approval requires an exact requirements packet revision"),
            );
            return false;
        }
        let plan = match (&plan, binding_kind) {
            (Some(plan), _) => Some((*plan).clone()),
            (None, Some(tj_config::capability::GateBindingKind::Plan)) => {
                match self.db.approved_plan_revision(run_id) {
                    Ok(Some(plan)) => Some(plan),
                    Ok(None) => {
                        tracing::error!(run_id = %run_id, gate = %gate_name, "Plan-bound gate has no approved plan revision");
                        self.emit(
                            run_id,
                            "plan_missing",
                            Some(gate_name),
                            Some("Plan approval requires a deterministically validated plan revision"),
                        );
                        return false;
                    }
                    Err(error) => {
                        tracing::error!(run_id = %run_id, %error, "Cannot load plan before gate entry");
                        return false;
                    }
                }
            }
            (None, _) => None,
        };
        let legacy_policy = self.legacy_policy_for_run(run_id).ok().flatten();
        if legacy_policy.is_some_and(|policy| policy.requires_plan_at_gate(gate_name))
            && plan.is_none()
        {
            tracing::error!(run_id = %run_id, gate = %gate_name, "Plan gate requires a valid structured plan bound to the approved packet");
            self.emit(
                run_id,
                "plan_missing",
                Some(gate_name),
                Some("Plan approval requires a deterministically validated plan revision"),
            );
            return false;
        }
        if let Err(error) = self.db.enter_gate_bound_to_output(
            run_id,
            run.state_version(),
            &gate_instance_id,
            gate_name,
            summary,
            packet.as_ref(),
            plan.as_ref(),
            output_artifact.as_ref(),
        ) {
            tracing::error!(run_id = %run_id, gate = %gate_name, %error, "Failed to persist gate entry");
            return false;
        }
        // Gate review has no attempt watchdog. A configured absolute deadline
        // still applies and is re-armed with its original remaining duration.
        self.start_watchdog(run_id, None);
        let current_flow = self.find_current_flow(run_id).unwrap_or_default();

        let _ = self
            .pipeline_events
            .send(PipelineNotification::GateReached {
                run_id: run_id.into(),
                flow: current_flow,
                summary: summary.into(),
            });
        true
    }

    async fn handle_resume_flow(&mut self, run_id: &str, completed_stage_id: &str) {
        tracing::info!(run_id = %run_id, stage_id = %completed_stage_id, "Resuming flow after interactive stage");

        let stage = match self.db.load_stage(completed_stage_id) {
            Ok(Some(s)) => s,
            _ => {
                tracing::error!(stage_id = %completed_stage_id, "Stage not found for resume");
                return;
            }
        };

        let flow = match self.db.load_flows_for_run(run_id).ok().and_then(|flows| {
            flows
                .into_iter()
                .find(|f| *f.status() == FlowStatus::Running)
        }) {
            Some(f) => f,
            None => {
                tracing::error!(run_id = %run_id, "No running flow found for resume");
                return;
            }
        };

        let run = match self.db.load_run(run_id) {
            Ok(Some(r)) => r,
            _ => return,
        };

        if stage.flow_id().as_str() != flow.id() || *stage.status() != tj_db::StageStatus::Completed
        {
            tracing::warn!(
                run_id,
                completed_stage_id,
                "Ignoring invalid resume stage identity"
            );
            return;
        }
        let completed_stage_name = stage.stage_name().to_string();

        let art_map = self.collect_artifacts(run_id);

        // Use worktree info if available, otherwise fall back to project workspace
        let wt_info = self.worktrees.get(run_id).cloned();
        let workspace_path = wt_info
            .as_ref()
            .map(|wt| wt.worktree_path.clone())
            .or_else(|| self.resolve_workspace_path(run_id).map(PathBuf::from));
        let branch_name = wt_info
            .as_ref()
            .map(|wt| wt.branch_name.clone())
            .or_else(|| run.base_branch().map(|s| s.to_string()));
        let flow_name = flow.flow_name().to_string();
        let description = run.description().to_string();

        self.spawn_flow_resume(
            SpawnFlowParams {
                run_id: run_id.to_string(),
                flow_name,
                description,
                existing_artifacts: art_map,
                workspace_path,
                branch_name,
                slug: run.slug().unwrap_or("task").to_string(),
                attempt_offset: 0,
            },
            &completed_stage_name,
            flow.id(),
        )
        .await;
    }

    async fn spawn_flow_resume(
        &mut self,
        params: SpawnFlowParams,
        completed_stage: &str,
        flow_id: &str,
    ) {
        let smg = match self.smg_for_run(&params.run_id) {
            Ok(smg)
                if smg
                    .flows
                    .get(&params.flow_name)
                    .is_some_and(|flow| flow.flow_type == FlowType::Flow) =>
            {
                smg
            }
            Ok(_) => {
                self.handle_flow_failed(
                    &params.run_id,
                    &params.flow_name,
                    "resume target is not an executable flow in the selected workflow",
                )
                .await;
                return;
            }
            Err(error) => {
                self.handle_flow_failed(&params.run_id, &params.flow_name, &error.to_string())
                    .await;
                return;
            }
        };
        let bundle = match self.bundle_for_run(&params.run_id) {
            Ok(bundle) => bundle,
            Err(error) => {
                self.handle_flow_failed(
                    &params.run_id,
                    &params.flow_name,
                    &format!("execution bundle resolution failed: {error:#}"),
                )
                .await;
                return;
            }
        };
        let attempt_id = match self.db.begin_flow_attempt(
            &params.run_id,
            &params.flow_name,
            flow_id,
        ) {
            Ok(id) => id,
            Err(error) => {
                tracing::error!(run_id = %params.run_id, %error, "Cannot reserve resume attempt");
                return;
            }
        };
        let flow_id = flow_id.to_string();
        self.emit(
            &params.run_id,
            "flow_started",
            Some(&params.flow_name),
            Some(&format!("resumed from stage '{}'", completed_stage)),
        );
        let tx = self.tx.clone();
        let db = Arc::clone(&self.db);
        let config_loader = self.config_loader.clone();

        // Reuse the run's webui bridge if it survives; otherwise make one so a
        // resumed interactive stage can still wait for web input.
        let resume_session: Option<Arc<dyn tj_chat::ChatSession>> =
            match self.web_bridges.get(&params.run_id) {
                Some(entry) => Some(entry.value().clone()),
                None => {
                    let bridge = tj_chat::WebInputBridge::new();
                    self.web_bridges
                        .insert(params.run_id.clone(), Arc::clone(&bridge));
                    Some(bridge)
                }
            };

        tracing::info!(run_id = %params.run_id, flow = %params.flow_name, trace_id = %self.resolve_trace_id(&params.run_id), "Spawning flow resume from stage '{}'", completed_stage);

        let trace_id = self.resolve_trace_id(&params.run_id);
        let completed_stage = completed_stage.to_string();

        let execution_run = params.run_id.clone();
        let processes = tj_tools::processes::ProcessSupervisor::default();
        let scope = processes.clone();
        let task = tokio::spawn(async move {
            scope.scope(async move {
            let result = std::panic::AssertUnwindSafe(async {
                let start = std::time::Instant::now();

                let cfg = FlowConfig {
                    flow_id: flow_id.clone(),
                    run_id: params.run_id.clone(),
                    flow_name: params.flow_name.clone(),
                    flow_number: None,
                    smg,
                    description: params.description,
                    existing_artifacts: params.existing_artifacts,
                    workspace_path: params.workspace_path,
                    branch_name: params.branch_name,
                    slug: params.slug,
                    attempt_offset: params.attempt_offset,
                    bundle: bundle.clone(),
                };

                let flow = Flow::new(db, config_loader, resume_session, cfg);

                let outcome = flow.resume(&completed_stage).await;
                let elapsed = start.elapsed();
                tracing::debug!(trace_id = %trace_id, elapsed_ms = elapsed.as_millis() as u64, "Flow resume completed");
                outcome
            }).catch_unwind().await;

            match result {
                Ok(outcome) => {
                    let _ = tx.send(ManagerEvent::FlowDone {
                        attempt_id: attempt_id.clone(),
                        run_id: params.run_id.clone(),
                        flow_name: params.flow_name.clone(),
                        outcome,
                        trace_id: trace_id.clone(),
                    });
                }
                Err(_) => {
                    tracing::error!(run_id = %params.run_id, "Flow resume task panicked");
                    let _ = tx.send(ManagerEvent::FlowDone {
                        attempt_id: attempt_id.clone(),
                        run_id: params.run_id.clone(),
                        flow_name: params.flow_name.clone(),
                        outcome: FlowOutcome::Failed {
                            flow_id: String::new(),
                            run_id: params.run_id.clone(),
                            flow_name: params.flow_name.clone(),
                            error: "Flow resume task panicked".into(),
                        },
                        trace_id: trace_id.clone(),
                    });
                }
            }
        }).await
        });
        self.start_watchdog(&execution_run, self.attempt_timeout_for_run(&execution_run));
        self.executions
            .insert(execution_run, ActiveExecution { task, processes });
    }

    async fn handle_gate_decision(
        &mut self,
        decision: tj_db::GateDecision,
        expected_state_version: i64,
    ) -> Result<tj_db::GateDecisionReceipt> {
        const MAX_GATE_REWORKS: usize = 3;
        let run_id = decision.run_id.as_str().to_string();
        let approved = decision.approved;
        let reason = decision.reason.clone();
        let gate_name = self
            .db
            .load_run(&run_id)
            .ok()
            .flatten()
            .and_then(|run| run.gate_name().map(str::to_string))
            .unwrap_or_else(|| decision.gate_name.clone());
        let receipt = self
            .db
            .decide_gate(&decision, expected_state_version, MAX_GATE_REWORKS)?;

        // The durable decision row is the command acknowledgment. Routing is a
        // separate durable effect: a replay or restart resumes only an
        // effect that has not already produced an execution attempt.
        if let Some(effect_id) = receipt.effect_id.as_deref() {
            let effect = match self.db.load_pending_effect(effect_id)? {
                Some(effect) => effect,
                None => {
                    tracing::error!(
                        run_id,
                        effect_id,
                        "Accepted gate decision lost its routing effect"
                    );
                    return Ok(receipt);
                }
            };
            if effect.status == tj_db::EffectStatus::Completed {
                return Ok(receipt);
            }
            if effect.status == tj_db::EffectStatus::Ambiguous {
                tracing::error!(
                    run_id,
                    effect_id,
                    "Gate routing effect is ambiguous; explicit reconciliation required"
                );
                return Ok(receipt);
            }
            let run_status = self
                .db
                .load_run(&run_id)
                .ok()
                .flatten()
                .map(|run| run.status().clone());
            let already_routed = self
                .db
                .has_attempt_since(&run_id, &effect.created_at)
                .unwrap_or(false)
                || matches!(
                    run_status,
                    Some(
                        RunStatus::Completed
                            | RunStatus::Cancelled
                            | RunStatus::Failed
                            | RunStatus::WaitingForGate
                    )
                );
            if already_routed {
                let _ = self.db.complete_pending_effect(effect_id);
                return Ok(receipt);
            }
            if effect.status == tj_db::EffectStatus::Running {
                let _ = self.db.reopen_pending_effect(effect_id);
            }
            if !self.db.claim_pending_effect(effect_id)? {
                tracing::warn!(
                    run_id,
                    effect_id,
                    "Gate routing effect is already claimed; leaving it for explicit reconciliation"
                );
                return Ok(receipt);
            }
        } else if receipt.replayed {
            // Legacy decisions admitted before durable routing effects.
            return Ok(receipt);
        }

        self.cancel_watchdog(&run_id);
        if receipt.disposition == "rework_limit_cancelled" {
            self.emit(&run_id, "run_cancelled", None, Some("rework limit reached"));
            if let Err(error) = self.stop_execution(&run_id, true).await {
                tracing::error!(run_id, %error, "Gate rework limit reached but execution shutdown was not confirmed; workspace preserved");
                self.complete_gate_effect(&receipt);
                return Ok(receipt);
            }
            if let Some(ref notifier) = self.notifier {
                notifier
                    .notify_error(
                        &run_id,
                        "gate",
                        "Rework limit reached — run cancelled after repeated rejections",
                    )
                    .await;
                notifier.notify_cleanup(&run_id).await;
            }
            self.cleanup_task_worktree(&run_id).await;
            self.complete_gate_effect(&receipt);
            return Ok(receipt);
        }

        let run = match self.db.load_run(&run_id) {
            Ok(Some(run)) => run,
            Ok(None) => {
                tracing::error!(run_id, "Accepted gate decision has no run row");
                self.complete_gate_effect(&receipt);
                return Ok(receipt);
            }
            Err(error) => {
                tracing::error!(run_id, %error, "Accepted gate decision could not reload the run; routing deferred");
                self.complete_gate_effect(&receipt);
                return Ok(receipt);
            }
        };

        if !approved {
            // Rejection is a return path: re-run the flow that led to the gate,
            // with the accepted cause injected so stages can act on it.
            let Some(prev_flow) = self.find_current_flow(&run_id) else {
                tracing::warn!(
                    run_id,
                    "No previous flow to rework into; cancelling accepted rejection"
                );
                if self.update_run_status(&run_id, RunStatus::Cancelled) {
                    self.emit(
                        &run_id,
                        "run_cancelled",
                        None,
                        Some("no previous flow to rework"),
                    );
                    if let Some(ref notifier) = self.notifier {
                        notifier
                            .notify_error(
                                &run_id,
                                "gate",
                                "Gate rejected and no previous flow to rework",
                            )
                            .await;
                        notifier.notify_cleanup(&run_id).await;
                    }
                    self.cleanup_task_worktree(&run_id).await;
                }
                self.complete_gate_effect(&receipt);
                return Ok(receipt);
            };
            tracing::info!(run_id, reason = ?reason, "Gate rejected — routing back for rework");
            let rework = serde_json::json!({
                "cause": reason.as_deref().unwrap_or("Rejected at gate"),
                "gate": gate_name,
                "rejected_for_rework": true,
                "at": chrono::Utc::now().to_rfc3339(),
            });
            let mut art_map = self.collect_artifacts(&run_id);
            art_map.insert("__rework".to_string(), rework);

            let wt_info = self.worktrees.get(&run_id).cloned();
            let workspace_path = wt_info
                .as_ref()
                .map(|wt| wt.worktree_path.clone())
                .or_else(|| self.resolve_workspace_path(&run_id).map(PathBuf::from));
            let branch_name = wt_info
                .as_ref()
                .map(|wt| wt.branch_name.clone())
                .or_else(|| run.base_branch().map(str::to_string));

            let rework_attempt = self.next_rework_attempt(&run_id, &prev_flow);
            if let Some(ref notifier) = self.notifier {
                notifier
                    .notify_gate_reached(
                        &run_id,
                        "rework",
                        &format!("Gate rejected — re-entering '{}' for rework", prev_flow),
                    )
                    .await;
            }
            self.emit(
                &run_id,
                "rework_started",
                Some(&prev_flow),
                reason.as_deref(),
            );
            self.spawn_flow(SpawnFlowParams {
                run_id: run_id.clone(),
                flow_name: prev_flow,
                description: run.description().to_string(),
                existing_artifacts: art_map,
                workspace_path,
                branch_name,
                slug: run.slug().unwrap_or("task").to_string(),
                attempt_offset: rework_attempt,
            })
            .await;
            self.complete_gate_effect(&receipt);
            return Ok(receipt);
        }

        tracing::info!(run_id, "Gate approved");
        // find_current_flow returns the last executed flow (e.g. discovery).
        // Skip its explicit gate node and run the following flow.
        let Some(last_completed_flow) = self.find_current_flow(&run_id) else {
            tracing::warn!(
                run_id,
                "No current flow found after gate approval; completing run"
            );
            self.emit(&run_id, "run_completed", None, None);
            self.update_run_status(&run_id, RunStatus::Completed);
            self.complete_gate_effect(&receipt);
            return Ok(receipt);
        };
        let gate_flow = match self.next_flow(&run_id, &last_completed_flow) {
            Ok(Some(flow)) => flow,
            Ok(None) => {
                tracing::warn!(run_id, last_flow = %last_completed_flow, "No gate flow found after last completed flow; completing run");
                self.emit(&run_id, "run_completed", None, None);
                self.update_run_status(&run_id, RunStatus::Completed);
                self.complete_gate_effect(&receipt);
                return Ok(receipt);
            }
            Err(error) => {
                tracing::error!(run_id, %error, "Cannot resolve gate route after accepted approval");
                self.emit(&run_id, "gate_route_failed", None, Some(&error.to_string()));
                self.update_run_status(&run_id, RunStatus::Failed);
                self.complete_gate_effect(&receipt);
                return Ok(receipt);
            }
        };
        let target_flow = match self.next_flow(&run_id, &gate_flow) {
            Ok(target) => target,
            Err(error) => {
                tracing::error!(run_id, %error, "Cannot resolve post-gate route after accepted approval");
                self.emit(
                    &run_id,
                    "gate_route_failed",
                    Some(&gate_flow),
                    Some(&error.to_string()),
                );
                self.update_run_status(&run_id, RunStatus::Failed);
                self.complete_gate_effect(&receipt);
                return Ok(receipt);
            }
        };

        let artifacts = self.collect_artifacts(&run_id);
        let wt_info = self.worktrees.get(&run_id).cloned();
        let workspace_path = wt_info
            .as_ref()
            .map(|wt| wt.worktree_path.clone())
            .or_else(|| self.resolve_workspace_path(&run_id).map(PathBuf::from));
        let branch_name = wt_info
            .as_ref()
            .map(|wt| wt.branch_name.clone())
            .or_else(|| run.base_branch().map(str::to_string));

        let policy = match self.run_policy(&run_id) {
            Ok(policy) => policy,
            Err(error) => {
                tracing::error!(run_id, %error, "Cannot resolve execution policy after gate approval");
                self.update_run_status(&run_id, RunStatus::Failed);
                self.complete_gate_effect(&receipt);
                return Ok(receipt);
            }
        };
        let requires_task_selection = match &policy {
            RunPolicy::V1(legacy) => target_flow
                .as_deref()
                .is_some_and(|flow| legacy.requires_task_selection_before(flow)),
            RunPolicy::V2(contract) => CapabilityRuntime::new(contract)
                .after_gate(&gate_flow)
                .is_some(),
        };

        match target_flow {
            Some(next_flow) if requires_task_selection => {
                let legacy = match &policy {
                    RunPolicy::V1(legacy) => Some(*legacy),
                    RunPolicy::V2(_) => None,
                };
                // Approval of the plan is not task selection. The declared next
                // flow must not start until exactly one approved, ready task
                // with its exact plan/packet identity has been recorded.
                match crate::capabilities::plan::valid_task_selection(&self.db, &run_id) {
                    Ok(Some(selection)) => {
                        tracing::info!(run_id, task = %selection.task_id, selection = %selection.id,
                            "Approved task selection exists; spawning the selected flow");
                        self.spawn_flow(SpawnFlowParams {
                            run_id: run_id.clone(),
                            flow_name: next_flow,
                            description: run.description().to_string(),
                            existing_artifacts: artifacts,
                            workspace_path,
                            branch_name,
                            slug: run.slug().unwrap_or("task").to_string(),
                            attempt_offset: 0,
                        })
                        .await;
                    }
                    Ok(None) => {
                        let mut waiting = match self.db.load_run(&run_id) {
                            Ok(Some(run)) => run,
                            _ => return Ok(receipt),
                        };
                        let waiting_state = legacy
                            .map(LegacyV1Policy::task_selection_gate_state)
                            .unwrap_or("task_selection_required");
                        waiting.set_status(RunStatus::WaitingForGate);
                        waiting.set_gate_state(Some(waiting_state.into()));
                        waiting.set_gate_identity(None, None);
                        waiting.set_updated_at(chrono::Utc::now().to_rfc3339());
                        if let Err(error) = self.db.save_run(&waiting) {
                            tracing::error!(run_id, %error, "Cannot persist task-selection wait");
                            return Ok(receipt);
                        }
                        self.emit(&run_id, waiting_state, None,
                            Some("plan approved; select exactly one ready task before the next flow may start"));
                        if let Some(ref notifier) = self.notifier {
                            notifier.notify_gate_reached(&run_id, "task-selection",
                                "Plan approved. Select one ready task to start the next flow; no task runs automatically.").await;
                        }
                    }
                    Err(error) => {
                        // Invalid selection identity is a closed gate, not an
                        // excuse to execute an arbitrary task.
                        let mut waiting = match self.db.load_run(&run_id) {
                            Ok(Some(run)) => run,
                            _ => return Ok(receipt),
                        };
                        let waiting_state = legacy
                            .map(LegacyV1Policy::task_selection_gate_state)
                            .unwrap_or("task_selection_required");
                        waiting.set_status(RunStatus::WaitingForGate);
                        waiting.set_gate_state(Some(waiting_state.into()));
                        waiting.set_updated_at(chrono::Utc::now().to_rfc3339());
                        let _ = self.db.save_run(&waiting);
                        self.emit(
                            &run_id,
                            "task_selection_rejected",
                            None,
                            Some(&format!("selection contract failed: {error}")),
                        );
                    }
                }
            }
            Some(next_flow) => {
                tracing::info!(run_id, gate = %gate_flow, next_flow = %next_flow, "Spawning post-gate flow after approval");
                self.spawn_flow(SpawnFlowParams {
                    run_id: run_id.clone(),
                    flow_name: next_flow,
                    description: run.description().to_string(),
                    existing_artifacts: artifacts,
                    workspace_path,
                    branch_name,
                    slug: run.slug().unwrap_or("task").to_string(),
                    attempt_offset: 0,
                })
                .await;
            }
            None => {
                tracing::info!(run_id, gate = %gate_flow, "No more flows after gate; completing run");
                self.emit(&run_id, "run_completed", None, None);
                self.update_run_status(&run_id, RunStatus::Completed);
                self.cleanup_task_worktree(&run_id).await;
            }
        }
        self.complete_gate_effect(&receipt);
        Ok(receipt)
    }

    fn complete_gate_effect(&self, receipt: &tj_db::GateDecisionReceipt) {
        let Some(effect_id) = receipt.effect_id.as_deref() else {
            return;
        };
        let effect = match self.db.load_pending_effect(effect_id) {
            Ok(Some(effect)) => effect,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(effect_id, %error, "Could not load gate routing effect");
                return;
            }
        };
        if matches!(
            effect.status,
            tj_db::EffectStatus::Completed | tj_db::EffectStatus::Ambiguous
        ) {
            return;
        }
        let run_status = self
            .db
            .load_run(&effect.run_id)
            .ok()
            .flatten()
            .map(|run| run.status().clone());
        let routed = matches!(
            run_status,
            Some(
                RunStatus::WaitingForGate
                    | RunStatus::Completed
                    | RunStatus::Cancelled
                    | RunStatus::Failed
                    | RunStatus::Interrupted
            )
        ) || self
            .db
            .has_attempt_since(&effect.run_id, &effect.created_at)
            .unwrap_or(false);
        if routed {
            if let Err(error) = self.db.complete_pending_effect(effect_id) {
                tracing::warn!(effect_id, %error, "Could not complete gate routing effect");
            }
        } else {
            let _ = self.db.mark_pending_effect_ambiguous(
                effect_id,
                "gate routing claimed but no execution attempt or terminal state was observed",
            );
            self.emit(
                &effect.run_id,
                "effect_reconciliation_required",
                None,
                Some("gate route did not produce a durable execution attempt"),
            );
        }
    }

    async fn handle_cancel_run(&mut self, run_id: &str) -> Result<()> {
        let persisted: Result<bool> = (|| {
            let run = self.db.load_run(run_id)?.context("Run does not exist")?;
            if matches!(
                run.status(),
                RunStatus::Running | RunStatus::WaitingForGate | RunStatus::Interrupted
            ) {
                anyhow::ensure!(
                    self.update_run_status(run_id, RunStatus::Cancelled),
                    "Cannot persist cancellation"
                );
                self.emit(
                    run_id,
                    "run_cancelled",
                    None,
                    Some("cancelled by user or shutdown"),
                );
                return Ok(true);
            }
            Ok(false)
        })();
        self.cancel_watchdog(run_id);
        // Even a persistence outage must not prevent process shutdown. Preserve
        // the workspace and return the persistence error after joining workers.
        self.stop_execution(run_id, true).await?;
        if self.db.has_run_budget(run_id).unwrap_or(false) {
            let _ = self.db.reconcile_budget_intervals(
                run_id,
                chrono::Utc::now(),
                "execution cancelled; active interval fenced",
            );
        }
        let changed = persisted?;
        anyhow::ensure!(
            !self.cleanup_blocked.contains(run_id),
            "Execution shutdown unconfirmed; workspace preserved"
        );
        if changed && let Some(ref notifier) = self.notifier {
            notifier
                .notify_error(run_id, "run", "Cancelled by user")
                .await;
            notifier.notify_cleanup(run_id).await;
        }

        // Cleanup worktree on cancel
        self.cleanup_task_worktree(run_id).await;
        Ok(())
    }

    async fn stop_execution(&mut self, run_id: &str, abort: bool) -> Result<()> {
        let Some(execution) = self.executions.remove(run_id) else {
            return Ok(());
        };
        if abort {
            execution.task.abort();
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let joined = execution.task.await;
            // Always join process reapers, even if the flow task panicked.
            execution.processes.join().await?;
            match joined {
                Ok(()) => Ok(()),
                Err(error) if abort && error.is_cancelled() => Ok(()),
                Err(error) => Err(anyhow::anyhow!("Execution task failed: {error}")),
            }
        })
        .await
        .unwrap_or_else(|_| {
            Err(anyhow::anyhow!(
                "Execution did not stop within five seconds"
            ))
        });
        if let Err(error) = &result {
            self.cleanup_blocked.insert(run_id.to_string());
            self.emit(
                run_id,
                "execution_shutdown_failed",
                None,
                Some(&error.to_string()),
            );
        }
        result
    }

    /// Look up existing flow records for a run and reconstruct a WorktreeInfo if the
    /// worktree still exists on disk. Returns None if no existing worktree is found.
    fn reconstruct_existing_worktree(&self, run_id: &str) -> Option<WorktreeInfo> {
        let flows = self.db.load_flows_for_run(run_id).ok()?;
        for flow in flows {
            if let Some(wp) = flow.worktree_path() {
                let worktree_path = PathBuf::from(wp);
                if worktree_path.exists() {
                    let branch = flow.branch_name().unwrap_or("").to_string();
                    let safe_name = branch.replace('/', "_");
                    let repo_path = self
                        .worktree_manager
                        .as_ref()
                        .map(|wm| wm.worktrees_base().join(format!("{}_repo", safe_name)))
                        .unwrap_or_default();
                    tracing::debug!(
                        run_id,
                        worktree_path = %worktree_path.display(),
                        branch = %branch,
                        repo_path = %repo_path.display(),
                        "Found existing worktree in DB flow record"
                    );
                    return Some(WorktreeInfo {
                        worktree_path,
                        branch_name: branch,
                        repo_path,
                    });
                }
            }
        }
        tracing::debug!(run_id, "No existing worktree found in DB flow records");
        None
    }

    /// Create a worktree for a run, returning (workspace_path, branch_name) if successful.
    async fn create_worktree_for_run(
        &mut self,
        run_id: &str,
        run: &tj_db::PipelineRun,
        slug: &str,
        base_branch: &str,
    ) -> Option<(Option<PathBuf>, Option<String>)> {
        let wm = self.worktree_manager.as_ref()?.clone();

        let run_id_owned = run_id.to_string();
        let slug_owned = slug.to_string();
        let base_branch_owned = base_branch.to_string();

        let repo_url = run.repo_url().map(|s| s.to_string());
        let local_path = if repo_url.is_none() {
            run.project_id()
                .and_then(|pid| self.db.load_project(pid.as_str()).ok().flatten())
                .and_then(|p| p.workspace_path)
        } else {
            None
        };

        if repo_url.is_none() && local_path.is_none() && run.project_id().is_some() {
            tracing::debug!(run_id, "Project has no workspace_path, skipping worktree");
            return None;
        }
        if repo_url.is_none() && run.project_id().is_none() {
            tracing::debug!(
                run_id,
                "No repo_url or project_id, skipping worktree creation"
            );
            return None;
        }

        // Resolve SSH key guard before cloning so the temp key file stays alive
        // during the entire spawn_blocking call. The guard is held in this async
        // scope and dropped only after spawn_blocking returns, ensuring the key
        // file exists when git contacts the remote.
        let ssh_guard = match self.resolve_ssh_guard(run_id).await {
            Ok(guard) => guard,
            Err(error) => {
                tracing::error!(run_id, %error, "SSH provisioning prerequisite failed; no empty repository fallback");
                self.update_run_status(run_id, RunStatus::Failed);
                if let Some(ref notifier) = self.notifier {
                    notifier
                        .notify_error(run_id, "workspace setup", &error.to_string())
                        .await;
                }
                return None;
            }
        };
        let ssh_env = ssh_guard.as_ref().map(|g| g.ssh_env());

        let wt_result = tokio::task::spawn_blocking(move || {
            if let Some(url) = repo_url {
                tracing::debug!(run_id = %run_id_owned, repo_url = %url, has_ssh_env = ssh_env.is_some(), "Creating worktree from remote repo");
                wm.create_with_env(&url, &run_id_owned, &slug_owned, &base_branch_owned, ssh_env.as_ref())
            } else if let Some(ref lp) = local_path {
                tracing::debug!(run_id = %run_id_owned, local_path = %lp, "Creating worktree from local repo");
                wm.create_from_local_with_env(lp, &run_id_owned, &slug_owned, &base_branch_owned, ssh_env.as_ref())
            } else {
                tracing::debug!(run_id = %run_id_owned, "No repo_url or local_path, skipping worktree creation");
                Err(anyhow::anyhow!("No repo_url or local_path available"))
            }
        }).await.unwrap_or_else(|e| {
            tracing::error!(run_id, error = %e, "spawn_blocking panicked in worktree creation");
            Err(anyhow::anyhow!("spawn_blocking panicked: {}", e))
        });
        // ssh_guard dropped here, after spawn_blocking completes — temp key file cleaned up

        match wt_result {
            Ok(wt_info) => {
                let ws_path = Some(wt_info.worktree_path.clone());
                let br_name = Some(wt_info.branch_name.clone());

                // Notify workspace ready
                if let Some(ref notifier) = self.notifier {
                    let flow_name = match self.find_current_flow(run_id) {
                        Some(f) => f,
                        None => self
                            .smg_for_run(run_id)
                            .map(|smg| smg.entry)
                            .unwrap_or_else(|_| "workflow".into()),
                    };
                    notifier.notify_workspace_ready(run_id, &flow_name).await;
                }

                tracing::debug!(
                    run_id,
                    worktree_path = ?wt_info.worktree_path,
                    branch = %wt_info.branch_name,
                    repo_path = ?wt_info.repo_path,
                    "Worktree created and stored"
                );
                self.worktrees.insert(run_id.to_string(), wt_info);

                Some((ws_path, br_name))
            }
            Err(e) => {
                tracing::error!(run_id, error = %e, "Failed to create worktree");
                self.update_run_status(run_id, RunStatus::Failed);
                if let Some(ref notifier) = self.notifier {
                    notifier
                        .notify_error(run_id, "workspace setup", &e.to_string())
                        .await;
                }
                None
            }
        }
    }

    async fn cleanup_task_worktree(&mut self, run_id: &str) {
        if self.cleanup_blocked.contains(run_id) {
            tracing::error!(
                run_id,
                "Preserving workspace: execution shutdown was not confirmed"
            );
            return;
        }
        if let Err(error) = self.stop_execution(run_id, true).await {
            tracing::error!(run_id, %error, "Preserving workspace after execution shutdown failure");
            return;
        }
        // Unpublished work is never deleted. A failed push/PR, an uncertain
        // remote operation, or uncommitted changes keep the workspace available
        // for explicit reconciliation/retry.
        if let Ok(Some(publication)) = self.db.latest_publication_for_run(run_id) {
            if publication.push_state != tj_db::publications::PublicationState::Succeeded
                || publication.pr_state != tj_db::publications::PublicationState::Succeeded
            {
                tracing::warn!(run_id, publication = %publication.id,
                    push = %publication.push_state.as_str(), pr = %publication.pr_state.as_str(),
                    "Preserving unpublished task workspace");
                return;
            }
        }
        if let Some(wt) = self.worktrees.get(run_id).cloned() {
            let path = wt.worktree_path.clone();
            let has_changes =
                tokio::task::spawn_blocking(move || WorktreeManager::has_changes(&path))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(false);
            if has_changes {
                tracing::warn!(run_id, "Preserving workspace with uncommitted changes");
                return;
            }
        }
        if let Some(wt) = self.worktrees.remove(run_id) {
            tracing::debug!(run_id, worktree = ?wt.worktree_path, "Cleaning up worktree");
            let wt_path = wt.worktree_path.clone();
            let repo_path = wt.repo_path.clone();
            let result =
                tokio::task::spawn_blocking(move || WorktreeManager::cleanup(&wt_path, &repo_path))
                    .await;
            match result {
                Ok(Ok(())) => {
                    tracing::debug!(run_id, "Worktree cleanup complete");
                }
                Ok(Err(e)) => {
                    tracing::warn!(run_id, error = %e, "Worktree cleanup failed");
                }
                Err(e) => {
                    tracing::error!(run_id, error = %e, "spawn_blocking panicked in worktree cleanup");
                }
            }
        }
    }

    async fn spawn_flow(&mut self, mut params: SpawnFlowParams) {
        let policy = match self.run_policy(&params.run_id) {
            Ok(policy) => policy,
            Err(error) => {
                self.handle_flow_failed(
                    &params.run_id,
                    &params.flow_name,
                    &format!("cannot resolve the frozen execution policy: {error:#}"),
                )
                .await;
                return;
            }
        };
        let (require_packet, require_plan, require_selection, input_destination) = match &policy {
            RunPolicy::V2(contract) => {
                let preparation = CapabilityRuntime::new(contract).before_flow(&params.flow_name);
                (
                    preparation.approved_packet,
                    preparation.approved_plan,
                    preparation.task_selection,
                    preparation.input_destination,
                )
            }
            RunPolicy::V1(legacy) => (
                legacy.requires_approved_packet_before(&params.flow_name),
                false,
                legacy.requires_task_selection_before(&params.flow_name),
                None,
            ),
        };
        if let Some(destination) = input_destination {
            let Some(workspace) = params.workspace_path.as_deref() else {
                self.handle_flow_failed(
                    &params.run_id,
                    &params.flow_name,
                    "workflow declares inputs but the flow has no workspace",
                )
                .await;
                return;
            };
            if let Err(error) = crate::capabilities::inputs::copy_project_inputs(
                &self.db,
                &params.run_id,
                workspace,
                destination,
            ) {
                self.handle_flow_failed(
                    &params.run_id,
                    &params.flow_name,
                    &format!("declared project input copy failed: {error:#}"),
                )
                .await;
                return;
            }
        }
        if require_selection {
            let selection = match crate::capabilities::plan::valid_task_selection(
                &self.db,
                &params.run_id,
            ) {
                Ok(Some(selection)) => selection,
                Ok(None) => {
                    self.handle_flow_failed(&params.run_id, &params.flow_name,
                        "the selected flow cannot start without exactly one approved ready task selection").await;
                    return;
                }
                Err(error) => {
                    self.handle_flow_failed(
                        &params.run_id,
                        &params.flow_name,
                        &format!("task selection identity failed validation: {error}"),
                    )
                    .await;
                    return;
                }
            };
            let payload = serde_json::from_str::<serde_json::Value>(&selection.payload_json)
                .unwrap_or(serde_json::Value::Null);
            let task_title = payload
                .pointer("/task/title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("approved task")
                .to_string();
            params
                .existing_artifacts
                .insert("__task_selection".into(), payload);
            params.description = format!(
                "{}

Approved task: {} — {} (selection {}, plan {})",
                params.description,
                selection.task_id,
                task_title,
                selection.id,
                selection.plan_revision_id
            );
        }
        if require_packet {
            match self.db.approved_packet_revision(&params.run_id) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    self.handle_flow_failed(
                        &params.run_id,
                        &params.flow_name,
                        "the declared flow cannot start without an approved requirements packet revision",
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    self.handle_flow_failed(&params.run_id, &params.flow_name, &error.to_string())
                        .await;
                    return;
                }
            }
        }
        if require_plan {
            match self.db.approved_plan_revision(&params.run_id) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    self.handle_flow_failed(
                        &params.run_id,
                        &params.flow_name,
                        "the declared flow cannot start without an approved structured plan revision",
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    self.handle_flow_failed(&params.run_id, &params.flow_name, &error.to_string())
                        .await;
                    return;
                }
            }
        }
        let smg = match self.smg_for_run(&params.run_id) {
            Ok(smg)
                if smg
                    .flows
                    .get(&params.flow_name)
                    .is_some_and(|flow| flow.flow_type == FlowType::Flow) =>
            {
                smg
            }
            Ok(_) => {
                self.handle_flow_failed(
                    &params.run_id,
                    &params.flow_name,
                    "target is not an executable flow in the selected workflow",
                )
                .await;
                return;
            }
            Err(error) => {
                self.handle_flow_failed(&params.run_id, &params.flow_name, &error.to_string())
                    .await;
                return;
            }
        };
        let bundle = match self.bundle_for_run(&params.run_id) {
            Ok(bundle) => bundle,
            Err(error) => {
                self.handle_flow_failed(
                    &params.run_id,
                    &params.flow_name,
                    &format!("execution bundle resolution failed: {error:#}"),
                )
                .await;
                return;
            }
        };
        let flow_id = format!("flow_{}", uuid::Uuid::new_v4());
        let attempt_id =
            match self
                .db
                .begin_flow_attempt(&params.run_id, &params.flow_name, &flow_id)
            {
                Ok(id) => id,
                Err(error) => {
                    tracing::error!(run_id = %params.run_id, %error, "Cannot reserve flow attempt");
                    return;
                }
            };
        // Active time is a run budget: start before any model/tool work and
        // fail closed when the run has already consumed its allowance.
        if self.db.has_run_budget(&params.run_id).unwrap_or(false)
            && let Err(error) = self.db.start_budget_interval(&params.run_id, &attempt_id)
        {
            tracing::error!(run_id = %params.run_id, %error, "Cannot start budgeted execution interval");
            self.handle_flow_failed(&params.run_id, &params.flow_name, &error.to_string())
                .await;
            return;
        }
        self.emit(
            &params.run_id,
            "flow_started",
            Some(&params.flow_name),
            None,
        );
        tracing::debug!(run_id = %params.run_id, flow = %params.flow_name, "spawn_flow entered");

        let tx = self.tx.clone();
        let db = Arc::clone(&self.db);
        let config_loader = self.config_loader.clone();

        let project_id = self
            .db
            .load_run(&params.run_id)
            .ok()
            .flatten()
            .and_then(|r| r.project_id().map(|p| p.as_str().to_string()))
            .unwrap_or_default();

        let flow_number = self
            .db
            .next_flow_number(&project_id, &params.flow_name)
            .ok();

        tracing::info!(run_id = %params.run_id, flow = %params.flow_name, number = ?flow_number, "Spawning flow");

        // Every flow gets a webui input bridge. With a chat provider the stage
        // hears Zulip OR the webui, whichever answers first; chat-less
        // deployments wait on the webui alone instead of running unattended.
        let mut chat_session = self
            .create_flow_session(&params.run_id, &params.flow_name, flow_number)
            .await;
        let bridge = tj_chat::WebInputBridge::new();
        self.web_bridges
            .insert(params.run_id.clone(), Arc::clone(&bridge));
        chat_session = Some(match chat_session {
            Some(session) => Arc::new(tj_chat::HybridSession::new(session, Arc::clone(&bridge))),
            None => bridge,
        });

        let trace_id = self.resolve_trace_id(&params.run_id);

        let execution_run = params.run_id.clone();
        let processes = tj_tools::processes::ProcessSupervisor::default();
        let scope = processes.clone();
        let task = tokio::spawn(async move {
            scope.scope(async move {
            let result = std::panic::AssertUnwindSafe(async {
                let start = std::time::Instant::now();

                let cfg = FlowConfig {
                    flow_id: flow_id.clone(),
                    run_id: params.run_id.clone(),
                    flow_name: params.flow_name.clone(),
                    flow_number,
                    smg,
                    description: params.description,
                    existing_artifacts: params.existing_artifacts,
                    workspace_path: params.workspace_path,
                    branch_name: params.branch_name,
                    slug: params.slug,
                    attempt_offset: params.attempt_offset,
                    bundle: bundle.clone(),
                };

                let flow = Flow::new(db, config_loader, chat_session, cfg);

                let outcome = flow.execute().await;
                let elapsed = start.elapsed();
                tracing::debug!(trace_id = %trace_id, elapsed_ms = elapsed.as_millis() as u64, "Flow execute completed");
                outcome
            }).catch_unwind().await;

            match result {
                Ok(outcome) => {
                    let _ = tx.send(ManagerEvent::FlowDone {
                        attempt_id: attempt_id.clone(),
                        run_id: params.run_id.clone(),
                        flow_name: params.flow_name.clone(),
                        outcome,
                        trace_id: trace_id.clone(),
                    });
                }
                Err(_) => {
                    tracing::error!(run_id = %params.run_id, "Flow task panicked");
                    let _ = tx.send(ManagerEvent::FlowDone {
                        attempt_id: attempt_id.clone(),
                        run_id: params.run_id.clone(),
                        flow_name: params.flow_name.clone(),
                        outcome: FlowOutcome::Failed {
                            flow_id: String::new(),
                            run_id: params.run_id.clone(),
                            flow_name: params.flow_name.clone(),
                            error: "Flow task panicked".into(),
                        },
                        trace_id: trace_id.clone(),
                    });
                }
            }
        }).await
        });
        self.start_watchdog(&execution_run, self.attempt_timeout_for_run(&execution_run));
        self.executions
            .insert(execution_run, ActiveExecution { task, processes });
    }

    fn bundle_for_run(&self, run_id: &str) -> Result<Option<Arc<ExecutionBundle>>> {
        let run = self
            .db
            .load_run(run_id)?
            .with_context(|| format!("run '{}' does not exist", run_id))?;
        Ok(load_execution_bundle(&self.db, &run)?.map(Arc::new))
    }

    /// The SMG a run executes on. New runs always resolve their immutable
    /// execution bundle; only legacy rows (no bundle id, admitted before
    /// TJ-012) fall back to the old pinned/configured path, and that fallback
    /// is recorded as an explicit non-reproducible limitation.
    fn smg_for_run(&self, run_id: &str) -> Result<StateMachineGraph> {
        if let Some(smg) = self
            .smg_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(run_id)
        {
            return Ok(smg.clone());
        }
        let run = self
            .db
            .load_run(run_id)?
            .with_context(|| format!("run '{}' does not exist", run_id))?;
        let smg = match load_execution_bundle(&self.db, &run)? {
            Some(bundle) => bundle.graph,
            None => {
                tracing::warn!(
                    run_id,
                    workflow_id = ?run.workflow_id(),
                    "Legacy run has no execution bundle; resolving mutable files (not fully reproducible)"
                );
                self.emit(
                    run_id,
                    "legacy_execution_bundle",
                    None,
                    Some("run was admitted before immutable execution bundles; restart/config changes are not fully reproducible"),
                );
                let workflow_id = run.workflow_id().ok_or_else(|| {
                    anyhow::anyhow!(
                        "run '{}' has no execution bundle and no pinned workflow; import a workflow and pin the lane before retrying",
                        run_id
                    )
                })?;
                let workflow = self.db.get_workflow(workflow_id)?.with_context(|| {
                    format!(
                        "run '{}' pins missing workflow '{}'; re-import the source and pin an existing version",
                        run_id, workflow_id
                    )
                })?;
                let smg: StateMachineGraph = serde_json::from_str(&workflow.content_json)
                    .with_context(|| {
                        format!(
                            "run '{}' pinned workflow '{}' contains invalid SMG JSON",
                            run_id, workflow_id
                        )
                    })?;
                self.config_loader.validate_smg(&smg).with_context(|| {
                    format!(
                        "run '{}' pinned workflow '{}' failed validation",
                        run_id, workflow_id
                    )
                })?;
                smg
            }
        };
        tj_config::validation::validate_graph(&smg).with_context(|| {
            format!("run '{}' workflow graph failed runtime validation", run_id)
        })?;
        self.smg_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(run_id.to_string(), smg.clone());
        Ok(smg)
    }

    fn next_flow(&self, run_id: &str, current: &str) -> Result<Option<String>> {
        let smg = self.smg_for_run(run_id);
        let smg = smg?;
        let idx = smg
            .flow_order
            .iter()
            .position(|f| f == current)
            .with_context(|| {
                format!(
                    "current flow '{}' is not in workflow '{}' flow-order",
                    current, smg.name
                )
            })?;
        Ok(smg.flow_order.get(idx + 1).cloned())
    }

    fn next_rework_attempt(&self, run_id: &str, flow_name: &str) -> u32 {
        let reworks = self
            .db
            .list_events(run_id)
            .map(|evts| {
                evts.iter()
                    .filter(|e| {
                        e.kind == "rework_started" && e.flow_name.as_deref() == Some(flow_name)
                    })
                    .count()
            })
            .unwrap_or(0);
        (reworks + 1) as u32
    }

    /// Close the durable completion effect after its route/publication work.
    /// A still-running run with no replacement attempt means the route did not
    /// durably start; that effect stays ambiguous instead of being reported as
    /// completed.
    async fn settle_flow_completion_effect(&mut self, run_id: &str, attempt_id: &str) {
        let key = format!("flow_completion:{attempt_id}");
        let effect = match self.db.pending_effect_by_key(&key) {
            Ok(Some(effect)) => effect,
            _ => return,
        };
        if effect.status == tj_db::EffectStatus::Completed {
            return;
        }
        let status = self
            .db
            .load_run(run_id)
            .ok()
            .flatten()
            .map(|run| run.status().clone());
        let routed = matches!(
            status,
            Some(
                RunStatus::WaitingForGate
                    | RunStatus::Completed
                    | RunStatus::Failed
                    | RunStatus::Cancelled
                    | RunStatus::Interrupted
            )
        ) || self
            .db
            .has_attempt_since(run_id, &effect.created_at)
            .unwrap_or(true);
        if routed {
            let _ = self.db.complete_pending_effect(&effect.id);
        } else {
            let _ = self.db.mark_pending_effect_ambiguous(
                &effect.id,
                "flow completion accepted but no durable follow-on route was observed",
            );
            self.emit(
                run_id,
                "effect_reconciliation_required",
                None,
                Some("flow completion did not start a durable follow-on route"),
            );
            if self.db.recovery_state(run_id).ok().flatten().is_none() {
                let _ = self.db.mark_run_interrupted(
                    run_id,
                    "flow completion pending routing could not be proven complete",
                    true,
                );
            }
        }
    }

    fn emit(&self, run_id: &str, kind: &str, flow: Option<&str>, summary: Option<&str>) {
        let e = tj_db::RunEvent {
            id: format!("evt_{}", uuid::Uuid::new_v4()),
            run_id: tj_db::RunId(run_id.to_string()),
            kind: kind.to_string(),
            actor: None,
            flow_name: flow.map(|s| s.to_string()),
            stage_name: None,
            summary: summary.map(|s| s.to_string()),
            payload: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(err) = self.db.append_event(&e) {
            tracing::warn!(run_id = %run_id, kind = %kind, error = %err, "Failed to append run event");
        }
    }

    fn find_current_flow(&self, run_id: &str) -> Option<String> {
        self.db
            .load_flows_for_run(run_id)
            .ok()?
            .into_iter()
            .rfind(|f| *f.status() == FlowStatus::Running || *f.status() == FlowStatus::Completed)
            .map(|f| f.flow_name().to_string())
    }

    /// Per-run attempt timeout derived from the run's own frozen/registry
    /// workflow limits. Startup never needs a process-wide graph to arm it.
    fn attempt_timeout_for_run(&self, run_id: &str) -> Option<u64> {
        self.smg_for_run(run_id)
            .ok()
            .map(|graph| graph.limits.max_steps as u64 * 600)
    }

    fn start_watchdog(&mut self, run_id: &str, attempt_timeout_secs: Option<u64>) {
        self.cancel_watchdog(run_id);
        let run = match self.db.load_run(run_id) {
            Ok(Some(run)) => run,
            _ => return,
        };
        let mut timer = attempt_timeout_secs.map(|seconds| {
            (
                std::time::Duration::from_secs(seconds),
                "attempt watchdog elapsed",
            )
        });
        if let Some(deadline) = run.deadline_at() {
            let remaining = chrono::DateTime::parse_from_rfc3339(deadline)
                .ok()
                .and_then(|deadline| {
                    deadline
                        .signed_duration_since(chrono::Utc::now())
                        .to_std()
                        .ok()
                })
                .unwrap_or_default();
            if timer
                .as_ref()
                .is_none_or(|(duration, _)| remaining <= *duration)
            {
                timer = Some((remaining, "absolute run deadline elapsed"));
            }
        }
        let Some((duration, reason)) = timer else {
            return;
        };
        let generation = uuid::Uuid::new_v4().to_string();
        let event_generation = generation.clone();
        let tx = self.tx.clone();
        let run_id_owned = run_id.to_string();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            tracing::warn!(run_id = %run_id_owned, "Watchdog fired");
            let _ = tx.send(ManagerEvent::WatchdogTick {
                run_id: run_id_owned,
                generation: event_generation,
            });
        });
        self.watchdogs.insert(
            run_id.to_string(),
            Watchdog {
                generation,
                task: handle,
                reason,
            },
        );
    }

    fn cancel_watchdog(&mut self, run_id: &str) {
        if let Some(handle) = self.watchdogs.remove(run_id) {
            handle.task.abort();
        }
    }

    async fn handle_watchdog_tick(&mut self, run_id: &str, generation: &str) {
        let reason = match self.watchdogs.get(run_id) {
            Some(watchdog) if watchdog.generation == generation => watchdog.reason,
            _ => return,
        };
        tracing::error!(run_id, reason, "Run timed out, cancelling");
        self.cancel_watchdog(run_id);
        let persisted = self.update_run_status(run_id, RunStatus::Failed);
        if persisted {
            self.emit(run_id, "run_timed_out", None, Some(reason));
        }
        if let Err(error) = self.stop_execution(run_id, true).await {
            tracing::error!(run_id, %error, "Timeout could not confirm execution shutdown");
            return;
        }
        if self.db.has_run_budget(run_id).unwrap_or(false) {
            let _ = self.db.reconcile_budget_intervals(
                run_id,
                chrono::Utc::now(),
                "watchdog timeout; active interval fenced",
            );
        }
        if !persisted {
            return;
        }
        if let Some(ref notifier) = self.notifier {
            notifier
                .notify_error(run_id, "watchdog", "Run timed out and was cancelled")
                .await;
            notifier.notify_cleanup(run_id).await;
        }

        // Cleanup worktree on watchdog timeout
        self.cleanup_task_worktree(run_id).await;
    }

    fn update_run_status(&self, run_id: &str, status: RunStatus) -> bool {
        let terminal = matches!(
            &status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
        );
        if let Ok(Some(mut run)) = self.db.load_run(run_id) {
            run.set_status(status);
            run.set_updated_at(chrono::Utc::now().to_rfc3339());
            if let Err(e) = self.db.save_run(&run) {
                tracing::error!(run_id = %run_id, error = %e, "Failed to save run state");
                return false;
            }
        } else {
            return false;
        }
        if terminal {
            self.web_bridges.remove(run_id);
            self.smg_cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(run_id);
        }
        true
    }

    /// Create a flow session via the chat provider for stage chat wiring.
    async fn create_flow_session(
        &self,
        run_id: &str,
        flow_name: &str,
        flow_number: Option<u64>,
    ) -> Option<Arc<dyn tj_chat::ChatSession>> {
        tracing::debug!(run_id, flow = %flow_name, "create_flow_session entered");

        let chat_provider = match self.chat.as_ref() {
            Some(cp) => {
                tracing::debug!(run_id, provider = %cp.provider_id(), "chat_provider present");
                cp
            }
            None => {
                tracing::debug!(run_id, "chat_provider is None, returning None");
                return None;
            }
        };

        let run = match self.db.load_run(run_id).ok().flatten() {
            Some(r) => {
                tracing::debug!(run_id, project_id = ?r.project_id(), lane_id = ?r.lane_id(), "Run found in DB");
                r
            }
            None => {
                tracing::debug!(run_id, "Run not found in DB");
                return None;
            }
        };

        let has_project_id = run.project_id().is_some();
        let project = run
            .project_id()
            .and_then(|pid| self.db.load_project(pid.as_str()).ok().flatten());
        tracing::debug!(
            run_id,
            has_project_id,
            project_found = project.is_some(),
            "Project lookup result"
        );

        let lane = run
            .lane_id()
            .and_then(|lid| self.db.load_lane(lid.as_str()).ok().flatten());
        let project_slug = project
            .as_ref()
            .map(|p| p.name.to_lowercase().replace(' ', "-"))
            .unwrap_or_else(|| "default-project".into());
        let lane_slug = lane
            .as_ref()
            .map(|l| l.name.to_lowercase().replace(' ', "-"))
            .unwrap_or_else(|| "default-lane".into());
        let description = run.description().to_string();

        tracing::debug!(
            run_id,
            project_slug = %project_slug,
            lane_slug = %lane_slug,
            "Resolved slugs"
        );

        let project_id = run
            .project_id()
            .map(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        let lane_id = run.lane_id().map(|l| l.as_str()).unwrap_or("").to_string();

        tracing::debug!(run_id, flow_number = ?flow_number, "Using passed-in flow number");

        let ctx = ConversationContext {
            project_id,
            project_slug,
            lane_id,
            lane_slug,
            run_id: run_id.to_string(),
            flow_name: Some(flow_name.to_string()),
            flow_number,
            run_description: Some(description),
            stage_name: None,
            stage_execution_id: None,
        };

        tracing::debug!(
            run_id,
            ctx.project_id = %ctx.project_id,
            ctx.project_slug = %ctx.project_slug,
            ctx.lane_id = %ctx.lane_id,
            ctx.lane_slug = %ctx.lane_slug,
            ctx.run_id = %ctx.run_id,
            ctx.flow_name = ?ctx.flow_name,
            ctx.flow_number = ?ctx.flow_number,
            "Built ConversationContext for create_flow_session"
        );

        tracing::debug!(run_id, provider = %chat_provider.provider_id(), "Calling chat_provider.create_flow_session");
        match chat_provider.create_flow_session(&ctx).await {
            Ok(session) => {
                tracing::debug!(run_id, flow = %flow_name, "chat_provider.create_flow_session returned Ok");

                // Send flow started message
                let flow_display = title_case(flow_name);
                let start_msg = format!("**{}** flow started", flow_display);
                match session.send(&start_msg).await {
                    Ok(()) => {
                        tracing::debug!(run_id, flow = %flow_name, "Flow started message sent successfully");
                    }
                    Err(e) => {
                        tracing::error!(run_id, flow = %flow_name, error = %e, "Failed to send flow started message");
                    }
                }

                // Store in DB for persistence
                let now = chrono::Utc::now().to_rfc3339();
                let provider_key = format!(
                    "{}::{} #{}",
                    ctx.project_slug,
                    title_case(flow_name),
                    flow_number.unwrap_or(0)
                );
                tracing::debug!(run_id, provider_key = %provider_key, "Saving conversation record");
                let record = tj_db::ConversationRecord {
                    id: format!("zulip-{}", uuid::Uuid::new_v4()),
                    run_id: tj_db::RunId(run_id.to_string()),
                    conversation_type: "flow".into(),
                    provider: chat_provider.provider_id().to_string(),
                    provider_key,
                    stage_name: None,
                    session_id: None,
                    created_at: now,
                };
                if let Err(e) = self.db.save_conversation(&record) {
                    tracing::error!(error = %e, "Failed to save flow conversation record");
                }

                tracing::info!(run_id = %run_id, flow = %flow_name, "Flow session created");
                Some(session)
            }
            Err(e) => {
                tracing::error!(run_id, flow = %flow_name, error = %e, "Failed to create flow session via chat_provider");
                let mut source = std::error::Error::source(&e);
                let mut depth = 0u32;
                while let Some(cause) = source {
                    tracing::error!(depth, cause = %cause, "Error chain");
                    source = std::error::Error::source(cause);
                    depth += 1;
                }
                None
            }
        }
    }
}

/// Human-readable summary for the gate_reached event: names the finished flow,
/// carries the last stage's own summary and how many stages produced artifacts,
/// and states which gate now awaits a decision.
fn build_gate_summary(
    flow: &str,
    gate: &str,
    last_stage_summary: Option<String>,
    artifact_count: usize,
) -> String {
    match last_stage_summary {
        Some(s) => format!(
            "{} flow completed — last stage: {} · artifacts from {} stage(s). Awaiting your decision at gate '{}'",
            flow, s, artifact_count, gate
        ),
        None => format!(
            "{} flow completed — artifacts from {} stage(s). Gate: {}",
            flow, artifact_count, gate
        ),
    }
}

#[cfg(test)]
mod gate_summary_tests {
    use super::build_gate_summary;

    #[test]
    fn includes_stage_summary_and_artifacts() {
        let s = build_gate_summary("flow-a", "gate-a", Some("Requirements captured".into()), 2);
        assert_eq!(
            s,
            "flow-a flow completed — last stage: Requirements captured · artifacts from 2 stage(s). Awaiting your decision at gate 'gate-a'"
        );
    }

    #[test]
    fn falls_back_without_stage_summary() {
        let s = build_gate_summary("flow-a", "gate-a", None, 0);
        assert_eq!(
            s,
            "flow-a flow completed — artifacts from 0 stage(s). Gate: gate-a"
        );
    }
}

/// Resolve a run's immutable bundle. A run with a bundle id must have a valid,
/// matching row; a missing/corrupt row never falls back to mutable config.
fn load_execution_bundle(
    db: &Database,
    run: &tj_db::PipelineRun,
) -> Result<Option<ExecutionBundle>> {
    let Some(bundle_id) = run.execution_bundle_id() else {
        return Ok(None);
    };
    let record = db.execution_bundle_for_run(run.id())?.with_context(|| {
        format!(
            "run '{}' references missing execution bundle '{}'",
            run.id(),
            bundle_id
        )
    })?;
    anyhow::ensure!(
        record.id == bundle_id,
        "run '{}' execution bundle id mismatch: run says '{}', store says '{}'",
        run.id(),
        bundle_id,
        record.id
    );
    anyhow::ensure!(
        run.execution_bundle_hash()
            .is_none_or(|hash| hash == record.bundle_hash),
        "run '{}' execution bundle hash mismatch",
        run.id()
    );
    let bundle = ExecutionBundle::from_json(&record.bundle_json)
        .with_context(|| format!("run '{}' execution bundle is invalid", run.id()))?;
    anyhow::ensure!(
        bundle.run_id == run.id(),
        "execution bundle for run '{}' belongs to '{}'",
        run.id(),
        bundle.run_id
    );
    Ok(Some(bundle))
}

/// Ensure a per-run fallback workspace directory exists under workspace_base.
fn fallback_run_workspace(workspace_base: &str, run_id: &str) -> Option<String> {
    let ws = format!("{}/{}", workspace_base.trim_end_matches('/'), run_id);
    match std::fs::create_dir_all(&ws) {
        Ok(()) => {
            tracing::info!(run_id = %run_id, workspace = %ws, "Using per-run fallback workspace");
            Some(ws)
        }
        Err(e) => {
            tracing::error!(run_id = %run_id, workspace = %ws, error = %e, "Failed to create fallback workspace");
            None
        }
    }
}

#[cfg(test)]
mod fallback_workspace_tests {
    use super::fallback_run_workspace;

    #[test]
    fn creates_and_returns_per_run_dir() {
        let base = std::env::temp_dir().join(format!("tj-ws-{}", uuid::Uuid::new_v4()));
        let base_str = base.to_str().unwrap();
        let ws = fallback_run_workspace(base_str, "run_test1").expect("workspace");
        assert_eq!(ws, format!("{}/run_test1", base_str));
        assert!(std::path::Path::new(&ws).is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn trims_trailing_slash_on_base() {
        let base = std::env::temp_dir().join(format!("tj-ws-{}", uuid::Uuid::new_v4()));
        let base_str = format!("{}/", base.to_str().unwrap());
        let ws = fallback_run_workspace(&base_str, "run_test2").expect("workspace");
        assert!(!ws.contains("//"));
        assert!(std::path::Path::new(&ws).is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// Collect relative file paths under root (bounded depth and count).
fn walk_files(root: &PathBuf, dir: &PathBuf, out: &mut Vec<String>, depth: u32) {
    const MAX_DEPTH: u32 = 6;
    const MAX_FILES: usize = 500;
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            walk_files(root, &path, out, depth + 1);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().to_string());
        }
    }
}
