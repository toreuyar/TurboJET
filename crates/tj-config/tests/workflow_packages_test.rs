//! Default, asset-free tests for workflow packages: package-confined reference
//! resolution, immutable asset collection, and fail-closed import errors.
//!
//! Every package lives in a disposable temp tree and uses synthetic content.

use serde_json::json;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tj_config::system::WorkflowSourceKind;
use tj_config::{ConfigLoader, WorkflowAssets};

struct PackageTree {
    _temp: TempDir,
    root: PathBuf,
    package: PathBuf,
}

impl PackageTree {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("fixture");
        let package = root.join("sources/pkg");
        write(
            &root.join("config/fixture.json"),
            &json!({
                "adc_dir": "config/stages",
                "workflow_sources": [
                    {"id": "pkg", "path": "sources/pkg", "kind": "package"}
                ]
            })
            .to_string(),
        );
        let tree = Self {
            _temp: temp,
            root,
            package,
        };
        tree.write_valid_package("first", "agents/second/agent.json");
        tree
    }

    fn config(&self) -> PathBuf {
        self.root.join("config/fixture.json")
    }

    fn loader(&self) -> ConfigLoader {
        ConfigLoader::from_file(&self.config()).unwrap()
    }

    fn write(&self, relative: &str, content: &str) {
        write(&self.package.join(relative), content);
    }

    fn write_graph(&self, agent_first: &str, agent_two: &str) {
        let graph = json!({
            "version": 1,
            "name": "Package fixture",
            "maturity": "experimental",
            "description": "Synthetic package",
            "catalog": {"id": "package-fixture"},
            "entry": "work",
            "exit": "work",
            "limits": {"maxIterations": 2, "maxSteps": 8, "maxDepth": 3},
            "flow-order": ["work"],
            "flows": {
                "work": {
                    "type": "flow",
                    "entry": "one",
                    "exit": "two",
                    "stages": {
                        "one": {"agent": agent_first, "routes": {"success": "two"}},
                        "two": {"agent": agent_two, "routes": {"success": "_exit"}}
                    }
                }
            }
        });
        self.write("workflow.smg.json", &graph.to_string());
    }

    fn write_valid_package(&self, agent_first: &str, agent_two_ref: &str) {
        self.write_graph(agent_first, agent_two_ref);
        self.write(
            "agents/first/agent.json",
            &json!({
                "id": "first", "name": "First", "prompt": "prompt.md",
                "schema": "schemas/shared.json", "timeout": 30
            })
            .to_string(),
        );
        self.write("agents/first/prompt.md", "Synthetic first prompt.\n");
        self.write(
            "agents/second/agent.json",
            &json!({
                "id": "second", "name": "Second", "prompt": "agents/second/prompt.md",
                "schema": "local.json", "timeout": 30,
                "output": {"status": {"schema": "schemas/status.json"}}
            })
            .to_string(),
        );
        self.write("agents/second/prompt.md", "Synthetic second prompt.\n");
        self.write(
            "agents/second/local.json",
            &schema(vec!["success"]).to_string(),
        );
        self.write("schemas/shared.json", &schema(vec!["success"]).to_string());
        self.write(
            "schemas/status.json",
            &json!({
                "type": "object",
                "properties": {"state": {"type": "string"}}
            })
            .to_string(),
        );
    }
}

fn schema(routes: Vec<&'static str>) -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["verdict"],
        "properties": {"verdict": {"type": "string", "enum": routes}}
    })
}

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn load(tree: &PackageTree) -> anyhow::Result<Vec<tj_config::LoadedWorkflow>> {
    tree.loader().load_workflow_sources()
}

#[test]
fn package_import_resolves_agents_and_records_every_asset() {
    let tree = PackageTree::new();
    let loaded = load(&tree).unwrap();
    assert_eq!(loaded.len(), 1);
    let workflow = &loaded[0];
    assert_eq!(workflow.source_kind, WorkflowSourceKind::Package);
    assert!(workflow.source_path.ends_with("workflow.smg.json"));
    assert!(
        workflow
            .source_rel_path
            .contains("sources/pkg/workflow.smg.json")
    );
    let assets = workflow
        .assets
        .iter()
        .map(|asset| asset.logical_path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        assets,
        vec![
            "agents/first/agent.json",
            "agents/first/prompt.md",
            "agents/second/agent.json",
            "agents/second/local.json",
            "agents/second/prompt.md",
            "schemas/shared.json",
            "schemas/status.json",
            "workflow.smg.json",
        ]
    );
    for asset in &workflow.assets {
        assert_eq!(asset.content_hash.len(), 64, "{}", asset.logical_path);
    }

    // The stored assets alone can validate and freeze the package workflow.
    let assets = WorkflowAssets::from_entries(
        workflow
            .assets
            .iter()
            .map(|asset| (asset.logical_path.clone(), asset.content.clone())),
    )
    .unwrap();
    tree.loader()
        .validate_smg_with_assets(&workflow.graph, &assets)
        .unwrap();
}

#[test]
fn directory_source_scans_legacy_graphs_and_immediate_packages_only() {
    let tree = PackageTree::new();
    let source = tree.root.join("sources/mixed");
    write(
        &tree.root.join("config/stages/legacy/agent.json"),
        &json!({
            "id": "legacy", "name": "Legacy", "prompt": "prompt.md",
            "schema": "config/stages/legacy/schema.json", "timeout": 30
        })
        .to_string(),
    );
    write(
        &tree.root.join("config/stages/legacy/prompt.md"),
        "Legacy prompt.\n",
    );
    write(
        &tree.root.join("config/stages/legacy/schema.json"),
        &schema(vec!["success"]).to_string(),
    );
    write(
        &source.join("legacy.smg.json"),
        &json!({
            "version": 1, "name": "Legacy", "catalog": {"id": "legacy-fixture"},
            "entry": "work", "exit": "work",
            "limits": {"maxIterations": 2, "maxSteps": 5, "maxDepth": 2},
            "flow-order": ["work"],
            "flows": {"work": {"entry": "one", "exit": "one", "stages": {
                "one": {"agent": "config/stages/legacy/agent.json", "routes": {"success": "_exit"}}
            }}}
        })
        .to_string(),
    );
    // A nested graph file is not a package and is ignored by the directory scan.
    write(
        &source.join("nested/ignored.smg.json"),
        &json!({"not": "a package manifest"}).to_string(),
    );
    std::fs::create_dir_all(source.join("not-a-package")).unwrap();
    // Copy the valid package into an immediate subdirectory.
    copy_dir(&tree.package, &source.join("embedded"));

    tree.write_config_sources(json!([
        {"id": "mixed", "path": "sources/mixed", "kind": "directory"}
    ]));

    let loaded = load(&tree).unwrap();
    assert_eq!(loaded.len(), 2);
    let legacy = loaded
        .iter()
        .find(|item| item.source_kind == WorkflowSourceKind::Directory)
        .expect("legacy graph");
    assert!(legacy.assets.is_empty());
    let package = loaded
        .iter()
        .find(|item| item.source_kind == WorkflowSourceKind::Package)
        .expect("package");
    assert!(!package.assets.is_empty());
}

#[test]
fn absolute_and_escaping_agent_references_are_rejected_before_registration() {
    let tree = PackageTree::new();
    tree.write_graph("/tmp/outside/agent.json", "agents/second/agent.json");
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(
        error.contains("must be package-relative, not absolute"),
        "{error}"
    );
    assert!(error.contains("/tmp/outside/agent.json"), "{error}");

    tree.write_graph("../outside/agent.json", "agents/second/agent.json");
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(
        error.contains("agent reference '../outside/agent.json'"),
        "{error}"
    );
    assert!(error.contains("must not contain '.'"), "{error}");
}

#[test]
fn missing_agent_empty_prompt_and_invalid_json_name_the_file() {
    let tree = PackageTree::new();

    tree.write_graph("agents/missing/agent.json", "agents/second/agent.json");
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(error.contains("agents/missing/agent.json"), "{error}");
    assert!(error.contains("stage 'one'"), "{error}");

    tree.write_valid_package("first", "agents/second/agent.json");
    tree.write("agents/first/prompt.md", "   \n");
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(
        error.contains("prompt agents/first/prompt.md is empty"),
        "{error}"
    );

    tree.write("agents/first/prompt.md", "Synthetic first prompt.\n");
    tree.write("agents/first/agent.json", "{not json");
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(
        error.contains("invalid ADC agents/first/agent.json"),
        "{error}"
    );

    tree.write(
        "agents/first/agent.json",
        &json!({
            "id": "first", "name": "First", "prompt": "prompt.md",
            "schema": "schemas/shared.json", "timeout": 30
        })
        .to_string(),
    );
    tree.write("schemas/shared.json", "{not json");
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(
        error.contains("invalid schema JSON schemas/shared.json"),
        "{error}"
    );
}

#[test]
fn multiple_or_missing_manifests_are_malformed_packages() {
    let tree = PackageTree::new();
    tree.write("nested/workflow.smg.json", "{}");
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(
        error.contains("must contain exactly one workflow.smg.json"),
        "{error}"
    );
    assert!(error.contains("nested/workflow.smg.json"), "{error}");

    std::fs::remove_file(tree.package.join("workflow.smg.json")).unwrap();
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(
        error.contains("must place workflow.smg.json at its root"),
        "{error}"
    );

    std::fs::remove_file(tree.package.join("nested/workflow.smg.json")).unwrap();
    tree.write("workflow.smg.json", "{malformed");
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(
        error.contains("workflow.smg.json is not valid SMG JSON"),
        "{error}"
    );
}

#[test]
fn package_import_never_falls_back_to_the_shared_adc_directory() {
    let tree = PackageTree::new();
    // A same-named shared ADC exists, but the package graph must not use it.
    let shared = tree.root.join("config/stages/first/agent.json");
    write(
        &shared,
        &json!({
            "id": "shared", "name": "Shared", "prompt": "prompt.md",
            "schema": "schemas/shared.json", "timeout": 30
        })
        .to_string(),
    );
    std::fs::remove_file(tree.package.join("agents/first/agent.json")).unwrap();
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(error.contains("agents/first/agent.json"), "{error}");
    assert!(!error.contains("shared"), "{error}");
}

#[test]
fn configured_missing_package_source_still_fails() {
    let tree = PackageTree::new();
    std::fs::remove_dir_all(&tree.package).unwrap();
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(
        error.contains("explicitly configured workflow source 'pkg' is missing"),
        "{error}"
    );
}

#[test]
fn asset_bounds_are_enforced_with_clear_errors() {
    let tree = PackageTree::new();
    tree.write("schemas/shared.json", &"x".repeat(1024 * 1024 + 1));
    let error = format!("{:#}", load(&tree).unwrap_err());
    assert!(
        error.contains("maximum text-asset size is 1 MiB"),
        "{error}"
    );
    assert!(error.contains("schemas/shared.json"), "{error}");
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

impl PackageTree {
    fn write_config_sources(&self, sources: serde_json::Value) {
        let mut config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(self.config()).unwrap()).unwrap();
        config["workflow_sources"] = sources;
        write(&self.config(), &config.to_string());
    }
}
