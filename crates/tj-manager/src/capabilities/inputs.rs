//! Declared project-input copy module.
//!
//! The destination and the fact that inputs are wanted both come from the
//! frozen capability contract. No flow, stage, gate or profile name is read.

use anyhow::{Context, Result};
use std::path::Path;
use tj_config::capability::resolve_within_workspace;
use tj_db::Database;

/// Copy regular files from `<project workspace>/inputs/` into the declared
/// run-workspace destination. Existing run files are never overwritten, symlinks
/// are never followed, and a destination outside the workspace fails closed.
pub fn copy_project_inputs(
    db: &Database,
    run_id: &str,
    workspace: &Path,
    destination: &str,
) -> Result<usize> {
    let project = db
        .load_run(run_id)?
        .and_then(|run| run.project_id().map(|id| id.as_str().to_string()))
        .and_then(|id| db.load_project(&id).ok().flatten());
    let Some(project) = project else {
        return Ok(0);
    };
    let Some(project_ws) = project.workspace_path.as_deref() else {
        return Ok(0);
    };
    let source = Path::new(project_ws).join("inputs");
    let entries = match std::fs::read_dir(&source) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", source.display()));
        }
    };
    let dest = resolve_within_workspace(workspace, destination)?;
    std::fs::create_dir_all(&dest)
        .with_context(|| format!("cannot create input destination {}", dest.display()))?;
    let mut copied = 0usize;
    for entry in entries {
        let entry = entry.context("cannot read a project input entry")?;
        let kind = entry
            .file_type()
            .with_context(|| format!("cannot inspect {}", entry.path().display()))?;
        if !kind.is_file() {
            continue;
        }
        let name = entry.file_name();
        let target = dest.join(&name);
        if target.exists() {
            continue;
        }
        std::fs::copy(entry.path(), &target).with_context(|| {
            format!(
                "cannot copy project input {} to {}",
                entry.path().display(),
                target.display()
            )
        })?;
        copied += 1;
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tj_db::{LaneId, PipelineRun, Project, ProjectId, RunStatus};

    fn run_with_inputs(root: &Path) -> (Database, String) {
        let db = Database::open(root.join("db.sqlite").to_str().unwrap()).unwrap();
        let project_ws = root.join("project");
        std::fs::create_dir_all(project_ws.join("inputs")).unwrap();
        std::fs::write(project_ws.join("inputs/notes.md"), "local evidence").unwrap();
        std::fs::create_dir_all(project_ws.join("inputs/nested")).unwrap();
        db.save_project(&Project {
            id: "proj".into(),
            name: "p".into(),
            repo_url: None,
            base_branch: "main".into(),
            workspace_path: Some(project_ws.display().to_string()),
            git_provider: None,
            profile: "arbitrary".into(),
            workspace_kind: Some("none".into()),
            catalog_id: None,
            provisioning_state: Some("not_required".into()),
            provisioning_error: None,
            created_at: "now".into(),
            updated_at: "now".into(),
        })
        .unwrap();
        let run_id = "run_inputs".to_string();
        db.save_run(&PipelineRun::new(
            run_id.clone(),
            Some(ProjectId("proj".into())),
            Some(LaneId("lane".into())),
            "copy inputs".into(),
            RunStatus::Running,
            None,
            Some("main".into()),
            Some("trace".into()),
            None,
        ))
        .unwrap();
        (db, run_id)
    }

    #[test]
    fn copies_only_regular_files_to_the_declared_destination() {
        let temp = tempfile::tempdir().unwrap();
        let (db, run_id) = run_with_inputs(temp.path());
        let workspace = temp.path().join("workspace");
        let copied = copy_project_inputs(&db, &run_id, &workspace, "supplied/evidence").unwrap();
        assert_eq!(copied, 1);
        assert_eq!(
            std::fs::read_to_string(workspace.join("supplied/evidence/notes.md")).unwrap(),
            "local evidence"
        );
        assert!(!workspace.join("supplied/evidence/nested").exists());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path(), workspace.join("outside")).unwrap();
            let error = copy_project_inputs(&db, &run_id, &workspace, "outside/inputs")
                .unwrap_err()
                .to_string();
            assert!(error.contains("escapes"), "{error}");
        }
    }
}
