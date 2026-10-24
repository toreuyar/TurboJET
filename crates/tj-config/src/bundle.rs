//! Immutable execution bundle (TJ-012).
//!
//! A run records the exact workflow, ADC, prompt, schema, provider/model and
//! limits it was admitted with. Runtime and restart resolution read this
//! bundle instead of re-reading mutable files, so editing `config/` or
//! repinning a lane cannot change an existing run.
//!
//! Secrets are deliberately absent: only the provider API-key *environment
//! variable name* is recorded. The value is read from the process environment
//! when a stage starts (or from the project secret store), never persisted.

use crate::adc::AgentDefinitionConfig;
use crate::capability::{self, WorkflowCapabilities};
use crate::loader::{ConfigLoader, FileResourceReader, StageResourceReader};
use crate::package::{StoredAssetResources, WorkflowAssets};
use crate::smg::{StageDef, StateMachineGraph};
use crate::system::ProviderConfig;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

/// Schema version written by this build. Readers also accept v1 bundles, but
/// v1 behavior is available only through the isolated legacy adapter.
pub const EXECUTION_BUNDLE_SCHEMA_VERSION: u32 = 2;

/// Bundle document versions this reader can parse. Version 0 and newer unknown
/// versions fail closed.
pub const SUPPORTED_BUNDLE_VERSIONS: &[u32] = &[1, 2];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionBundle {
    pub schema_version: u32,
    pub run_id: String,
    pub workflow_id: Option<String>,
    pub workflow_hash: String,
    pub graph: StateMachineGraph,
    /// TJ-016: run-wide budgets frozen with the execution contract. Older
    /// bundles without the field read as the documented defaults.
    #[serde(default)]
    pub budgets: crate::budget::RunBudgetLimits,
    /// `flows[flow][stage]` — every executable stage is frozen at admission.
    pub flows: HashMap<String, HashMap<String, FrozenStage>>,
    /// Hash of this document with `bundle_hash` empty. Stored separately by
    /// the database so a caller can verify the persisted JSON was not changed.
    pub bundle_hash: String,
    pub created_at: String,
    /// Version 2 freezes the effective capability contract exactly. Both
    /// fields are absent from a genuine v1 document, so v1 bytes and hashes are
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_contract: Option<WorkflowCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenStage {
    pub flow_name: String,
    pub stage_name: String,
    /// The SMG reference as declared.
    pub agent_ref: String,
    /// Resolved file path used at admission (diagnostic reference only).
    pub adc_path: String,
    pub adc_hash: String,
    pub adc: AgentDefinitionConfig,
    pub prompt_path: String,
    pub prompt_hash: String,
    pub prompt: String,
    pub schema_path: Option<String>,
    pub schema_hash: Option<String>,
    pub response_schema: Option<serde_json::Value>,
    /// Provider identity without the credential value.
    pub provider: ProviderConfig,
    /// Selected model for this stage (ADC override or provider default).
    pub model: String,
    /// Stage timeout from the workflow stage definition.
    pub stage_timeout_secs: u64,
    /// Limits frozen for this stage/run.
    pub limits: FrozenLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrozenLimits {
    pub adc_timeout_secs: u64,
    pub max_output_retries: u32,
    pub max_iterations: u32,
    pub max_steps: u32,
    pub max_depth: u32,
}

impl ExecutionBundle {
    /// Build and validate the complete executable bundle before a run is
    /// persisted. Missing/invalid references fail here, before any model call.
    pub fn build(
        loader: &ConfigLoader,
        run_id: &str,
        workflow_id: Option<&str>,
        graph: &StateMachineGraph,
    ) -> Result<Self> {
        let mut reader = FileResourceReader::new(loader);
        Self::build_with_reader(loader, run_id, workflow_id, graph, &mut reader)
    }

    /// Build an execution bundle for a package workflow from stored registry
    /// assets only. Missing assets fail closed; no ADC, prompt or schema file is
    /// read from the package directory.
    pub fn build_from_assets(
        loader: &ConfigLoader,
        run_id: &str,
        workflow_id: Option<&str>,
        graph: &StateMachineGraph,
        assets: &WorkflowAssets,
    ) -> Result<Self> {
        let mut reader = StoredAssetResources::new(assets);
        Self::build_with_reader(loader, run_id, workflow_id, graph, &mut reader)
    }

    fn build_with_reader(
        loader: &ConfigLoader,
        run_id: &str,
        workflow_id: Option<&str>,
        graph: &StateMachineGraph,
        reader: &mut dyn StageResourceReader,
    ) -> Result<Self> {
        anyhow::ensure!(
            !run_id.trim().is_empty(),
            "execution bundle requires a run id"
        );
        loader.validate_smg_with_reader(graph, reader)?;
        let capabilities = capability::effective_contract(graph);
        capabilities.validate(graph)?;
        let capability_hash = capability::contract_hash(&capabilities)?;

        let sys = loader.config();
        sys.budgets.validate().context("invalid run budgets")?;
        let mut flows: HashMap<String, HashMap<String, FrozenStage>> = HashMap::new();
        for flow_name in &graph.flow_order {
            let flow = graph.flows.get(flow_name).with_context(|| {
                format!("workflow '{}' flow '{}' is missing", graph.name, flow_name)
            })?;
            let mut frozen_stages = HashMap::new();
            for (stage_name, stage_def) in &flow.stages {
                let stage = freeze_stage_with_reader(
                    loader,
                    &sys.providers,
                    graph,
                    flow_name,
                    stage_name,
                    stage_def,
                    reader,
                )?;
                frozen_stages.insert(stage_name.clone(), stage);
            }
            flows.insert(flow_name.clone(), frozen_stages);
        }

        let mut bundle = Self {
            schema_version: EXECUTION_BUNDLE_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            workflow_id: workflow_id.map(str::to_string),
            workflow_hash: hash_value(&serde_json::to_value(graph)?),
            graph: graph.clone(),
            budgets: sys.budgets.clone(),
            flows,
            bundle_hash: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            capability_contract: Some(capabilities),
            capability_hash: Some(capability_hash),
        };
        bundle.bundle_hash = bundle.compute_hash()?;
        Ok(bundle)
    }

    pub fn stage(&self, flow_name: &str, stage_name: &str) -> Option<&FrozenStage> {
        self.flows.get(flow_name)?.get(stage_name)
    }

    /// Recompute the self-hash. Used on load to detect tampering/truncation.
    pub fn compute_hash(&self) -> Result<String> {
        let mut copy = self.clone();
        copy.bundle_hash.clear();
        Ok(hash_value(&serde_json::to_value(copy)?))
    }

    pub fn verify_hash(&self) -> Result<()> {
        let actual = self.compute_hash()?;
        anyhow::ensure!(
            actual == self.bundle_hash,
            "execution bundle hash mismatch: stored {} recomputed {}",
            self.bundle_hash,
            actual
        );
        Ok(())
    }

    /// Strict load for runtime/restart: schema version, self-hash, graph
    /// contract and (for v2) the frozen capability contract must all match. A
    /// corrupt bundle never falls back to files.
    pub fn from_json(raw: &str) -> Result<Self> {
        let bundle: Self = serde_json::from_str(raw)
            .context("execution bundle is not valid JSON for this version")?;
        anyhow::ensure!(
            SUPPORTED_BUNDLE_VERSIONS.contains(&bundle.schema_version),
            "unsupported execution bundle schema version {} (supported: {})",
            bundle.schema_version,
            SUPPORTED_BUNDLE_VERSIONS
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        bundle.verify_hash()?;
        crate::validation::validate_graph(&bundle.graph)?;
        anyhow::ensure!(
            bundle.workflow_hash == hash_value(&serde_json::to_value(&bundle.graph)?),
            "execution bundle workflow hash does not match its frozen graph"
        );
        match bundle.schema_version {
            1 => {
                anyhow::ensure!(
                    bundle.capability_contract.is_none() && bundle.capability_hash.is_none(),
                    "v1 execution bundle must not claim a capability contract"
                );
            }
            2 => {
                let capabilities = bundle
                    .capability_contract
                    .as_ref()
                    .context("v2 execution bundle is missing its frozen capability contract")?;
                let capability_hash = bundle
                    .capability_hash
                    .as_ref()
                    .context("v2 execution bundle is missing its capability hash")?;
                capabilities.validate(&bundle.graph)?;
                anyhow::ensure!(
                    &capability::contract_hash(capabilities)? == capability_hash,
                    "execution bundle capability hash mismatch"
                );
                let effective = capability::effective_contract(&bundle.graph);
                anyhow::ensure!(
                    &effective == capabilities,
                    "frozen capability contract does not match the workflow graph declaration"
                );
            }
            _ => unreachable!("supported bundle versions checked above"),
        }
        Ok(bundle)
    }
}

/// Freeze one stage. This is the single implementation used by run admission
/// and by the legacy (pre-bundle) runtime fallback, so both resolve references
/// identically.
pub fn freeze_stage(
    loader: &ConfigLoader,
    providers: &[ProviderConfig],
    graph: &StateMachineGraph,
    flow_name: &str,
    stage_name: &str,
    stage_def: &StageDef,
) -> Result<FrozenStage> {
    let mut reader = FileResourceReader::new(loader);
    freeze_stage_with_reader(
        loader,
        providers,
        graph,
        flow_name,
        stage_name,
        stage_def,
        &mut reader,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn freeze_stage_with_reader(
    _loader: &ConfigLoader,
    providers: &[ProviderConfig],
    graph: &StateMachineGraph,
    flow_name: &str,
    stage_name: &str,
    stage_def: &StageDef,
    reader: &mut dyn StageResourceReader,
) -> Result<FrozenStage> {
    let contract = format!(
        "workflow '{}' flow '{}' stage '{}'",
        graph.name, flow_name, stage_name
    );
    let adc_resource = reader
        .read_adc(stage_name, &stage_def.agent)
        .with_context(|| {
            format!(
                "{contract}: invalid agent/ADC reference '{}'",
                stage_def.agent
            )
        })?;
    let adc_raw = adc_resource
        .text()
        .with_context(|| format!("{contract}: cannot read ADC {}", adc_resource.path))?
        .to_string();
    let adc: AgentDefinitionConfig = serde_json::from_str(&adc_raw)
        .with_context(|| format!("{contract}: invalid ADC {}", adc_resource.path))?;

    let prompt_resource = reader
        .read_prompt(&adc_resource, &adc.prompt)
        .with_context(|| format!("{contract}: cannot read prompt"))?;
    let prompt = prompt_resource
        .text()
        .with_context(|| format!("{contract}: cannot read prompt {}", prompt_resource.path))?
        .to_string();
    anyhow::ensure!(
        !prompt.trim().is_empty(),
        "{contract}: prompt {} is empty",
        prompt_resource.path
    );

    let (schema_path, schema_hash, response_schema) = if adc.schema.trim().is_empty() {
        (None, None, None)
    } else {
        let schema_resource = reader
            .read_schema(&adc_resource, &adc.schema)
            .with_context(|| format!("{contract}: cannot read schema"))?;
        let raw = schema_resource
            .text()
            .with_context(|| format!("{contract}: cannot read schema {}", schema_resource.path))?
            .to_string();
        let value: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("{contract}: invalid schema JSON {}", schema_resource.path))?;
        (
            Some(schema_resource.path),
            Some(hash_bytes(raw.as_bytes())),
            Some(value),
        )
    };

    let provider = resolve_provider_for_adc(providers, &adc, &contract)?;
    let model = resolve_model_for_adc(&provider, &adc, &contract)?;

    let limits = FrozenLimits {
        adc_timeout_secs: adc.timeout,
        max_output_retries: adc.max_output_retries,
        max_iterations: graph.limits.max_iterations,
        max_steps: graph.limits.max_steps,
        max_depth: graph.limits.max_depth,
    };
    Ok(FrozenStage {
        flow_name: flow_name.to_string(),
        stage_name: stage_name.to_string(),
        agent_ref: stage_def.agent.clone(),
        adc_path: adc_resource.path,
        adc_hash: hash_bytes(adc_raw.as_bytes()),
        adc,
        prompt_path: prompt_resource.path,
        prompt_hash: hash_bytes(prompt.as_bytes()),
        prompt: prompt.to_string(),
        schema_path,
        schema_hash,
        response_schema,
        provider,
        model,
        stage_timeout_secs: stage_def.timeout,
        limits,
    })
}

/// Provider selection shared by bundle admission and the legacy file-based
/// stage resolver. Records only identity/configuration, never a credential.
pub fn resolve_provider_for_adc(
    providers: &[ProviderConfig],
    adc: &AgentDefinitionConfig,
    contract: &str,
) -> Result<ProviderConfig> {
    let provider = match adc.provider_id.as_deref() {
        Some(provider_id) => providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .with_context(|| format!("{contract}: provider '{}' is not configured", provider_id))?,
        None => providers
            .first()
            .with_context(|| format!("{contract}: no provider is configured"))?,
    }
    .clone();
    anyhow::ensure!(
        !provider.id.trim().is_empty(),
        "{contract}: provider id must not be empty"
    );
    anyhow::ensure!(
        !provider.api_key_env.trim().is_empty(),
        "{contract}: provider '{}' must name an API-key environment variable",
        provider.id
    );
    Ok(provider)
}

pub fn resolve_model_for_adc(
    provider: &ProviderConfig,
    adc: &AgentDefinitionConfig,
    contract: &str,
) -> Result<String> {
    let model = adc
        .model
        .clone()
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| provider.default_model.clone());
    anyhow::ensure!(
        !model.trim().is_empty(),
        "{contract}: provider '{}' has no default model and the ADC names none",
        provider.id
    );
    Ok(model)
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn hash_value(value: &serde_json::Value) -> String {
    // serde_json::Value objects use a sorted map by default (no preserve_order
    // feature), so this is a stable canonical representation for hashing.
    match serde_json::to_vec(value) {
        Ok(bytes) => hash_bytes(&bytes),
        Err(_) => hash_bytes(value.to_string().as_bytes()),
    }
}

/// Convenience for callers that only have a path and want a readable hash.
pub fn hash_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(hash_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::ConfigLoader;
    use serde_json::json;

    struct TempTree(std::path::PathBuf);
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn tree() -> (TempTree, ConfigLoader, StateMachineGraph) {
        let root = std::env::temp_dir().join(format!(
            "tj-bundle-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let guard = TempTree(root.clone());
        write(
            &root.join("config/fixture.json"),
            &json!({
                "smg_path": "config/graph.json",
                "adc_dir": "config/stages",
                "providers": [{
                    "id": "supported",
                    "type": "open_ai",
                    "api_key_env": "SUPPORTED_API_KEY",
                    "base_url": "https://provider.invalid/v1",
                    "default_model": "supported-model"
                }]
            })
            .to_string(),
        );
        let graph = json!({
            "version": 1, "name": "Frozen", "entry": "work", "exit": "work",
            "limits": {"maxIterations": 3, "maxSteps": 9, "maxDepth": 4},
            "flow-order": ["work"],
            "flows": {"work": {"type": "flow", "entry": "start", "exit": "start",
                "stages": {"start": {"agent": "config/stages/one/agent.json", "routes": {"success": "_exit"}}}}}
        });
        write(&root.join("config/graph.json"), &graph.to_string());
        write(
            &root.join("config/stages/one/agent.json"),
            &json!({
                "id": "one", "name": "One", "prompt": "prompt.md",
                "schema": "config/stages/one/schema.json", "timeout": 42,
                "max_output_retries": 2
            })
            .to_string(),
        );
        write(
            &root.join("config/stages/one/prompt.md"),
            "Do the frozen work.",
        );
        write(
            &root.join("config/stages/one/schema.json"),
            &json!({
                "type": "object", "required": ["verdict"],
                "properties": {"verdict": {"type": "string", "enum": ["success"]}}
            })
            .to_string(),
        );
        let loader = ConfigLoader::from_file(&root.join("config/fixture.json")).unwrap();
        let graph: StateMachineGraph = serde_json::from_value(graph).unwrap();
        (guard, loader, graph)
    }

    #[test]
    fn bundle_records_content_hashes_provider_and_limits_without_secrets() {
        let (_guard, loader, graph) = tree();
        let bundle = ExecutionBundle::build(&loader, "run_1", Some("wf_1"), &graph).unwrap();
        assert_eq!(bundle.run_id, "run_1");
        assert_eq!(bundle.workflow_id.as_deref(), Some("wf_1"));
        let stage = bundle.stage("work", "start").unwrap();
        assert_eq!(stage.model, "supported-model");
        assert_eq!(stage.provider.api_key_env, "SUPPORTED_API_KEY");
        assert_eq!(stage.limits.adc_timeout_secs, 42);
        assert_eq!(stage.limits.max_output_retries, 2);
        assert_eq!(stage.prompt, "Do the frozen work.");
        assert_eq!(stage.prompt_hash, hash_bytes(b"Do the frozen work."));
        assert!(bundle.bundle_hash.len() == 64);
        let encoded = serde_json::to_string(&bundle).unwrap();
        assert!(
            !encoded.contains("sk-"),
            "no credential value can be recorded"
        );
        ExecutionBundle::from_json(&encoded).unwrap();
    }

    #[test]
    fn bundle_is_immutable_and_tampering_is_detected() {
        let (_guard, loader, graph) = tree();
        let bundle = ExecutionBundle::build(&loader, "run_2", None, &graph).unwrap();
        let encoded = serde_json::to_string(&bundle).unwrap();
        let mut tampered: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        tampered["flows"]["work"]["start"]["prompt"] = json!("changed after admission");
        let error = ExecutionBundle::from_json(&tampered.to_string())
            .unwrap_err()
            .to_string();
        assert!(error.contains("hash mismatch"), "{error}");
    }

    #[test]
    fn missing_schema_fails_before_a_bundle_exists() {
        let (guard, loader, graph) = tree();
        std::fs::remove_file(guard.0.join("config/stages/one/schema.json")).unwrap();
        let error = format!(
            "{:#}",
            ExecutionBundle::build(&loader, "run_3", None, &graph).unwrap_err()
        );
        assert!(error.contains("cannot read schema"), "{error}");
    }

    #[test]
    fn synthetic_workflow_builds_a_complete_bundle() {
        use tj_test_support::{
            DEFAULT_CHECK, DEFAULT_FINISH, DEFAULT_FLOW, DEFAULT_GATE, DEFAULT_START,
            SyntheticTree, SyntheticWorkflow,
        };

        let tree = SyntheticTree::with_workflow(SyntheticWorkflow::new().with_gate(DEFAULT_GATE));
        let loader = ConfigLoader::from_file(tree.config_path()).unwrap();
        let graph = loader.load_workflow_sources().unwrap().remove(0).graph;
        let bundle = ExecutionBundle::build(&loader, "run_synthetic", None, &graph).unwrap();
        let executable_stages: usize = graph
            .flows
            .values()
            .filter(|flow| flow.flow_type == crate::smg::FlowType::Flow)
            .map(|flow| flow.stages.len())
            .sum();
        let frozen_stages: usize = bundle.flows.values().map(HashMap::len).sum();
        assert_eq!(frozen_stages, executable_stages);
        for stage in [DEFAULT_START, DEFAULT_CHECK, DEFAULT_FINISH] {
            assert!(bundle.stage(DEFAULT_FLOW, stage).is_some(), "{stage}");
        }
        let frozen = bundle.stage(DEFAULT_FLOW, DEFAULT_START).unwrap();
        assert_eq!(frozen.provider.id, "fixture-provider");
        assert_eq!(frozen.model, "fixture-model");
        ExecutionBundle::from_json(&serde_json::to_string(&bundle).unwrap()).unwrap();
    }

    #[test]
    fn unknown_provider_fails_before_a_bundle_exists() {
        let (guard, loader, graph) = tree();
        write(
            &guard.0.join("config/stages/one/agent.json"),
            &json!({
                "id": "one", "name": "One", "prompt": "prompt.md",
                "schema": "config/stages/one/schema.json", "provider_id": "missing"
            })
            .to_string(),
        );
        let error = format!(
            "{:#}",
            ExecutionBundle::build(&loader, "run_4", None, &graph).unwrap_err()
        );
        assert!(
            error.contains("provider 'missing' is not configured"),
            "{error}"
        );
    }

    #[test]
    fn v2_bundle_freezes_the_effective_contract_and_rejects_tampering() {
        let (_guard, loader, graph) = tree();
        let bundle = ExecutionBundle::build(&loader, "run_v2", None, &graph).unwrap();
        assert_eq!(bundle.schema_version, 2);
        let contract = bundle.capability_contract.as_ref().unwrap();
        assert_eq!(contract, &crate::capability::effective_contract(&graph));
        assert_eq!(
            bundle.capability_hash.as_deref(),
            Some(crate::capability::contract_hash(contract).unwrap().as_str())
        );
        let encoded = serde_json::to_string(&bundle).unwrap();
        ExecutionBundle::from_json(&encoded).unwrap();

        let mut tampered_contract: ExecutionBundle = serde_json::from_str(&encoded).unwrap();
        tampered_contract
            .capability_contract
            .as_mut()
            .unwrap()
            .workspace = crate::capability::WorkspaceCapability {
            kind: crate::capability::WorkspaceMode::Repository,
            provisioning: Some(crate::capability::RepositoryProvisioning::Optional),
        };
        tampered_contract.bundle_hash = tampered_contract.compute_hash().unwrap();
        let error = ExecutionBundle::from_json(&serde_json::to_string(&tampered_contract).unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("capability hash mismatch"), "{error}");

        let mut tampered_hash: ExecutionBundle = serde_json::from_str(&encoded).unwrap();
        tampered_hash.capability_hash = Some("0".repeat(64));
        tampered_hash.bundle_hash = tampered_hash.compute_hash().unwrap();
        let error = ExecutionBundle::from_json(&serde_json::to_string(&tampered_hash).unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("capability hash mismatch"), "{error}");
    }

    #[test]
    fn v1_bundle_shape_recomputes_its_original_hash_and_rejects_capability_claims() {
        // Real v1 serialized shape (no capability fields). This literal is
        // intentionally inline so the golden hash never depends on repo assets.
        let raw = r#"{"budgets":{"active_secs":14400,"cost_micros":0,"interaction_nudges":12,"model_calls":120,"output_corrections":24,"question_rounds_per_run":12,"question_rounds_per_stage":6,"tool_calls":200},"bundle_hash":"4aab651ca1b455f219b5525e9ab896390b4ba4caf79c1b6732f9ccb2f2b741c9","created_at":"2026-10-22T00:00:00+00:00","flows":{"flow-a":{"start":{"adc":{"id":"start","input":{"artifacts":[]},"max_output_retries":0,"model":null,"name":"Start","output":{"artifacts":[],"status":null},"prompt":"prompt.md","provider_id":null,"schema":"config/stages/flow-a/start/schema.json","timeout":30,"tools":{"bash":false,"edit":false,"read":true,"write":true}},"adc_hash":"7f8a2900b79cffee2c05414d4861639960b1acb1c34a5e57416d1e2837c97b11","adc_path":"/tmp/legacy-fixture/config/stages/flow-a/start/agent.json","agent_ref":"config/stages/flow-a/start/agent.json","flow_name":"flow-a","limits":{"adc_timeout_secs":30,"max_depth":2,"max_iterations":2,"max_output_retries":0,"max_steps":5},"model":"fixture-model","prompt":"Do the frozen work.\n","prompt_hash":"5bcf23f91fbf84b365e6c24818f3c132fcdf1fb46473137363c1e64a2766b6dd","prompt_path":"/tmp/legacy-fixture/config/stages/flow-a/start/prompt.md","provider":{"api_key_env":"FIXTURE_API_KEY","base_url":"http://127.0.0.1:1","default_model":"fixture-model","id":"fixture-provider","input_cost_micros_per_million":0,"output_cost_micros_per_million":0,"rate_limit_retries":3,"retry_base_ms":250,"transient_retries":2,"type":"open_ai"},"response_schema":{"properties":{"verdict":{"enum":["success"],"type":"string"}},"required":["verdict"],"type":"object"},"schema_hash":"53808614ecf9882a03ff0d142b8f7fd86d49f63347e23cb277096f137f98e278","schema_path":"/tmp/legacy-fixture/config/stages/flow-a/start/schema.json","stage_name":"start","stage_timeout_secs":30}}},"graph":{"description":"frozen v1 fixture","entry":"flow-a","exit":"flow-a","flow-order":["flow-a"],"flows":{"flow-a":{"entry":"start","exit":"start","stages":{"start":{"agent":"config/stages/flow-a/start/agent.json","interactive":false,"routes":{"success":"_exit"},"runner_type":null,"timeout":30}},"type":"flow"}},"limits":{"maxDepth":2,"maxIterations":2,"maxSteps":5},"maturity":"experimental","name":"legacy-fixture","version":1},"run_id":"run_legacy_fixture","schema_version":1,"workflow_hash":"2c71e06ea7a4a6458569a94796ee87fa4ef628f915c6707d0983cf0fc840cccd","workflow_id":null}"#;
        let bundle = ExecutionBundle::from_json(raw).unwrap();
        assert_eq!(bundle.schema_version, 1);
        assert!(bundle.capability_contract.is_none());
        assert!(bundle.capability_hash.is_none());
        assert_eq!(
            bundle.bundle_hash,
            "4aab651ca1b455f219b5525e9ab896390b4ba4caf79c1b6732f9ccb2f2b741c9"
        );
        assert_eq!(
            bundle.workflow_hash,
            "2c71e06ea7a4a6458569a94796ee87fa4ef628f915c6707d0983cf0fc840cccd"
        );
        // Re-serialization keeps the v1 field set and recomputes the original
        // hash. The reader must not add capability fields or rewrite identity.
        let reencoded = serde_json::to_string(&bundle).unwrap();
        assert!(!reencoded.contains("capability_contract"));
        assert!(!reencoded.contains("capability_hash"));
        let reparsed = ExecutionBundle::from_json(&reencoded).unwrap();
        assert_eq!(reparsed.bundle_hash, bundle.bundle_hash);
        assert_eq!(reparsed.workflow_hash, bundle.workflow_hash);
        assert_eq!(bundle.compute_hash().unwrap(), bundle.bundle_hash);

        let mut claiming: serde_json::Value = serde_json::from_str(raw).unwrap();
        claiming["capability_contract"] = json!({"schema_version": 1});
        claiming["capability_hash"] = json!("x");
        let error = ExecutionBundle::from_json(&claiming.to_string())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("v1 execution bundle must not claim") || error.contains("hash mismatch"),
            "{error}"
        );
    }

    #[test]
    fn unsupported_bundle_versions_fail_closed() {
        let (_guard, loader, graph) = tree();
        let bundle = ExecutionBundle::build(&loader, "run_versions", None, &graph).unwrap();
        for version in [0, 3, 99] {
            let mut value = serde_json::to_value(&bundle).unwrap();
            value["schema_version"] = json!(version);
            let error = ExecutionBundle::from_json(&value.to_string())
                .unwrap_err()
                .to_string();
            assert!(error.contains("unsupported execution bundle"), "{error}");
        }
    }
}
