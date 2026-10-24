//! Shipped generic repository-free template contracts.
//!
//! The templates are self-contained workflow packages. Every graph, ADC, prompt
//! and schema is read from the package registry import, never from a shared
//! agent or schema tree.

use std::path::{Path, PathBuf};

use tj_config::package::agent_logical_path;
use tj_config::{
    AgentDefinitionConfig, ConfigLoader, LoadedWorkflow, WorkflowAssets, WorkflowMaturity,
    WorkflowSourceKind,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn loader() -> ConfigLoader {
    ConfigLoader::from_file(&repo_root().join("config/turbojet.json"))
        .expect("repository config loads")
}

fn package(id: &str) -> LoadedWorkflow {
    loader()
        .load_workflow_sources()
        .expect("shipped workflow packages load")
        .into_iter()
        .find(|workflow| {
            workflow
                .graph
                .catalog
                .as_ref()
                .is_some_and(|catalog| catalog.id == id)
        })
        .unwrap_or_else(|| panic!("package catalog '{id}' is not loaded"))
}

fn assets(workflow: &LoadedWorkflow) -> WorkflowAssets {
    WorkflowAssets::from_entries(
        workflow
            .assets
            .iter()
            .map(|asset| (asset.logical_path.clone(), asset.content.clone())),
    )
    .unwrap_or_else(|error| panic!("{} assets: {error}", workflow.source_rel_path))
}

fn adc(assets: &WorkflowAssets, agent_ref: &str) -> AgentDefinitionConfig {
    let logical_path = agent_logical_path(agent_ref).unwrap();
    let raw = assets
        .get(&logical_path)
        .unwrap_or_else(|| panic!("missing package ADC {logical_path}"));
    serde_json::from_slice(raw)
        .unwrap_or_else(|error| panic!("invalid package ADC {logical_path}: {error}"))
}

#[test]
fn each_generic_template_validates_with_shared_limits_and_confined_tools() {
    let loader = loader();
    for (id, limits) in [
        ("agent-loop", (3, 12, 4)),
        ("clarify-produce-review", (3, 12, 4)),
        ("research-synthesize-verify", (3, 10, 4)),
    ] {
        let workflow = package(id);
        assert_eq!(
            workflow.source_kind,
            WorkflowSourceKind::Package,
            "{id}: must load as a package"
        );
        assert!(!workflow.assets.is_empty(), "{id}: package assets missing");
        let assets = assets(&workflow);
        loader
            .validate_smg_with_assets(&workflow.graph, &assets)
            .unwrap_or_else(|error| panic!("{id}: {error:#}"));

        let smg = &workflow.graph;
        assert_eq!(smg.maturity, WorkflowMaturity::Experimental);
        assert_eq!(
            (
                smg.limits.max_iterations,
                smg.limits.max_steps,
                smg.limits.max_depth,
            ),
            limits,
            "{id}: shared graph limits drifted"
        );
        assert!(
            !smg.flow_order
                .iter()
                .any(|flow| matches!(flow.as_str(), "engineering" | "delivery" | "planning")),
            "{id}: generic flows must not enter repository SDLC"
        );
        for flow in smg.flows.values() {
            for stage in flow.stages.values() {
                let adc = adc(&assets, &stage.agent);
                assert!(adc.tools.read, "{id}: read capability is required");
                assert!(adc.tools.write, "{id}: write capability is required");
                assert!(!adc.tools.bash, "{id}: shell must be unavailable");
                assert!(!adc.tools.edit, "{id}: repository edit must be unavailable");
                assert_eq!(adc.max_output_retries, 3);
                assert_eq!(adc.timeout, 300);
                assert_eq!(adc.output.artifacts.len(), 1);
                assert_eq!(adc.output.artifacts[0].name, "output.md");
                assert!(adc.output.artifacts[0].required);
            }
        }
    }
}

#[test]
fn generic_routes_express_the_documented_loops() {
    let agent = package("agent-loop").graph;
    let flow = &agent.flows["agent-loop"];
    assert_eq!(
        flow.stages["agent-check"].routes["revise"]
            .as_simple()
            .unwrap(),
        "agent-act"
    );
    assert_eq!(
        flow.stages["agent-check"].routes["needs-input"]
            .as_simple()
            .unwrap(),
        "agent-result"
    );
    assert_eq!(
        flow.stages["agent-act"].routes["needs-input"]
            .as_simple()
            .unwrap(),
        "agent-result"
    );

    let clarify = package("clarify-produce-review").graph;
    assert!(clarify.is_terminal_gate_profile());
    assert_eq!(
        clarify.flows["clarify-produce-review"].entry.as_deref(),
        Some("clarify")
    );
    assert_eq!(
        clarify.flows["clarify-produce-review"].exit.as_deref(),
        Some("produce")
    );

    let research = package("research-synthesize-verify").graph;
    let flow = &research.flows["research-synthesize-verify"];
    assert_eq!(
        flow.stages["verify"].routes["revise"].as_simple().unwrap(),
        "synthesize"
    );
    assert_eq!(
        flow.stages["verify"].routes["missing-evidence"]
            .as_simple()
            .unwrap(),
        "research"
    );
    assert_eq!(
        flow.stages["synthesize"].routes["missing-evidence"]
            .as_simple()
            .unwrap(),
        "research"
    );
}

#[test]
fn generic_profile_metadata_matches_the_shipped_packages() {
    let root = repo_root();
    for id in [
        "agent-loop",
        "clarify-produce-review",
        "research-synthesize-verify",
    ] {
        let path = root.join("workflows").join(id).join("workflow.smg.json");
        assert!(path.is_file(), "{} must exist", path.display());
        assert!(root.join("workflows").join(id).join("agents").is_dir());
        assert!(root.join("workflows").join(id).join("schemas").is_dir());
        let workflow = package(id);
        assert_eq!(
            workflow.graph.catalog.as_ref().unwrap().id,
            id,
            "{id}: catalog id drifted"
        );
    }
    assert!(tj_config::profiles::is_generic(
        tj_config::profiles::AGENT_LOOP_PROFILE
    ));
    assert_eq!(
        tj_config::profiles::gate_binding(tj_config::profiles::CLARIFY_PRODUCE_REVIEW_PROFILE),
        tj_config::profiles::GateBinding::Artifact
    );
}
