//! Registry-facing workflow catalog metadata.
//!
//! Catalog metadata is embedded in the workflow document so a graph's identity
//! travels with its content hash. It is intentionally *not* a static profile
//! table: startup, the API catalog, project selection and the UI all derive
//! their choices from imported registry rows.

use serde::{Deserialize, Serialize};

use crate::smg::StateMachineGraph;

/// Optional `catalog` block in a workflow document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowCatalog {
    /// Stable operator-facing identity across revisions.
    pub id: String,
    /// Display label. Defaults to the workflow name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Display description. Defaults to the workflow description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Explicit project-creation default. At most one imported workflow may set
    /// this; order and filename never imply a default.
    #[serde(default)]
    pub default: bool,
}

impl WorkflowCatalog {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            is_valid_catalog_id(&self.id),
            "invalid catalog.id '{}': use lowercase letters, digits, '.', '_' or '-' and start with a letter or digit",
            self.id
        );
        Ok(())
    }

    pub fn label_or<'a>(&'a self, workflow_name: &'a str) -> &'a str {
        self.label.as_deref().unwrap_or(workflow_name)
    }

    pub fn description_or<'a>(&'a self, workflow_description: &'a str) -> &'a str {
        self.description.as_deref().unwrap_or(workflow_description)
    }
}

/// Validate the import-time catalog contract. Structural workflow validation is
/// separate (`ConfigLoader::validate_smg`); this is the migration gate that
/// prevents an old graph from silently becoming a new generic workflow.
pub fn validate_for_source_import(graph: &StateMachineGraph) -> anyhow::Result<&WorkflowCatalog> {
    let catalog = graph.catalog.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "workflow '{}' has no catalog declaration; add catalog.id (for example \"catalog\": {{\"id\": \"{}\"}}) and re-import. Old graphs are never admitted implicitly.",
            graph.name,
            suggested_catalog_id(&graph.name)
        )
    })?;
    catalog.validate()?;
    Ok(catalog)
}

pub fn is_valid_catalog_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// Suggest a valid id from arbitrary text without ever using it as the source
/// of truth. This is only used in error guidance.
pub fn suggested_catalog_id(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        let normalized = if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            ch
        } else {
            '-'
        };
        if normalized == '-' {
            if !out.is_empty() && !last_dash {
                out.push('-');
                last_dash = true;
            }
        } else {
            out.push(normalized);
            last_dash = false;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "workflow".into()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_stable_operator_identifiers() {
        for id in ["a", "team-review", "team.review_2", "0flow"] {
            assert!(is_valid_catalog_id(id), "{id}");
        }
        for id in ["", "-a", "A", "a/b", "a b", "_a", "a:b"] {
            assert!(!is_valid_catalog_id(id), "{id}");
        }
    }

    #[test]
    fn labels_and_descriptions_default_without_rewriting_the_graph() {
        let catalog = WorkflowCatalog {
            id: "team-review".into(),
            label: None,
            description: None,
            default: false,
        };
        assert_eq!(catalog.label_or("Graph name"), "Graph name");
        assert_eq!(
            catalog.description_or("Graph description"),
            "Graph description"
        );
    }
}
