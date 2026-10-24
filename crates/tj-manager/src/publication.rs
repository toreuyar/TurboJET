//! TJ-036/TJ-037: one approved Engineering task through durable draft-PR
//! publication.
//!
//! The coordinator owns remote effects, not the model. Before any push it
//! writes an immutable publication identity (task, plan/packet hashes, base and
//! commit SHA, branch, workspace, test/review/acceptance artifacts). Retries
//! reconcile an unknown/running operation against the remote first. A push
//! that failed is never recorded as success, and a PR is only created after a
//! durable pushed branch/commit exists. Humans merge; this code never marks a
//! PR ready or merges it.

use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use tj_db::plans::PlanTaskSelection;
use tj_db::publications::{NewPublication, PublicationState, RemotePublication};
use tj_db::{Database, RunArtifact};
use tj_git::worktree::WorktreeManager;
use tj_git::{CreatePrOptions, GitProvider, PrStatus};

#[derive(Debug, Clone)]
pub struct RoleEvidence {
    pub role: tj_config::capability::PublicationRole,
    pub stage: String,
    pub artifact: RunArtifact,
    /// Declared execution policy for this evidence role. `Some(Shell)` requires
    /// durable shell activity; absence must not be silently upgraded to it.
    pub execution: Option<tj_config::capability::EvidenceExecution>,
    /// Declared requirement to map evidence to the approved task criteria.
    pub criterion_mapping: bool,
}

/// Evidence loaded for one declared publication. Every role and stage comes
/// from the capability contract; nothing is inferred from the flow name.
#[derive(Debug, Clone)]
pub struct PublicationEvidence {
    pub selection: Option<PlanTaskSelection>,
    pub task: Option<tj_plan::PlanTask>,
    pub plan: Option<tj_db::plans::PlanRevision>,
    pub items: Vec<RoleEvidence>,
    pub changed_files: Vec<String>,
    pub scope_violations: Vec<String>,
}

/// Historical name retained for source compatibility with module callers. It is
/// an alias, not a second evidence model.
pub type EngineeringEvidence = PublicationEvidence;

impl RoleEvidence {
    fn role_stage_key(&self) -> String {
        format!("{:?}:{}", self.role, self.stage)
    }
}

impl PublicationEvidence {
    pub fn role(&self, role: tj_config::capability::PublicationRole) -> Option<&RoleEvidence> {
        self.items.iter().find(|item| item.role == role)
    }

    fn artifact_for(&self, role: tj_config::capability::PublicationRole) -> (String, String) {
        self.role(role)
            .map(|item| (item.artifact.id.clone(), item.artifact.content_hash.clone()))
            .unwrap_or_default()
    }
}

/// Load and validate exactly the declared evidence roles/stages for publication.
/// A declared `requires_task_selection` still requires exactly one approved ready
/// task; a declared shell/test criterion mapping is checked against durable
/// execution activity.
pub fn load_publication_evidence(
    db: &Database,
    run_id: &str,
    flow_name: &str,
    capability: &tj_config::capability::PublicationCapability,
    changed_files: Vec<String>,
) -> Result<PublicationEvidence> {
    let plan = db.approved_plan_revision(run_id)?;
    let selection = match &plan {
        Some(plan) => {
            let selections: Vec<_> = db
                .plan_task_selections(run_id)?
                .into_iter()
                .filter(|selection| selection.plan_revision_id == plan.id)
                .collect();
            match selections.len() {
                1 => Some(selections.into_iter().next().unwrap()),
                0 => None,
                count => anyhow::bail!(
                    "publication requires at most one approved task for this plan revision (found {count})"
                ),
            }
        }
        None => None,
    };
    if capability.requires_task_selection {
        let plan = plan
            .as_ref()
            .context("publication requires an approved structured plan revision")?;
        let selection = selection.as_ref().context(
            "publication requires exactly one approved ready task for this plan revision",
        )?;
        anyhow::ensure!(
            selection.plan_content_hash == plan.content_hash,
            "selected task plan hash does not match the approved plan revision"
        );
    }
    let task = match (&plan, &selection) {
        (Some(plan), Some(selection)) => {
            let plan_content: tj_plan::TaskPlan = tj_plan::TaskPlan::from_json(&plan.content_json)
                .context("approved plan revision is no longer structurally valid")?;
            Some(
                plan_content
                    .task(&selection.task_id)
                    .with_context(|| {
                        format!(
                            "selected task '{}' is missing from the approved plan",
                            selection.task_id
                        )
                    })?
                    .clone(),
            )
        }
        _ => None,
    };
    if let (Some(plan), Some(selection), Some(task)) = (&plan, &selection, &task) {
        let packet = db
            .approved_packet_revision(run_id)?
            .context("approved plan has no approved source packet revision")?;
        let packet_value: Value = serde_json::from_str(&packet.content_json)
            .context("approved source packet content is invalid")?;
        let context = tj_plan::PlanContext::from_packet_value(&packet_value);
        let readiness = plan_content_readiness(plan, &context);
        let task_readiness = readiness
            .iter()
            .find(|candidate| candidate.task_id == selection.task_id)
            .with_context(|| {
                format!(
                    "selected task '{}' has no readiness record",
                    selection.task_id
                )
            })?;
        anyhow::ensure!(
            task_readiness.ready,
            "selected task '{}' is not ready under durable evidence: {}",
            selection.task_id,
            task_readiness.blocked_reasons.join("; ")
        );
        let payload: Value = serde_json::from_str(&selection.payload_json)
            .context("selected task payload is not valid JSON")?;
        anyhow::ensure!(
            payload.pointer("/task/id").and_then(Value::as_str) == Some(selection.task_id.as_str()),
            "selected task payload identity does not match its durable row"
        );
        anyhow::ensure!(
            payload
                .pointer("/plan/content_hash")
                .and_then(Value::as_str)
                == Some(plan.content_hash.as_str()),
            "selected task payload plan hash does not match the approved plan revision"
        );
        anyhow::ensure!(
            payload.pointer("/readiness/ready").and_then(Value::as_bool) == Some(true),
            "selected task was not ready when the publication handoff was recorded"
        );
        let _ = task;
    }

    let artifacts = db.list_artifacts(run_id)?;
    let mut items = Vec::new();
    for required in &capability.required_evidence {
        let artifact = artifacts
            .iter()
            .filter(|artifact| artifact.flow_name == flow_name)
            .filter(|artifact| artifact.stage_name == required.stage)
            .max_by_key(|artifact| (artifact.attempt, artifact.version))
            .cloned()
            .with_context(|| {
                format!(
                    "publication did not produce required '{:?}' evidence at stage '{}'",
                    required.role, required.stage
                )
            })?;
        let output: Value = serde_json::from_str(&artifact.content).with_context(|| {
            format!(
                "'{:?}' evidence artifact at stage '{}' is not structured JSON",
                required.role, required.stage
            )
        })?;
        anyhow::ensure!(
            output.get("verdict").and_then(Value::as_str) == Some("success"),
            "publication cannot publish: '{:?}' evidence at stage '{}' did not report success (artifact {})",
            required.role,
            required.stage,
            artifact.id
        );
        anyhow::ensure!(
            !artifact.content_hash.trim().is_empty(),
            "publication '{:?}' artifact has no content hash",
            required.role
        );
        let needs_criteria = required.criterion_mapping
            || matches!(required.role, tj_config::capability::PublicationRole::Test)
                && required.execution == Some(tj_config::capability::EvidenceExecution::Shell);
        if needs_criteria {
            let acceptance_criteria = task
                .as_ref()
                .map(|task| task.acceptance_criteria.as_slice())
                .unwrap_or(&[]);
            verify_test_execution_evidence(
                db,
                run_id,
                flow_name,
                &artifact,
                acceptance_criteria,
                required.execution == Some(tj_config::capability::EvidenceExecution::Shell),
                required.criterion_mapping,
            )?;
        }
        items.push(RoleEvidence {
            role: required.role,
            stage: required.stage.clone(),
            artifact,
            execution: required.execution,
            criterion_mapping: required.criterion_mapping,
        });
    }

    let scope_violations = task
        .as_ref()
        .map(|task| scope_violations(&task.components, &changed_files))
        .unwrap_or_default();
    Ok(PublicationEvidence {
        selection,
        task,
        plan,
        items,
        changed_files,
        scope_violations,
    })
}

fn plan_content_readiness(
    plan: &tj_db::plans::PlanRevision,
    context: &tj_plan::PlanContext,
) -> Vec<tj_plan::TaskReadiness> {
    match tj_plan::TaskPlan::from_json(&plan.content_json) {
        Ok(content) => content.readiness(&std::collections::HashSet::new(), Some(context)),
        Err(_) => Vec::new(),
    }
}

/// Verify the evidence policy that the workflow actually declared. Shell
/// execution is required only for `execution: "shell"`; acceptance-criterion
/// mapping is required only for `criterion_mapping: true`. A declaration must
/// not silently inherit the historical SDLC's stricter test policy.
///
/// Stage activity is written by the runner, not by the model, so a pure
/// `verdict=success` assertion cannot satisfy a declared shell policy. Commands
/// must have a successful `bash` tool result with exit code 0 in the same
/// flow/stage.
fn verify_test_execution_evidence(
    db: &Database,
    run_id: &str,
    flow_name: &str,
    test: &RunArtifact,
    acceptance_criteria: &[String],
    require_shell: bool,
    require_criterion_mapping: bool,
) -> Result<Vec<String>> {
    let output: Value =
        serde_json::from_str(&test.content).context("'test' artifact is not structured JSON")?;

    if require_criterion_mapping {
        anyhow::ensure!(
            !acceptance_criteria.is_empty(),
            "publication requires criterion mapping but the approved task has no acceptance criteria"
        );
        let mapped: Vec<(String, String)> = output
            .get("criteria_evidence")
            .and_then(Value::as_array)
            .with_context(|| {
                format!(
                    "test artifact {} has no criteria_evidence mapping; test results must map to the approved acceptance criteria",
                    test.id
                )
            })?
            .iter()
            .filter_map(|entry| {
                Some((
                    entry.get("criterion")?.as_str()?.trim().to_string(),
                    entry.get("evidence")?.as_str()?.trim().to_string(),
                ))
            })
            .filter(|(criterion, evidence)| !criterion.is_empty() && !evidence.is_empty())
            .collect();
        for criterion in acceptance_criteria {
            anyhow::ensure!(
                mapped.iter().any(|(mapped, _)| mapped == criterion),
                "test artifact {} does not map evidence to acceptance criterion '{}'",
                test.id,
                criterion
            );
        }
    }

    if !require_shell {
        return Ok(Vec::new());
    }

    let commands: Vec<String> = output
        .get("executed_commands")
        .and_then(Value::as_array)
        .with_context(|| {
            format!(
                "publication test artifact {} does not list executed_commands; a model verdict alone is not execution evidence",
                test.id
            )
        })?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_string)
        .collect();
    anyhow::ensure!(
        !commands.is_empty(),
        "publication test artifact {} has no non-empty executed_commands; a model verdict alone is not execution evidence",
        test.id
    );

    // Stage logs carry the agent definition id while the durable artifact
    // carries the SMG stage key. Resolve the actual stage execution by flow +
    // stage key, then read the durable activity for that stage id. Without
    // this, a real Manager run could never match its own tool execution even
    // though the command succeeded.
    let logs = match db
        .load_flows_for_run(run_id)?
        .into_iter()
        .filter(|flow| flow.flow_name() == flow_name)
        .next_back()
    {
        Some(flow) => match db
            .load_stages_for_flow(flow.id())?
            .into_iter()
            .filter(|stage| stage.stage_name() == test.stage_name)
            .next_back()
        {
            Some(stage) => db.list_stage_logs_for_stage(stage.id(), 10_000)?,
            // Legacy/seeded activity rows may name the stage directly; keep the
            // same run/flow/stage-scoped query as a narrow compatibility path.
            None => db.list_stage_logs_for_flow_stage(run_id, flow_name, &test.stage_name)?,
        },
        None => db.list_stage_logs_for_flow_stage(run_id, flow_name, &test.stage_name)?,
    };
    // Activity retention can leave several attempts under the same stage name.
    // Judge the latest execution only; earlier rework failures are history.
    let latest_stage = logs.last().map(|log| log.stage_id.clone());
    let mut pending: Option<String> = None;
    let mut successful: Vec<String> = Vec::new();
    let mut failed_test_commands: Vec<String> = Vec::new();
    for log in logs {
        if Some(&log.stage_id) != latest_stage.as_ref() {
            continue;
        }
        if log.kind == "tool_call" {
            // Every call replaces the pending command, including non-bash
            // calls, so a later result can never pair with an older command.
            pending = None;
            if let Some(raw) = log.content.strip_prefix("bash ") {
                if let Ok(args) = serde_json::from_str::<Value>(raw) {
                    pending = args
                        .get("command")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|command| !command.is_empty())
                        .map(str::to_string);
                }
            }
            continue;
        }
        if log.kind == "tool_result" {
            if let Some(raw) = log.content.strip_prefix("bash → ") {
                let command = pending.take();
                let result: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
                let success = result.get("success").and_then(Value::as_bool) == Some(true)
                    && result.get("exit_code").and_then(Value::as_i64) == Some(0);
                match command {
                    Some(command) if success => successful.push(command),
                    Some(command) if looks_like_test_command(&command) => {
                        failed_test_commands.push(command)
                    }
                    _ => {}
                }
            } else if log.content.starts_with("bash") {
                pending = None;
            }
        }
    }
    anyhow::ensure!(
        failed_test_commands.is_empty(),
        "publication test stage ran a test/build/check command that failed; a later success cannot hide it: {}",
        failed_test_commands.join("; ")
    );

    for command in &commands {
        anyhow::ensure!(
            successful.iter().any(|executed| executed == command),
            "publication test command '{}' has no successful durable bash execution in flow '{}' stage '{}'; model claims are not accepted",
            command,
            flow_name,
            test.stage_name
        );
        anyhow::ensure!(
            looks_like_test_command(command),
            "publication test command '{}' is not a recognizable test/build/check command; refusing to treat a no-op as test evidence",
            command
        );
    }
    Ok(commands)
}

fn looks_like_test_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let first = lower.split_whitespace().next().unwrap_or_default();
    if matches!(
        first,
        "echo" | "true" | "false" | "printf" | ":" | "cat" | "sleep" | "!"
    ) {
        return false;
    }
    // A failing command hidden behind an always-successful fallback is not
    // evidence that tests passed. Compound `&&` commands still propagate the
    // left-hand exit status and remain acceptable.
    if lower.contains("||") || lower.contains(';') {
        return false;
    }
    [
        "test", "check", "lint", "build", "clippy", "pytest", "cargo", "npm", "pnpm", "yarn",
        "go test", "make", "gradle", "mvn", "ctest", "tox", "rspec", "jest", "vitest",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// A changed path must fall under an explicitly planned component. Components
/// A changed path must fall under an explicitly planned component. Components
/// may be a path prefix (`crates/tj-db`), a file, a glob ending in `/**`, or a
/// bare component name matched against any path segment.
pub fn scope_violations(components: &[String], files: &[String]) -> Vec<String> {
    let normalized: Vec<String> = components
        .iter()
        .map(|component| {
            component
                .trim()
                .trim_start_matches("./")
                .trim_end_matches('/')
                .to_string()
        })
        .filter(|component| !component.is_empty())
        .collect();
    files
        .iter()
        .filter(|file| {
            let file = file.trim_start_matches("./");
            !normalized.iter().any(|component| {
                if component == "**" || component == "*" {
                    return true;
                }
                if let Some(prefix) = component.strip_suffix("/**") {
                    return file == prefix || file.starts_with(&format!("{prefix}/"));
                }
                if component.contains('*') {
                    let prefix = component.split('*').next().unwrap_or_default();
                    return file.starts_with(prefix);
                }
                file == component
                    || file.starts_with(&format!("{component}/"))
                    || (!component.contains('/')
                        && file.split('/').any(|segment| segment == component))
            })
        })
        .cloned()
        .collect()
}

pub struct PublishRequest<'a> {
    pub run_id: &'a str,
    pub flow_name: &'a str,
    pub attempt_id: &'a str,
    pub workspace: &'a Path,
    pub branch: &'a str,
    pub base_branch: &'a str,
    pub base_sha: &'a str,
    pub evidence: &'a EngineeringEvidence,
    pub ssh_env: Option<&'a HashMap<String, String>>,
}

/// Commit, durably record, push and open a draft PR. Reconciliation happens
/// before any retry; the function is safe to call again after restart.
pub async fn publish_draft_pr(
    db: &Database,
    provider: Option<&dyn GitProvider>,
    request: PublishRequest<'_>,
) -> Result<RemotePublication> {
    let evidence = request.evidence;
    anyhow::ensure!(
        !evidence.items.is_empty(),
        "publication requires declared evidence roles"
    );
    for item in &evidence.items {
        let output: Value = serde_json::from_str(&item.artifact.content).with_context(|| {
            format!(
                "'{:?}' evidence artifact at stage '{}' is not structured JSON",
                item.role, item.stage
            )
        })?;
        anyhow::ensure!(
            output.get("verdict").and_then(Value::as_str) == Some("success"),
            "publication refused: '{:?}' evidence at stage '{}' did not report success",
            item.role,
            item.stage
        );
        anyhow::ensure!(
            !item.artifact.content_hash.trim().is_empty(),
            "publication '{:?}' artifact has no content hash",
            item.role
        );
    }
    if let Some(item) = evidence.role(tj_config::capability::PublicationRole::Test) {
        let acceptance_criteria = evidence
            .task
            .as_ref()
            .map(|task| task.acceptance_criteria.as_slice())
            .unwrap_or(&[]);
        verify_test_execution_evidence(
            db,
            request.run_id,
            request.flow_name,
            &item.artifact,
            acceptance_criteria,
            item.execution == Some(tj_config::capability::EvidenceExecution::Shell),
            item.criterion_mapping,
        )?;
    }
    anyhow::ensure!(
        evidence.scope_violations.is_empty(),
        "publication changed files outside the approved task scope: {}",
        evidence.scope_violations.join(", ")
    );
    let provider = provider.context(
        "Draft PR publication requires a configured git provider; unpublished workspace is retained",
    )?;

    let workspace_for_status = request.workspace.to_path_buf();
    let has_changes =
        tokio::task::spawn_blocking(move || WorktreeManager::has_changes(&workspace_for_status))
            .await
            .context("worktree status task panicked")??;
    let commit_sha = if has_changes {
        let task_title = evidence
            .task
            .as_ref()
            .map(|task| task.title.as_str())
            .unwrap_or(request.flow_name);
        let task_id = evidence
            .task
            .as_ref()
            .map(|task| task.id.as_str())
            .unwrap_or("-");
        let plan_hash = evidence
            .plan
            .as_ref()
            .map(|plan| plan.content_hash.as_str())
            .unwrap_or("-");
        let packet_hash = evidence
            .plan
            .as_ref()
            .map(|plan| plan.packet_content_hash.as_str())
            .unwrap_or("-");
        let message = format!(
            "tj: {} for {} ({})\n\nPlan: {}\nPacket: {}\nTask: {}",
            task_title, request.run_id, request.attempt_id, plan_hash, packet_hash, task_id,
        );
        let workspace = request.workspace.to_path_buf();
        tokio::task::spawn_blocking(move || WorktreeManager::commit_all(&workspace, &message))
            .await
            .context("commit task panicked")??
    } else {
        // No new files is not publication success: the approved task still
        // needs a durable commit identity. Use the existing HEAD only if it is
        // ahead of the recorded base.
        let workspace = request.workspace.to_path_buf();
        let head = tokio::task::spawn_blocking(move || WorktreeManager::head_sha(&workspace))
            .await
            .context("head lookup panicked")??;
        anyhow::ensure!(
            head != request.base_sha,
            "approved task produced no repository changes; refusing to publish an empty task"
        );
        head
    };

    let operation_key = publication_operation_key(&request, &commit_sha);
    let existing = db.publication_by_operation_key(&operation_key)?;
    let publication = match existing {
        Some(publication) => publication,
        None => {
            let created_at = chrono::Utc::now().to_rfc3339();
            let (publication, _) = db.create_publication(&NewPublication {
                id: format!("pub_{}", uuid::Uuid::new_v4()),
                run_id: request.run_id.into(),
                flow_name: request.flow_name.into(),
                attempt_id: request.attempt_id.into(),
                operation_key,
                selection_id: evidence
                    .selection
                    .as_ref()
                    .map(|selection| selection.id.clone())
                    .unwrap_or_default(),
                task_id: evidence
                    .selection
                    .as_ref()
                    .map(|selection| selection.task_id.clone())
                    .unwrap_or_default(),
                plan_revision_id: evidence
                    .plan
                    .as_ref()
                    .map(|plan| plan.id.clone())
                    .unwrap_or_default(),
                plan_content_hash: evidence
                    .plan
                    .as_ref()
                    .map(|plan| plan.content_hash.clone())
                    .unwrap_or_default(),
                packet_revision_id: evidence
                    .plan
                    .as_ref()
                    .map(|plan| plan.packet_revision_id.clone())
                    .unwrap_or_default(),
                packet_content_hash: evidence
                    .plan
                    .as_ref()
                    .map(|plan| plan.packet_content_hash.clone())
                    .unwrap_or_default(),
                branch: request.branch.into(),
                base_branch: request.base_branch.into(),
                workspace_path: request.workspace.display().to_string(),
                base_sha: Some(request.base_sha.into()),
                commit_sha: Some(commit_sha.clone()),
                test_artifact_id: evidence
                    .artifact_for(tj_config::capability::PublicationRole::Test)
                    .0,
                test_artifact_hash: evidence
                    .artifact_for(tj_config::capability::PublicationRole::Test)
                    .1,
                review_artifact_id: evidence
                    .artifact_for(tj_config::capability::PublicationRole::Review)
                    .0,
                review_artifact_hash: evidence
                    .artifact_for(tj_config::capability::PublicationRole::Review)
                    .1,
                acceptance_artifact_id: evidence
                    .artifact_for(tj_config::capability::PublicationRole::Acceptance)
                    .0,
                acceptance_artifact_hash: evidence
                    .artifact_for(tj_config::capability::PublicationRole::Acceptance)
                    .1,
                scope_files_json: serde_json::to_string(&evidence.changed_files)?,
                scope_violations_json: serde_json::to_string(&evidence.scope_violations)?,
                evidence_json: Some(evidence_json(evidence)?),
                draft: true,
                created_at,
            })?;
            publication
        }
    };

    // Reconcile a prior attempt against the remote before deciding to retry.
    let mut publication =
        reconcile_push(db, &publication, request.workspace, request.ssh_env).await?;
    if publication.push_state != PublicationState::Succeeded {
        db.mark_push_running(&publication.id)?;
        let workspace = request.workspace.to_path_buf();
        let branch = request.branch.to_string();
        let env = request.ssh_env.cloned();
        let push = tokio::task::spawn_blocking(move || {
            WorktreeManager::push_with_env(&workspace, &branch, env.as_ref())
        })
        .await
        .context("push task panicked")?;
        match push {
            Ok(()) => {
                let workspace = request.workspace.to_path_buf();
                let branch = request.branch.to_string();
                let env = request.ssh_env.cloned();
                let remote = tokio::task::spawn_blocking(move || {
                    WorktreeManager::ls_remote_branch(&workspace, &branch, env.as_ref())
                })
                .await
                .context("remote verification panicked")??;
                match remote {
                    Some(sha) if sha == commit_sha => db.complete_push(&publication.id, &sha)?,
                    Some(sha) => {
                        let error = format!(
                            "remote branch '{}' points at {sha}, expected commit {commit_sha}; refusing to overwrite",
                            request.branch
                        );
                        db.mark_push_unknown(&publication.id, &error)?;
                        anyhow::bail!(error);
                    }
                    None => {
                        let error = "push command succeeded but the remote branch is absent; publication is unproven";
                        db.mark_push_unknown(&publication.id, error)?;
                        anyhow::bail!("{error}");
                    }
                }
            }
            Err(error) => {
                // A lost response may still have landed. Reconcile once before
                // classifying the push as failed.
                let workspace = request.workspace.to_path_buf();
                let branch = request.branch.to_string();
                let env = request.ssh_env.cloned();
                let remote = tokio::task::spawn_blocking(move || {
                    WorktreeManager::ls_remote_branch(&workspace, &branch, env.as_ref())
                })
                .await
                .context("remote verification after push failure panicked")?;
                match remote {
                    Ok(Some(sha)) if sha == commit_sha => {
                        db.complete_push(&publication.id, &sha)?;
                    }
                    Ok(Some(sha)) => {
                        db.mark_push_unknown(&publication.id, &format!(
                            "push failed and remote branch points at {sha}, expected {commit_sha}"
                        ))?;
                        anyhow::bail!("push failed and remote branch has a conflicting SHA");
                    }
                    Ok(None) | Err(_) => {
                        db.fail_push(&publication.id, &error.to_string())?;
                        return Err(error)
                            .context("remote push failed; unpublished workspace retained");
                    }
                }
            }
        }
        publication = db
            .publication(&publication.id)?
            .context("publication disappeared")?;
    }

    // PR reconciliation: never create a second PR after a lost response.
    if publication.pr_state == PublicationState::Succeeded {
        return Ok(publication);
    }
    if let Some(existing) = provider.find_pr(request.branch).await? {
        if existing.status != PrStatus::Open || !existing.draft {
            let reason = format!(
                "existing PR {} for branch '{}' is {:?} and draft={}; refusing to claim draft-PR success",
                existing.number.unwrap_or(0),
                request.branch,
                existing.status,
                existing.draft
            );
            db.mark_pr_unknown(&publication.id, &reason)?;
            anyhow::bail!(reason);
        }
        let number = existing.number.context(
            "existing provider PR has no number; refusing to claim draft-PR success without an exact PR identity",
        )?;
        db.complete_pr(&publication.id, number, &existing.url)?;
        return Ok(db.publication(&publication.id)?.unwrap());
    }
    db.mark_pr_running(&publication.id)?;
    let body = draft_pr_body(&publication, evidence);
    let result = provider
        .create_pr(CreatePrOptions {
            repo_path: request.workspace.to_path_buf(),
            source_branch: request.branch.to_string(),
            target_branch: request.base_branch.to_string(),
            title: format!(
                "[{}] {}",
                evidence
                    .task
                    .as_ref()
                    .map(|task| task.id.as_str())
                    .unwrap_or(publication.task_id.as_str()),
                evidence
                    .task
                    .as_ref()
                    .map(|task| task.title.as_str())
                    .unwrap_or(request.flow_name)
            ),
            body,
            labels: vec![],
            reviewers: vec![],
            draft: true,
        })
        .await;
    match result {
        Ok(pr) => {
            anyhow::ensure!(
                pr.status == PrStatus::Open,
                "provider did not return an open draft PR"
            );
            db.complete_pr(&publication.id, pr.number.unwrap_or(0), &pr.url)?;
        }
        Err(error) => {
            db.mark_pr_unknown(&publication.id, &error.to_string())?;
            // The HTTP response may have been lost after the remote accepted
            // the PR. Look it up before reporting failure to the retry path.
            if let Ok(Some(existing)) = provider.find_pr(request.branch).await
                && existing.status == PrStatus::Open
                && existing.draft
            {
                db.complete_pr(&publication.id, existing.number.unwrap_or(0), &existing.url)?;
            } else {
                db.fail_publication(&publication.id, "pr", &error.to_string())?;
                return Err(error)
                    .context("draft PR creation failed; pushed branch and workspace retained");
            }
        }
    }
    Ok(db
        .publication(&publication.id)?
        .context("publication disappeared after PR")?)
}

async fn reconcile_push(
    db: &Database,
    publication: &RemotePublication,
    workspace: &Path,
    env: Option<&HashMap<String, String>>,
) -> Result<RemotePublication> {
    let commit = publication
        .commit_sha
        .clone()
        .context("publication has no commit SHA")?;
    let workspace = workspace.to_path_buf();
    let branch = publication.branch.clone();
    let env = env.cloned();
    let remote = tokio::task::spawn_blocking(move || {
        WorktreeManager::ls_remote_branch(&workspace, &branch, env.as_ref())
    })
    .await
    .context("push reconciliation panicked")??;
    match remote {
        Some(sha) if sha == commit => {
            if publication.push_state != PublicationState::Succeeded {
                db.complete_push(&publication.id, &sha)?;
            }
        }
        Some(sha) => {
            let error = format!(
                "remote branch '{}' has SHA {sha}, expected {commit}",
                publication.branch
            );
            db.mark_push_unknown(&publication.id, &error)?;
            anyhow::bail!("{error}");
        }
        None if publication.push_state == PublicationState::Succeeded => {
            let error = "publication claims a pushed branch but the remote branch is absent";
            db.mark_push_unknown(&publication.id, error)?;
            anyhow::bail!("{error}");
        }
        None => {
            // Reconciliation proved the previous attempt did not land. Clear
            // the stale error; the caller may retry exactly the same operation.
            if publication.push_state == PublicationState::Unknown {
                db.fail_push(
                    &publication.id,
                    "remote branch absent after an uncertain push; retry allowed",
                )?;
            }
        }
    }
    db.publication(&publication.id)?
        .context("publication disappeared during reconciliation")
}

fn evidence_json(evidence: &PublicationEvidence) -> Result<String> {
    let roles: Vec<Value> = evidence
        .items
        .iter()
        .map(|item| {
            serde_json::json!({
                "role": item.role,
                "stage": item.stage,
                "artifact_id": item.artifact.id,
                "artifact_version": item.artifact.version,
                "content_hash": item.artifact.content_hash,
            })
        })
        .collect();
    Ok(serde_json::to_string(
        &serde_json::json!({ "roles": roles }),
    )?)
}

pub fn publication_operation_key(request: &PublishRequest<'_>, commit_sha: &str) -> String {
    let selection_id = request
        .evidence
        .selection
        .as_ref()
        .map(|selection| selection.id.as_str())
        .unwrap_or("");
    let task_id = request
        .evidence
        .selection
        .as_ref()
        .map(|selection| selection.task_id.as_str())
        .unwrap_or("");
    let plan_id = request
        .evidence
        .plan
        .as_ref()
        .map(|plan| plan.id.as_str())
        .unwrap_or("");
    let plan_hash = request
        .evidence
        .plan
        .as_ref()
        .map(|plan| plan.content_hash.as_str())
        .unwrap_or("");
    let packet_hash = request
        .evidence
        .plan
        .as_ref()
        .map(|plan| plan.packet_content_hash.as_str())
        .unwrap_or("");
    let mut hasher = Sha256::new();
    for value in [
        request.run_id,
        request.flow_name,
        request.attempt_id,
        selection_id,
        plan_id,
        plan_hash,
        packet_hash,
        task_id,
        request.branch,
        request.base_sha,
        commit_sha,
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0u8]);
    }
    for item in &request.evidence.items {
        hasher.update(item.role_stage_key().as_bytes());
        hasher.update([0u8]);
        hasher.update(item.artifact.id.as_bytes());
        hasher.update([0u8]);
        hasher.update(item.artifact.content_hash.as_bytes());
        hasher.update([0u8]);
    }
    format!("pubop:{}", hex::encode(hasher.finalize()))
}

fn draft_pr_body(publication: &RemotePublication, evidence: &EngineeringEvidence) -> String {
    let criteria = evidence
        .task
        .as_ref()
        .map(|task| {
            task.acceptance_criteria
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let verification = evidence
        .task
        .as_ref()
        .map(|task| {
            task.verification
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let files = evidence
        .changed_files
        .iter()
        .map(|item| format!("- `{item}`"))
        .collect::<Vec<_>>()
        .join("\n");
    let roles = evidence
        .items
        .iter()
        .map(|item| {
            format!(
                "- {:?} evidence ('{}'): `{}` ({})",
                item.role, item.stage, item.artifact.id, item.artifact.content_hash
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Draft PR opened by TurboJet for one approved task.\n\n\
         ## Identity\n\
         - Task: `{task}` — {title}\n\
         - Run / attempt: `{run}` / `{attempt}`\n\
         - Branch: `{branch}` → `{base}`\n\
         - Commit: `{commit}`\n\
         - Plan revision/hash: `{plan}` / `{plan_hash}`\n\
         - Packet revision/hash: `{packet}` / `{packet_hash}`\n\n\
         ## Acceptance criteria\n{criteria}\n\n\
         ## Verification\n{verification}\n\n\
         ## Declared execution evidence\n{roles}\n\
         - Every declared evidence role reported `success`; unresolved major findings cannot reach this draft.\n\n\
         ## Changed files\n{files}\n\n\
         ## Limitations\n\
         - This PR is a draft. A human reviewer owns review and merge; TurboJet never merges or marks it ready.\n\
         - Scope is limited to the approved task components; no unreviewed expansion is included.\n\
         - Model/provider/live-forge limitations are recorded in the run evidence.\n",
        task = publication.task_id,
        title = evidence
            .task
            .as_ref()
            .map(|task| task.title.as_str())
            .unwrap_or("(workflow flow)"),
        run = publication.run_id,
        attempt = publication.attempt_id,
        branch = publication.branch,
        base = publication.base_branch,
        commit = publication.commit_sha.as_deref().unwrap_or("—"),
        plan = publication.plan_revision_id,
        plan_hash = publication.plan_content_hash,
        packet = publication.packet_revision_id,
        packet_hash = publication.packet_content_hash,
        roles = if roles.is_empty() {
            "- (none)".into()
        } else {
            roles
        },
        criteria = if criteria.is_empty() {
            "- (none)".into()
        } else {
            criteria
        },
        verification = if verification.is_empty() {
            "- (none)".into()
        } else {
            verification
        },
        files = if files.is_empty() {
            "- (no changed files)".into()
        } else {
            files
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_requires_explicit_component_match() {
        let components = vec![
            "crates/tj-db".into(),
            "docs/checks.md".into(),
            "webui".into(),
        ];
        let files = vec![
            "crates/tj-db/src/lib.rs".into(),
            "docs/checks.md".into(),
            "webui/src/App.tsx".into(),
        ];
        assert!(scope_violations(&components, &files).is_empty());
        assert_eq!(
            scope_violations(&components, &["Cargo.toml".into()]),
            vec!["Cargo.toml".to_string()]
        );
    }

    #[test]
    fn test_command_evidence_rejects_noops_and_hidden_failures() {
        assert!(looks_like_test_command("cargo test --workspace"));
        assert!(looks_like_test_command("npm run lint"));
        assert!(!looks_like_test_command("echo cargo test"));
        assert!(!looks_like_test_command("true"));
        assert!(!looks_like_test_command("cargo test || true"));
        assert!(!looks_like_test_command("cargo test; echo done"));
        assert!(!looks_like_test_command("sleep 30"));
    }

    #[test]
    fn evidence_roles_and_stages_are_declaration_data_not_names() {
        let selection = PlanTaskSelection {
            id: "sel".into(),
            run_id: "run".into(),
            plan_id: "plan".into(),
            plan_revision_id: "rev".into(),
            plan_content_hash: "a".repeat(64),
            task_id: "T-1".into(),
            selected_by: "alice".into(),
            created_at: "now".into(),
            payload_json: "{}".into(),
        };
        let artifact = |id: &str| RunArtifact {
            id: id.into(),
            run_id: tj_db::RunId("run".into()),
            flow_name: "deliver-code".into(),
            stage_name: "compile-check".into(),
            attempt: 0,
            version: 1,
            content: "{\"verdict\":\"success\"}".into(),
            content_hash: "c".repeat(64),
            created_at: "now".into(),
        };
        let evidence = EngineeringEvidence {
            selection: Some(selection),
            task: None,
            plan: None,
            items: vec![RoleEvidence {
                role: tj_config::capability::PublicationRole::Test,
                stage: "compile-check".into(),
                artifact: artifact("artifact-a"),
                execution: None,
                criterion_mapping: false,
            }],
            changed_files: vec![],
            scope_violations: vec![],
        };
        let request = PublishRequest {
            run_id: "run",
            flow_name: "deliver-code",
            attempt_id: "attempt",
            workspace: Path::new("/ws"),
            branch: "tj/run",
            base_branch: "main",
            base_sha: "base",
            evidence: &evidence,
            ssh_env: None,
        };
        let encoded: Value = serde_json::from_str(&evidence_json(&evidence).unwrap()).unwrap();
        assert_eq!(encoded["roles"][0]["stage"], "compile-check");
        assert_eq!(encoded["roles"][0]["role"], "test");
        let key = publication_operation_key(&request, "commit");
        let mut renamed = evidence.clone();
        renamed.items[0].stage = "totally-different-stage".into();
        let request = PublishRequest {
            run_id: "run",
            flow_name: "deliver-code",
            attempt_id: "attempt",
            workspace: Path::new("/ws"),
            branch: "tj/run",
            base_branch: "main",
            base_sha: "base",
            evidence: &renamed,
            ssh_env: None,
        };
        assert_ne!(key, publication_operation_key(&request, "commit"));
    }

    #[test]
    fn declared_test_policy_controls_shell_and_criterion_validation() {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::open(temp.path().join("state.db").to_str().unwrap()).unwrap();
        db.append_artifact(&RunArtifact {
            id: "artifact-check".into(),
            run_id: tj_db::RunId("run".into()),
            flow_name: "ship-it".into(),
            stage_name: "check".into(),
            attempt: 0,
            version: 1,
            content: "{\"verdict\":\"success\",\"content\":\"checked\"}".into(),
            content_hash: "d".repeat(64),
            created_at: "now".into(),
        })
        .unwrap();
        let mut capability: tj_config::capability::PublicationCapability =
            serde_json::from_value(serde_json::json!({
                "flow": "ship-it",
                "required_evidence": [{"role": "test", "stage": "check"}]
            }))
            .unwrap();

        // No declared shell/criterion policy: success verdict is the whole
        // contract and the loader must not inherit SDLC-only checks.
        load_publication_evidence(&db, "run", "ship-it", &capability, vec![]).unwrap();

        capability.required_evidence[0].execution =
            Some(tj_config::capability::EvidenceExecution::Shell);
        let error =
            load_publication_evidence(&db, "run", "ship-it", &capability, vec![]).unwrap_err();
        assert!(
            format!("{error:#}").contains("executed_commands"),
            "{error:#}"
        );

        capability.required_evidence[0].execution = None;
        capability.required_evidence[0].criterion_mapping = true;
        let error =
            load_publication_evidence(&db, "run", "ship-it", &capability, vec![]).unwrap_err();
        assert!(
            format!("{error:#}").contains("no acceptance criteria"),
            "{error:#}"
        );
    }

    #[test]
    fn plan_less_publication_loads_declared_evidence_without_task_identity() {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::open(temp.path().join("state.db").to_str().unwrap()).unwrap();
        db.append_artifact(&RunArtifact {
            id: "artifact-review".into(),
            run_id: tj_db::RunId("run".into()),
            flow_name: "ship-it".into(),
            stage_name: "inspect".into(),
            attempt: 0,
            version: 1,
            content: "{\"verdict\":\"success\",\"content\":\"reviewed\"}".into(),
            content_hash: "c".repeat(64),
            created_at: "now".into(),
        })
        .unwrap();
        let capability: tj_config::capability::PublicationCapability =
            serde_json::from_value(serde_json::json!({
                "flow": "ship-it",
                "required_evidence": [{"role": "review", "stage": "inspect"}]
            }))
            .unwrap();
        let evidence = load_publication_evidence(
            &db,
            "run",
            "ship-it",
            &capability,
            vec!["src/new.rs".into()],
        )
        .unwrap();
        assert!(evidence.plan.is_none());
        assert!(evidence.selection.is_none());
        assert!(evidence.task.is_none());
        assert_eq!(evidence.items.len(), 1);
        assert_eq!(
            evidence.items[0].role,
            tj_config::capability::PublicationRole::Review
        );
        assert_eq!(evidence.items[0].artifact.id, "artifact-review");
        // Plan-less publication does not invent a task scope. It also does not
        // claim criterion mapping; validation refuses that combination.
        assert!(evidence.scope_violations.is_empty());
    }

    #[test]
    fn operation_key_is_stable_and_identity_scoped() {
        let selection = PlanTaskSelection {
            id: "sel".into(),
            run_id: "run".into(),
            plan_id: "plan".into(),
            plan_revision_id: "rev".into(),
            plan_content_hash: "a".repeat(64),
            task_id: "T-1".into(),
            selected_by: "alice".into(),
            created_at: "now".into(),
            payload_json: "{}".into(),
        };
        let plan = tj_db::plans::PlanRevision {
            id: "rev".into(),
            plan_id: "plan".into(),
            run_id: "run".into(),
            revision: 1,
            schema_version: 1,
            content_json: "{}".into(),
            content_hash: "a".repeat(64),
            packet_revision_id: "pkt".into(),
            packet_content_hash: "b".repeat(64),
            packet_id: "packet".into(),
            packet_revision: 1,
            parent_revision_id: None,
            source_artifact_id: None,
            source_artifact_hash: None,
            flow_name: None,
            stage_name: None,
            change_request: None,
            validation_json: "{}".into(),
            created_by: "alice".into(),
            created_at: "now".into(),
        };
        let task = tj_plan::PlanTask {
            id: "T-1".into(),
            title: "t".into(),
            objective: "o".into(),
            requirement_refs: vec![],
            enabling_rationale: Some("e".into()),
            decision_refs: vec![],
            dependencies: vec![],
            acceptance_criteria: vec!["a".into()],
            components: vec!["src".into()],
            estimate: tj_plan::TaskEstimate {
                size: "s".into(),
                uncertainty: "low".into(),
                rationale: None,
            },
            risks: vec![],
            verification: vec![],
            notes: None,
        };
        let artifact = |stage: &str| RunArtifact {
            id: stage.into(),
            run_id: tj_db::RunId("run".into()),
            flow_name: "flow-a".into(),
            stage_name: stage.into(),
            attempt: 0,
            version: 1,
            content: "{\"verdict\":\"success\"}".into(),
            content_hash: "c".repeat(64),
            created_at: "now".into(),
        };
        let evidence = EngineeringEvidence {
            selection: Some(selection),
            task: Some(task),
            plan: Some(plan),
            items: vec![
                RoleEvidence {
                    role: tj_config::capability::PublicationRole::Test,
                    stage: "evidence-a".into(),
                    artifact: artifact("evidence-a"),
                    execution: Some(tj_config::capability::EvidenceExecution::Shell),
                    criterion_mapping: true,
                },
                RoleEvidence {
                    role: tj_config::capability::PublicationRole::Review,
                    stage: "evidence-b".into(),
                    artifact: artifact("evidence-b"),
                    execution: None,
                    criterion_mapping: false,
                },
                RoleEvidence {
                    role: tj_config::capability::PublicationRole::Acceptance,
                    stage: "evidence-c".into(),
                    artifact: artifact("evidence-c"),
                    execution: None,
                    criterion_mapping: false,
                },
            ],
            changed_files: vec![],
            scope_violations: vec![],
        };
        let request = PublishRequest {
            run_id: "run",
            flow_name: "flow-a",
            attempt_id: "attempt",
            workspace: Path::new("/ws"),
            branch: "tj/run",
            base_branch: "main",
            base_sha: "base",
            evidence: &evidence,
            ssh_env: None,
        };
        let a = publication_operation_key(&request, "commit");
        let b = publication_operation_key(&request, "commit");
        assert_eq!(a, b);
        let c = publication_operation_key(&request, "other");
        assert_ne!(a, c);
    }
}
