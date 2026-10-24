//! Shipped workflow package contract checks: every executable stage declares
//! (a) a package schema file that exists and parses, (b) a `verdict` enum equal
//! to the stage's SMG route keys EXACTLY, and (c) `schema` +
//! `max_output_retries` in its package agent.json. The graph is the source of
//! truth; route drift fails here before the config lint ever runs.
//!
//! Shipped templates are self-contained packages. The old shared agent/schema
//! trees no longer exist, so every reference stays inside its package. The
//! optional ignored local SDLC package is validated when present.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tj_config::package::{WorkflowAssets, agent_logical_path};
use tj_config::{AgentDefinitionConfig, ConfigLoader, LoadedWorkflow, WorkflowSourceKind};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(path: &Path) -> Value {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("invalid JSON in {}: {e}", path.display()))
}

struct StageSpec {
    stage: String,
    package: PathBuf,
    agent_ref: String,
    routes: Vec<String>,
}

impl StageSpec {
    fn agent_logical_path(&self) -> String {
        agent_logical_path(&self.agent_ref).unwrap_or_else(|error| {
            panic!(
                "stage '{}': invalid package agent reference '{}': {error}",
                self.stage, self.agent_ref
            )
        })
    }

    fn adc_path(&self) -> PathBuf {
        self.package.join(self.agent_logical_path())
    }

    fn schema_path(&self, adc: &Value) -> PathBuf {
        let schema_path = adc["schema"]
            .as_str()
            .unwrap_or_else(|| panic!("stage '{}': agent declares no schema path", self.stage));
        assert!(
            !schema_path.trim().is_empty(),
            "stage '{}': agent schema path is empty",
            self.stage
        );
        let schema = Path::new(schema_path);
        let root_candidate = self.package.join(schema);
        if root_candidate.is_file() {
            return root_candidate;
        }
        self.adc_path()
            .parent()
            .expect("ADC has a parent")
            .join(schema)
    }
}

/// Shipped package directories plus the ignored local SDLC package when it
/// exists on this checkout. A fresh clone has no local package, so the default
/// optional suite reads only tracked `workflows/<name>/` packages.
fn package_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let workflows = repo_root().join("workflows");
    if workflows.is_dir() {
        for entry in std::fs::read_dir(&workflows).expect("workflows dir readable") {
            let path = entry.expect("workflows entry readable").path();
            if path.is_dir() && path.join("workflow.smg.json").is_file() {
                roots.push(path);
            }
        }
    }
    if let Some(local) = sdlc_package() {
        roots.push(local);
    }
    roots.sort();
    roots
}

fn smg_stages() -> Vec<StageSpec> {
    let mut out = Vec::new();
    for package in package_roots() {
        let path = package.join("workflow.smg.json");
        let smg = read_json(&path);
        for (flow_name, flow) in smg["flows"]
            .as_object()
            .unwrap_or_else(|| panic!("{}: smg has no flows object", path.display()))
            .iter()
        {
            for (stage_name, stage) in flow["stages"]
                .as_object()
                .unwrap_or_else(|| panic!("flow {flow_name} has no stages object"))
                .iter()
            {
                let routes = stage["routes"]
                    .as_object()
                    .unwrap_or_else(|| panic!("stage {flow_name}/{stage_name} has no routes"));
                let mut keys: Vec<String> = routes.keys().cloned().collect();
                keys.sort();
                let agent_ref = stage["agent"].as_str().unwrap_or_else(|| {
                    panic!("stage {flow_name}/{stage_name} has no agent reference")
                });
                out.push(StageSpec {
                    stage: stage_name.clone(),
                    package: package.clone(),
                    agent_ref: agent_ref.to_string(),
                    routes: keys,
                });
            }
        }
    }
    out
}

fn assert_stage_schema_and_retry_bound(stage: &StageSpec) -> Value {
    let adc = read_json(&stage.adc_path());
    assert!(
        adc["schema"].as_str().is_some_and(|s| !s.trim().is_empty()),
        "stage '{}': agent.json must declare a schema path",
        stage.stage
    );
    assert_eq!(
        adc["max_output_retries"].as_u64(),
        Some(3),
        "stage '{}': agent.json must document max_output_retries: 3",
        stage.stage
    );
    read_json(&stage.schema_path(&adc))
}

fn verdict_enum(schema: &Value) -> Vec<String> {
    let mut keys: Vec<String> = schema["properties"]["verdict"]["enum"]
        .as_array()
        .expect("schema has a verdict enum")
        .iter()
        .map(|v| v.as_str().expect("enum entries are strings").to_string())
        .collect();
    keys.sort();
    keys
}

fn assets(workflow: &LoadedWorkflow) -> WorkflowAssets {
    WorkflowAssets::from_entries(
        workflow
            .assets
            .iter()
            .map(|asset| (asset.logical_path.clone(), asset.content.clone())),
    )
    .expect("package assets")
}

fn asset_adc(assets: &WorkflowAssets, agent_ref: &str) -> AgentDefinitionConfig {
    let logical_path = agent_logical_path(agent_ref).unwrap();
    serde_json::from_slice(
        assets
            .get(&logical_path)
            .unwrap_or_else(|| panic!("missing package ADC {logical_path}")),
    )
    .unwrap_or_else(|error| panic!("invalid package ADC {logical_path}: {error}"))
}

/// Load a package through the real import path from a disposable config.
fn load_package(package: &Path) -> (tempfile::TempDir, ConfigLoader, LoadedWorkflow) {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("turbojet.json");
    std::fs::write(
        &config,
        json!({
            "listen_addr": "127.0.0.1:0",
            "workflow_sources": [{
                "id": "opt-in-package",
                "path": package.to_string_lossy(),
                "kind": "package"
            }]
        })
        .to_string(),
    )
    .unwrap();
    let loader = ConfigLoader::from_file(&config).expect("package fixture config loads");
    let workflow = loader
        .load_workflow_sources()
        .expect("opt-in package imports")
        .remove(0);
    (temp, loader, workflow)
}

#[test]
fn every_smg_stage_schema_enum_is_exactly_its_route_keys() {
    let stages = smg_stages();
    assert!(
        !stages.is_empty(),
        "no shipped workflow stages found; package scanning drifted"
    );
    for stage in stages {
        let schema = assert_stage_schema_and_retry_bound(&stage);
        assert_eq!(
            verdict_enum(&schema),
            stage.routes,
            "stage '{}' (agent '{}'): schema verdict enum must equal its SMG route keys",
            stage.stage,
            stage.agent_ref
        );
    }
}

#[test]
fn every_smg_stage_agent_declares_schema_and_retry_bound() {
    for stage in smg_stages() {
        let _ = assert_stage_schema_and_retry_bound(&stage);
    }
}

#[test]
fn smg_agent_dirs_cover_every_executable_agent_directory() {
    for stage in smg_stages() {
        let adc = stage.adc_path();
        assert!(
            adc.is_file(),
            "stage '{}' references missing agent file '{}'",
            stage.stage,
            adc.display()
        );
    }
    // No global "extras" assertion: agent directory contents are not a workflow
    // contract. Only every referenced agent file must exist.
}

/// Explicit opt-in SDLC package. The original local graph is never read here;
/// operators point TJ_SDLC_PACKAGE at the derived package (or rely on the local
/// default location when it exists). No full production graph is tracked.
fn sdlc_package() -> Option<PathBuf> {
    let path = std::env::var_os("TJ_SDLC_PACKAGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("config/local/full-sdlc.package"));
    if !path.join("workflow.smg.json").is_file() {
        eprintln!(
            "SKIP explicit SDLC package check: {} is absent (set TJ_SDLC_PACKAGE to opt in)",
            path.display()
        );
        return None;
    }
    Some(path)
}

#[test]
fn no_non_interactive_prompt_instructs_a_status_key() {
    for package in package_roots() {
        let agents = package.join("agents");
        if !agents.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&agents).expect("package agents dir") {
            let prompt_path = entry
                .expect("agent entry readable")
                .path()
                .join("prompt.md");
            if !prompt_path.is_file() {
                continue;
            }
            let prompt = std::fs::read_to_string(&prompt_path).expect("prompt readable");
            assert!(
                !prompt.contains("set status to"),
                "{} still instructs a status key for routing (must use verdict)",
                prompt_path.display()
            );
        }
    }
}

#[test]
fn discovery_profile_is_supported_repository_free_and_ends_at_the_approval_gate() {
    let root = repo_root();
    let loader = ConfigLoader::from_file(&root.join("config/turbojet.json"))
        .expect("repository config loads");
    let workflow = loader
        .load_workflow_sources()
        .expect("shipped workflow packages load")
        .into_iter()
        .find(|workflow| {
            workflow
                .graph
                .catalog
                .as_ref()
                .is_some_and(|catalog| catalog.id == "discovery")
        })
        .expect("discovery package is loaded");
    assert_eq!(workflow.source_kind, WorkflowSourceKind::Package);
    let assets = assets(&workflow);
    loader
        .validate_smg_with_assets(&workflow.graph, &assets)
        .expect("Discovery profile validates from package assets");
    let graph = &workflow.graph;
    assert_eq!(graph.maturity, tj_config::smg::WorkflowMaturity::Supported);
    assert!(
        graph.is_terminal_gate_profile(),
        "Discovery must end at its final approval gate"
    );
    assert_eq!(graph.entry, "discovery");
    assert_eq!(graph.exit, "discovery-gate");
    assert_eq!(graph.flow_order, vec!["discovery", "discovery-gate"]);
    assert!(
        !graph
            .flows
            .keys()
            .any(|name| matches!(name.as_str(), "planning" | "engineering" | "delivery"))
    );
    for flow in graph.flows.values() {
        for stage in flow.stages.values() {
            let adc = asset_adc(&assets, &stage.agent);
            assert!(
                !adc.tools.bash,
                "supported Discovery stages must not grant shell/bash"
            );
            assert!(
                !adc.tools.edit,
                "supported Discovery stages must not grant repository editing"
            );
        }
    }
}

/// Normalize agent references to their agent id and drop the package catalog /
/// capability metadata, so the derived package graph can be compared to the
/// original local graph without depending on the old shared path prefix.
fn normalized_graph(mut graph: Value, package_relative: bool) -> Value {
    let object = graph.as_object_mut().expect("graph object");
    object.remove("catalog");
    object.remove("capabilities");
    for flow in object["flows"]
        .as_object_mut()
        .expect("graph flows")
        .values_mut()
    {
        let flow = flow.as_object_mut().expect("flow object");
        let Some(stages) = flow.get_mut("stages").and_then(Value::as_object_mut) else {
            continue;
        };
        for stage in stages.values_mut() {
            let reference = stage["agent"]
                .as_str()
                .expect("stage agent reference")
                .to_string();
            let id = if package_relative {
                let logical = agent_logical_path(&reference).expect("package agent reference");
                Path::new(&logical)
                    .parent()
                    .and_then(Path::file_name)
                    .expect("logical parent")
                    .to_string_lossy()
                    .into_owned()
            } else {
                Path::new(&reference)
                    .parent()
                    .and_then(Path::file_name)
                    .unwrap_or_else(|| Path::new(&reference).file_name().expect("agent name"))
                    .to_string_lossy()
                    .into_owned()
            };
            let stage = stage.as_object_mut().unwrap();
            stage.insert("agent".into(), json!(id));
            // Serialization fills documented defaults the original JSON omits.
            stage.remove("runner_type");
            if stage.get("timeout").and_then(Value::as_u64) == Some(600) {
                stage.remove("timeout");
            }
            if stage.get("interactive").and_then(Value::as_bool) == Some(false) {
                stage.remove("interactive");
            }
        }
        let empty = stages.is_empty();
        if empty {
            flow.remove("stages");
        }
    }
    graph
}

#[test]
fn derived_local_sdlc_package_preserves_the_original_graph() {
    use sha2::{Digest, Sha256};

    let original_path = repo_root().join("config/local/full-sdlc.smg.json");
    if !original_path.is_file() {
        eprintln!("SKIP local SDLC regression: original local graph is absent");
        return;
    }
    let original_raw = std::fs::read(&original_path).unwrap();
    let original_hash = hex::encode(Sha256::digest(&original_raw));
    assert_eq!(
        original_hash, "a88bed13195f925f9af01169ae9b438e3e161cdbc05257443569a25530f5d34f",
        "the original local SDLC graph must remain byte-for-byte unchanged"
    );
    let Some(package) = sdlc_package() else {
        return;
    };
    let (_temp, loader, workflow) = load_package(&package);
    assert_eq!(workflow.source_kind, WorkflowSourceKind::Package);
    let package_assets = assets(&workflow);
    loader
        .validate_smg_with_assets(&workflow.graph, &package_assets)
        .expect("derived local SDLC package validates");

    let original: Value = serde_json::from_slice(&original_raw).unwrap();
    let package_graph = serde_json::to_value(&workflow.graph).unwrap();
    assert_eq!(
        normalized_graph(original, false),
        normalized_graph(package_graph, true),
        "the derived package may add only catalog/capabilities and package-relative agent refs"
    );
    assert_eq!(workflow.graph.catalog.as_ref().unwrap().id, "full-sdlc");
    let contract = tj_config::capability::effective_contract(&workflow.graph);
    assert_eq!(
        contract.workspace.kind,
        tj_config::capability::WorkspaceMode::Repository
    );
    let packet_gate = contract
        .requirements_packet
        .as_ref()
        .expect("local package declares a packet capability")
        .binding_gate
        .clone();
    let plan_gate = contract
        .task_plan
        .as_ref()
        .expect("local package declares a plan capability")
        .binding_gate
        .clone();
    assert_eq!(
        contract.gate_binding(&packet_gate),
        tj_config::capability::GateBindingKind::Packet
    );
    assert_eq!(
        contract.gate_binding(&plan_gate),
        tj_config::capability::GateBindingKind::Plan
    );
    contract.validate(&workflow.graph).unwrap();

    // Compare the package to the ignored original; no stage or agent identity is
    // restated here, so this public test does not define the private graph.
    let mut referenced: Vec<String> = workflow
        .graph
        .flows
        .values()
        .flat_map(|flow| flow.stages.values())
        .map(|stage| {
            agent_logical_path(&stage.agent)
                .expect("local package references a valid package-relative agent")
        })
        .collect();
    referenced.sort_unstable();
    referenced.dedup();
    for agent in &referenced {
        assert!(
            package_assets.contains(agent),
            "missing package agent {agent}"
        );
    }

    let mut agent_paths: Vec<&str> = package_assets
        .iter()
        .map(|(path, _)| path)
        .filter(|path| path.starts_with("agents/") && path.ends_with("/agent.json"))
        .collect();
    agent_paths.sort_unstable();
    assert_eq!(
        agent_paths.len(),
        referenced.len(),
        "the package owns every agent the graph uses: {agent_paths:?}"
    );
}
