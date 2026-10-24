//! Explicit workflow-source resolution and import validation.
//!
//! These are default (asset-free) tests: every graph and asset lives in a
//! disposable synthetic tree.

use serde_json::json;
use tj_config::ConfigLoader;
use tj_test_support::{SyntheticTree, SyntheticWorkflow};

struct TempTree(std::path::PathBuf);
impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_tree() -> TempTree {
    TempTree(std::env::temp_dir().join(format!("tj-workflow-sources-{}", uuid::Uuid::new_v4())))
}

fn write_tree(root: &std::path::Path, config: &serde_json::Value) -> std::path::PathBuf {
    let path = root.join("config/fixture.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, config.to_string()).unwrap();
    path
}

fn graph(name: &str, catalog: &str) -> serde_json::Value {
    let workflow = SyntheticWorkflow::new()
        .named(name)
        .with_catalog(catalog)
        .with_gate("approval");
    workflow.to_json()
}

#[test]
fn empty_sources_are_valid_and_never_imply_a_default_graph() {
    let tree = SyntheticTree::new();
    tree.update_config(|config| {
        config["workflow_sources"] = json!([]);
        config.as_object_mut().unwrap().remove("smg_path");
    })
    .unwrap();
    let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
    assert!(loader.load_workflow_sources().unwrap().is_empty());
    let error = loader.load_smg().unwrap_err().to_string();
    assert!(error.contains("no smg_path configured"), "{error}");
}

#[test]
fn directory_scan_is_non_recursive_sorted_and_catalog_required() {
    let tree = SyntheticTree::new();
    let source = tree.root().join("sources");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    tree.update_config(|config| {
        config["workflow_sources"] = json!([
            {"id": "main", "path": "sources", "kind": "directory"}
        ]);
        config.as_object_mut().unwrap().remove("smg_path");
    })
    .unwrap();
    std::fs::write(
        source.join("b-workflow.smg.json"),
        graph("Second arbitrary workflow", "arbitrary-second").to_string(),
    )
    .unwrap();
    std::fs::write(
        source.join("a-workflow.smg.json"),
        graph("First arbitrary workflow", "arbitrary-first").to_string(),
    )
    .unwrap();
    // Nested files are ignored unless the nested directory is its own source.
    std::fs::write(
        source.join("nested/ignored.smg.json"),
        graph("Nested ignored", "ignored").to_string(),
    )
    .unwrap();

    let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
    let loaded = loader.load_workflow_sources().unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(
        loaded
            .iter()
            .map(|item| item.graph.catalog.as_ref().unwrap().id.as_str())
            .collect::<Vec<_>>(),
        vec!["arbitrary-first", "arbitrary-second"]
    );
    assert!(loaded[0].source_path.ends_with("a-workflow.smg.json"));
    assert!(
        loaded[0]
            .source_rel_path
            .contains("sources/a-workflow.smg.json")
    );

    let mut without_catalog = graph("Legacy shape", "legacy");
    without_catalog.as_object_mut().unwrap().remove("catalog");
    std::fs::write(
        source.join("c-workflow.smg.json"),
        without_catalog.to_string(),
    )
    .unwrap();
    let error = format!("{:#}", loader.load_workflow_sources().unwrap_err());
    assert!(
        error.contains("has no catalog declaration") && error.contains("c-workflow.smg.json"),
        "{error}"
    );
}

#[test]
fn missing_explicit_sources_and_duplicate_ids_fail_with_the_path() {
    let tree = SyntheticTree::new();
    tree.update_config(|config| {
        config["workflow_sources"] =
            json!([{"id": "missing", "path": "does/not/exist.smg.json", "kind": "file"}]);
        config.as_object_mut().unwrap().remove("smg_path");
    })
    .unwrap();
    let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
    let error = format!("{:#}", loader.load_workflow_sources().unwrap_err());
    assert!(
        error.contains("explicitly configured workflow source 'missing' is missing")
            && error.contains("does/not/exist.smg.json"),
        "{error}"
    );

    let temp = temp_tree();
    let path = write_tree(
        &temp.0,
        &json!({
            "workflow_sources": [
                {"id": "same", "path": "a.smg.json", "kind": "file"},
                {"id": "same", "path": "b.smg.json", "kind": "file"}
            ]
        }),
    );
    let loader = ConfigLoader::from_file(&path).unwrap();
    let error = format!("{:#}", loader.load_workflow_sources().unwrap_err());
    assert!(
        error.contains("duplicate workflow source id 'same'"),
        "{error}"
    );
    drop(temp);
}

#[test]
fn missing_optional_alias_is_valid_but_explicitly_configured_smg_path_must_exist() {
    let tree = SyntheticTree::new();
    tree.update_config(|config| {
        config["workflow_sources"] = json!([]);
        config["smg_path"] = serde_json::Value::Null;
    })
    .unwrap();
    let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
    assert!(loader.load_workflow_sources().unwrap().is_empty());

    tree.update_config(|config| {
        config["workflow_sources"] = json!([]);
        config["smg_path"] = json!("sources/absent.smg.json");
    })
    .unwrap();
    let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
    let error = format!("{:#}", loader.load_workflow_sources().unwrap_err());
    assert!(
        error.contains("workflow source 'legacy-smg-path' is missing")
            && error.contains("sources/absent.smg.json"),
        "{error}"
    );
}

#[test]
fn capability_declarations_are_validated_before_any_workflow_is_returned() {
    let tree = SyntheticTree::new();
    let source = tree.root().join("sources");
    std::fs::create_dir_all(&source).unwrap();
    tree.update_config(|config| {
        config["workflow_sources"] = json!([
            {"id": "main", "path": "sources", "kind": "file"}
        ]);
        config.as_object_mut().unwrap().remove("smg_path");
    })
    .unwrap();

    let valid = SyntheticWorkflow::new()
        .named("Arbitrary gated pipeline")
        .with_catalog("arbitrary-gated")
        .with_gate("approval")
        .with_capabilities(json!({
            "schema_version": 1,
            "workspace": {"kind": "none"},
            "gates": {"approval": "artifact"}
        }));
    std::fs::write(source.join("valid.smg.json"), valid.to_json().to_string()).unwrap();
    tree.update_config(|config| {
        config["workflow_sources"] = json!([
            {"id": "main", "path": "sources/valid.smg.json", "kind": "file"}
        ]);
    })
    .unwrap();
    let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
    let loaded = loader.load_workflow_sources().unwrap();
    assert_eq!(loaded.len(), 1);
    assert!(loaded[0].graph.capabilities.is_some());

    let invalid_cases = [
        // Unknown schema version.
        json!({"schema_version": 2}),
        // Unknown module name.
        json!({"schema_version": 1, "unknown_module": {"x": true}}),
        // Unknown gate kind.
        json!({"schema_version": 1, "gates": {"approval": "none"}}),
        // Gate reference does not exist.
        json!({"schema_version": 1, "gates": {"missing-gate": "artifact"}}),
        // Unsafe workspace-relative input destination.
        json!({"schema_version": 1, "inputs": {"copy_project_inputs_to": "../escape"}}),
    ];
    for (index, capabilities) in invalid_cases.into_iter().enumerate() {
        let workflow = SyntheticWorkflow::new()
            .named("Arbitrary gated pipeline")
            .with_catalog("arbitrary-gated")
            .with_gate("approval")
            .with_capabilities(capabilities.clone());
        let path = source.join(format!("invalid-{index}.smg.json"));
        std::fs::write(&path, workflow.to_json().to_string()).unwrap();
        tree.update_config(|config| {
            config["workflow_sources"] = json!([
                {"id": "main", "path": path.to_string_lossy(), "kind": "file"}
            ]);
        })
        .unwrap();
        let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
        let error = format!("{:#}", loader.load_workflow_sources().unwrap_err());
        assert!(
            error.contains("cannot be imported")
                || error.contains("invalid capability")
                || error.contains("failed validation")
                || error.contains("unknown field")
                || error.contains("schema_version")
                || error.contains("must not")
                || error.contains("is not valid SMG JSON"),
            "capability {capabilities} was not rejected: {error}"
        );
    }
}

#[test]
fn only_one_catalog_default_is_allowed_and_smg_path_is_a_deprecated_alias() {
    let tree = SyntheticTree::new();
    let first = tree.root().join("sources/first.smg.json");
    let second = tree.root().join("sources/second.smg.json");
    std::fs::create_dir_all(first.parent().unwrap()).unwrap();
    let workflow = SyntheticWorkflow::new()
        .named("Default one")
        .with_catalog("default-one")
        .catalog_default(true);
    std::fs::write(&first, workflow.to_json().to_string()).unwrap();
    let workflow = SyntheticWorkflow::new()
        .named("Default two")
        .with_catalog("default-two")
        .catalog_default(true);
    std::fs::write(&second, workflow.to_json().to_string()).unwrap();
    tree.update_config(|config| {
        config["workflow_sources"] = json!([
            {"id": "first", "path": "sources/first.smg.json", "kind": "file"},
            {"id": "second", "path": "sources/second.smg.json", "kind": "file"}
        ]);
        config["smg_path"] = json!("sources/first.smg.json");
    })
    .unwrap();
    let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
    let error = format!("{:#}", loader.load_workflow_sources().unwrap_err());
    assert!(error.contains("at most one catalog default"), "{error}");
}
