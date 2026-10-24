//! Declared local-citation consistency verification.
//!
//! This proves only that each citation names a relative file under the declared
//! input directory and that the quoted excerpt actually appears in that file.
//! It is not semantic truth or source trust. Names of stages and the input
//! directory are declaration data, never inferred.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use tj_config::capability::{VerificationCapability, resolve_within_workspace};

/// Validate the declared producer artifact and every artifact of the same flow
/// that carries a citations array. Any escape, missing file, missing excerpt or
/// empty citation fails with a citation-consistency error.
pub fn validate_local_citations(
    capability: &VerificationCapability,
    workspace: &Path,
    artifacts: &HashMap<String, serde_json::Value>,
) -> Result<()> {
    let inputs_dir = capability.inputs_dir.as_deref().unwrap_or("inputs");
    let inputs = resolve_within_workspace(workspace, inputs_dir)?;
    let canonical_inputs = std::fs::canonicalize(&inputs).with_context(|| {
        format!(
            "supplied input directory is unavailable: {}",
            inputs.display()
        )
    })?;
    let producer_stage = capability
        .producer_stage
        .as_deref()
        .context("verification.local_citations requires producer_stage")?;
    let producer = artifacts.get(producer_stage).with_context(|| {
        format!(
            "declared verification producer stage '{}' produced no artifact",
            producer_stage
        )
    })?;

    let mut checked_groups: Vec<(&str, &Vec<serde_json::Value>)> = Vec::new();
    let citations = producer
        .get("citations")
        .and_then(|value| value.as_array())
        .with_context(|| format!("declared producer '{producer_stage}' has no citations array"))?;
    anyhow::ensure!(
        !citations.is_empty(),
        "declared producer '{producer_stage}' has no source-backed citations"
    );
    checked_groups.push((producer_stage, citations));
    for (stage, value) in artifacts {
        if stage == producer_stage {
            continue;
        }
        if let Some(citations) = value.get("citations").and_then(|value| value.as_array()) {
            checked_groups.push((stage.as_str(), citations));
        }
    }

    for (group, citations) in checked_groups {
        for (index, citation) in citations.iter().enumerate() {
            let label = format!("{group} citation {index}");
            let path = citation
                .get("path")
                .and_then(|value| value.as_str())
                .with_context(|| format!("citation consistency failed: {label} has no path"))?;
            let excerpt = citation
                .get("excerpt")
                .and_then(|value| value.as_str())
                .with_context(|| format!("citation consistency failed: {label} has no excerpt"))?;
            anyhow::ensure!(
                !path.trim().is_empty() && !excerpt.trim().is_empty(),
                "citation consistency failed: {label} has an empty path or excerpt"
            );
            anyhow::ensure!(
                !path.contains("://") && !Path::new(path).is_absolute(),
                "citation consistency failed: {label} must be a relative local input path, not an external source"
            );
            let relative = Path::new(path);
            anyhow::ensure!(
                !relative
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)),
                "citation consistency failed: {label} attempts to leave the input directory"
            );
            let candidate = workspace.join(relative);
            let canonical = std::fs::canonicalize(&candidate).with_context(|| {
                format!("citation consistency failed: {label} path does not exist: {path}")
            })?;
            anyhow::ensure!(
                canonical.starts_with(&canonical_inputs),
                "citation consistency failed: {label} points outside {inputs_dir}: {path}"
            );
            let meta = std::fs::metadata(&canonical)?;
            anyhow::ensure!(
                meta.is_file() && meta.len() <= 2_000_000,
                "citation consistency failed: {label} is not a readable input file (or is too large): {path}"
            );
            let text = std::fs::read_to_string(&canonical).with_context(|| {
                format!("citation consistency failed: {label} input is not readable text: {path}")
            })?;
            anyhow::ensure!(
                text.contains(excerpt),
                "citation consistency failed: {label} excerpt does not appear in the cited file: {path}"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tj_config::capability::{VerificationCapability, VerificationKind};

    #[test]
    fn validates_declared_stage_and_input_directory_without_name_inference() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path();
        std::fs::create_dir_all(workspace.join("supplied")).unwrap();
        std::fs::write(
            workspace.join("supplied/pricing.md"),
            "Monthly cost starts at 400 EUR",
        )
        .unwrap();
        let capability = VerificationCapability {
            kind: VerificationKind::LocalCitations,
            flow: "any-flow".into(),
            producer_stage: Some("draft".into()),
            inputs_dir: Some("supplied".into()),
        };
        let mut artifacts = HashMap::new();
        artifacts.insert(
            "draft".to_string(),
            serde_json::json!({
                "citations": [{
                    "path": "supplied/pricing.md",
                    "excerpt": "Monthly cost starts at 400 EUR"
                }]
            }),
        );
        validate_local_citations(&capability, workspace, &artifacts).unwrap();

        artifacts.insert(
            "draft".to_string(),
            serde_json::json!({
                "citations": [{"path": "supplied/missing.md", "excerpt": "x"}]
            }),
        );
        let error = validate_local_citations(&capability, workspace, &artifacts)
            .unwrap_err()
            .to_string();
        assert!(error.contains("citation consistency failed"), "{error}");

        let mut escape = HashMap::new();
        escape.insert(
            "draft".to_string(),
            serde_json::json!({
                "citations": [{"path": "../outside.md", "excerpt": "x"}]
            }),
        );
        let error = validate_local_citations(&capability, workspace, &escape)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("escape") || error.contains("leave"),
            "{error}"
        );
    }
}
