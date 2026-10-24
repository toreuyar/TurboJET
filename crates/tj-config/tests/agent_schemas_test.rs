//! Interactive-stage schema and package-wiring verification.
//!
//! The package is synthesized at runtime with neutral agent names; no tracked
//! workflow/package fixture is read. The tests validate real verdict-tool
//! payloads through `validate_json_value` and prove the execution bundle freezes
//! the package ADC schema the same way production admission does
//! (`ExecutionBundle::build_from_assets`).

use serde_json::{Value, json};
use tj_config::package::{WorkflowAssets, agent_logical_path};
use tj_config::{AgentDefinitionConfig, ConfigLoader, ExecutionBundle};
use tj_test_support::{
    SyntheticPackage, neutral_capability_package_agents, neutral_capability_workflow,
};

fn package_workflow() -> Value {
    neutral_capability_workflow("synthetic-schema-package", false)
}

fn package() -> SyntheticPackage {
    SyntheticPackage::new(
        &package_workflow(),
        &neutral_capability_package_agents(false),
    )
}

fn load_schema_file(root: &std::path::Path, name: &str) -> Value {
    let path = root.join("schemas").join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("invalid JSON in {}: {e}", path.display()))
}

fn verdict_enum(schema: &Value) -> Vec<&str> {
    schema["properties"]["verdict"]["enum"]
        .as_array()
        .expect("schema has verdict enum")
        .iter()
        .map(|v| v.as_str().expect("enum entries are strings"))
        .collect()
}

fn package_loader() -> (SyntheticPackage, ConfigLoader) {
    let package = package();
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("turbojet.json");
    std::fs::write(
        &config,
        json!({
            "listen_addr": "127.0.0.1:0",
            "providers": [{
                "id": "fixture", "type": "open_ai", "api_key_env": "FIXTURE_KEY",
                "base_url": "http://127.0.0.1:1", "default_model": "fixture-model"
            }],
            "workflow_sources": [{
                "id": "fixture",
                "path": package.root().to_string_lossy(),
                "kind": "package"
            }]
        })
        .to_string(),
    )
    .unwrap();
    let loader = ConfigLoader::from_file(&config).unwrap();
    (package, loader)
}

fn question_schema() -> Value {
    json!({
        "type": "object",
        "required": ["question"],
        "properties": {
            "question": {"type": "string"},
            "options": {"type": "array"},
            "multi_select": {"type": "boolean"},
            "free_form": {"type": "boolean"}
        }
    })
}

#[test]
fn writer_schema_enum_is_exactly_the_smg_route_key() {
    let package = package();
    let schema = load_schema_file(package.root(), "writer.json");
    assert_eq!(verdict_enum(&schema), vec!["generated"]);
}

#[test]
fn planner_schema_enum_is_exactly_the_route_space() {
    let package = package();
    let schema = load_schema_file(package.root(), "planner.json");
    assert_eq!(verdict_enum(&schema), vec!["success", "cannot-complete"]);
}

#[test]
fn writer_schema_accepts_valid_generated_artifact() {
    let package = package();
    let artifact = json!({
        "verdict": "generated",
        "summary": "Generated a reviewable requirements packet.",
        "content": "# Requirements Packet\n..."
    });
    tj_config::loader::validate_json_value(
        &artifact,
        &load_schema_file(package.root(), "writer.json"),
        "",
    )
    .unwrap();
}

#[test]
fn planner_schema_accepts_valid_success_artifact() {
    let package = package();
    let artifact = json!({
        "verdict": "success",
        "summary": "Planning stage complete.",
        "content": "# Planning\n..."
    });
    tj_config::loader::validate_json_value(
        &artifact,
        &load_schema_file(package.root(), "planner.json"),
        "",
    )
    .unwrap();
}

#[test]
fn planner_schema_accepts_valid_cannot_complete_artifact() {
    let package = package();
    let artifact = json!({
        "verdict": "cannot-complete",
        "summary": "The approved packet lacks one required decision."
    });
    tj_config::loader::validate_json_value(
        &artifact,
        &load_schema_file(package.root(), "planner.json"),
        "",
    )
    .unwrap();
}

#[test]
fn writer_schema_rejects_verdict_outside_its_routes() {
    let package = package();
    let artifact = json!({ "verdict": "success", "summary": "x" });
    let err = tj_config::loader::validate_json_value(
        &artifact,
        &load_schema_file(package.root(), "writer.json"),
        "",
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Validation error at verdict"), "{}", msg);
    assert!(msg.contains("not one of"), "{}", msg);
}

#[test]
fn planner_schema_rejects_legacy_needs_clarification_verdict() {
    let package = package();
    let artifact = json!({ "verdict": "needs-clarification", "summary": "x" });
    let err = tj_config::loader::validate_json_value(
        &artifact,
        &load_schema_file(package.root(), "planner.json"),
        "",
    )
    .unwrap_err();
    assert!(err.to_string().contains("not one of"), "{}", err);
}

#[test]
fn planner_schema_rejects_missing_required_summary() {
    let package = package();
    let artifact = json!({ "verdict": "success" });
    let err = tj_config::loader::validate_json_value(
        &artifact,
        &load_schema_file(package.root(), "planner.json"),
        "",
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("required field 'summary' missing"), "{}", msg);
}

#[test]
fn writer_schema_rejects_missing_required_verdict() {
    let package = package();
    let artifact = json!({ "summary": "x" });
    let err = tj_config::loader::validate_json_value(
        &artifact,
        &load_schema_file(package.root(), "writer.json"),
        "",
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("required field 'verdict' missing"),
        "{}",
        err
    );
}

#[test]
fn question_schema_mirrors_the_question_request_type() {
    let schema = question_schema();
    let valid = json!({
        "question": "Which platforms must be supported?",
        "options": [
            { "id": "web", "label": "Web" },
            { "id": "mobile", "label": "Mobile" }
        ],
        "multi_select": true,
        "free_form": false
    });
    tj_config::loader::validate_json_value(&valid, &schema, "").unwrap();

    // `multi_select`/`free_form` are serde-defaulted in QuestionRequest and the
    // tool declaration requires only `question` — a minimal payload must pass.
    let minimal = json!({ "question": "Any constraints on budget?" });
    tj_config::loader::validate_json_value(&minimal, &schema, "").unwrap();

    let no_question = json!({ "options": [] });
    let err = tj_config::loader::validate_json_value(&no_question, &schema, "").unwrap_err();
    assert!(
        err.to_string()
            .contains("required field 'question' missing"),
        "{}",
        err
    );
}

#[test]
fn package_agent_schema_and_retry_bound_freeze_into_the_execution_bundle() {
    let (package, loader) = package_loader();
    let workflow = loader
        .load_workflow_sources()
        .expect("synthetic package loads")
        .remove(0);
    let assets = WorkflowAssets::from_entries(
        workflow
            .assets
            .iter()
            .map(|asset| (asset.logical_path.clone(), asset.content.clone())),
    )
    .expect("synthetic assets");
    loader
        .validate_smg_with_assets(&workflow.graph, &assets)
        .expect("synthetic package validates from stored assets");

    for stage in ["writer", "planner"] {
        let logical_path = agent_logical_path(stage).unwrap();
        let raw = assets
            .get(&logical_path)
            .unwrap_or_else(|| panic!("missing package ADC {logical_path}"));
        let adc: AgentDefinitionConfig = serde_json::from_slice(raw).unwrap();
        assert!(
            !adc.schema.trim().is_empty(),
            "{stage} agent.json must declare a schema path"
        );
        assert_eq!(adc.max_output_retries, 3, "{stage} documents the bound");
        let schema: Value = serde_json::from_slice(
            assets
                .get(&adc.schema)
                .unwrap_or_else(|| panic!("missing package schema {}", adc.schema)),
        )
        .unwrap();
        assert!(
            schema
                .get("properties")
                .and_then(|p| p.get("verdict"))
                .is_some(),
            "{stage} schema must constrain verdict"
        );
    }

    // Production admission path for packages: the bundle freezes ADC content
    // and response schema from the registry assets, never from the filesystem.
    let bundle =
        ExecutionBundle::build_from_assets(&loader, "run_fixture", None, &workflow.graph, &assets)
            .expect("package bundle builds from assets");
    let writer = bundle.stage("collect", "writer").expect("writer stage");
    assert!(writer.response_schema.is_some());
    assert_eq!(writer.adc.max_output_retries, 3);
    assert!(!writer.prompt.trim().is_empty());

    drop(package);
}
