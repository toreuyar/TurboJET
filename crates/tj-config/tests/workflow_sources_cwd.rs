//! Relative workflow sources are resolved from the configuration base
//! directory, never from the process working directory. This test binary has a
//! single test so changing the process CWD cannot race another test.
//!
//! Every workflow, agent, prompt and schema is synthetic and lives in a
//! disposable tree; no shipped distribution asset is referenced.

use serde_json::json;
use tj_config::ConfigLoader;
use tj_test_support::SyntheticTree;

#[test]
fn relative_file_and_directory_sources_ignore_the_process_cwd() {
    let tree = SyntheticTree::new();
    let source_dir = tree.root().join("sources");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(
        source_dir.join("arbitrary.smg.json"),
        tree.workflow_json().to_string(),
    )
    .unwrap();
    tree.update_config(|config| {
        config["workflow_sources"] = json!([
            {"id": "file-source", "path": "config/graph.json", "kind": "file"},
            {"id": "dir-source", "path": "sources", "kind": "directory"}
        ]);
        config.as_object_mut().unwrap().remove("smg_path");
    })
    .unwrap();

    let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
    let original = std::env::current_dir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::env::set_current_dir(elsewhere.path()).unwrap();
    let loaded = loader.load_workflow_sources().unwrap();
    std::env::set_current_dir(&original).unwrap();

    assert_eq!(loaded.len(), 2);
    let file = loaded
        .iter()
        .find(|item| item.source_id == "file-source")
        .expect("file source");
    assert_eq!(
        file.graph.catalog.as_ref().unwrap().id,
        "synthetic-workflow"
    );
    assert_eq!(file.source_rel_path, "config/graph.json");
    assert_eq!(file.source_path, tree.root().join("config/graph.json"));

    let directory = loaded
        .iter()
        .find(|item| item.source_id == "dir-source")
        .expect("directory source");
    assert_eq!(
        directory.graph.catalog.as_ref().unwrap().id,
        "synthetic-workflow"
    );
    assert_eq!(directory.source_rel_path, "sources/arbitrary.smg.json");
    assert_eq!(
        directory.source_path,
        tree.root().join("sources/arbitrary.smg.json")
    );
}
