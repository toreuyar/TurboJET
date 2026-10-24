//! Synthetic, asset-free fixtures for TurboJet tests.
//!
//! Nothing in this crate reads repository assets, embeds shipped graph JSON or
//! names a shipped workflow. Default tests build neutral graphs and disposable
//! config/ADC/prompt/schema trees here, so the engine suite can run in a
//! checkout with `workflows/`, `config/local/`, legacy graph files and
//! `examples/` removed.
//!
//! The builder emits plain `serde_json::Value` documents so this crate does not
//! depend on the production crates it supports (and therefore cannot create a
//! dependency cycle through a dev-dependency).

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

mod synthetic_package;
pub use synthetic_package::{
    SyntheticPackage, SyntheticPackageAgent, neutral_capability_package_agents,
    neutral_capability_workflow, write_synthetic_package,
};

pub const DEFAULT_FLOW: &str = "flow-a";
pub const DEFAULT_SECOND_FLOW: &str = "flow-b";
pub const DEFAULT_GATE: &str = "gate-a";
pub const DEFAULT_START: &str = "start";
pub const DEFAULT_CHECK: &str = "check";
pub const DEFAULT_FINISH: &str = "finish";

/// One synthetic stage. Routes are explicit verdict -> target pairs.
#[derive(Debug, Clone)]
pub struct SyntheticStage {
    name: String,
    interactive: bool,
    routes: Vec<(String, String)>,
}

impl SyntheticStage {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            interactive: false,
            routes: Vec::new(),
        }
    }

    pub fn interactive(mut self) -> Self {
        self.interactive = true;
        self
    }

    pub fn route(mut self, verdict: impl Into<String>, target: impl Into<String>) -> Self {
        self.routes.push((verdict.into(), target.into()));
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// One synthetic executable flow. A gate is represented by `SyntheticFlow::gate`.
#[derive(Debug, Clone)]
pub struct SyntheticFlow {
    name: String,
    stages: Vec<SyntheticStage>,
    is_gate: bool,
}

impl SyntheticFlow {
    /// A linear flow whose final stage routes `success` to `_exit`. The graph
    /// writer rewrites that to `_next-flow` when this is not the final flow.
    pub fn linear(name: impl Into<String>, stages: &[&str]) -> Self {
        assert!(
            !stages.is_empty(),
            "a synthetic flow needs at least one stage"
        );
        let mut built = Vec::new();
        for (index, stage) in stages.iter().enumerate() {
            let target = stages.get(index + 1).copied().unwrap_or("_exit");
            built.push(SyntheticStage::new(*stage).route("success", target));
        }
        Self {
            name: name.into(),
            stages: built,
            is_gate: false,
        }
    }

    pub fn with_stages(name: impl Into<String>, stages: Vec<SyntheticStage>) -> Self {
        assert!(
            !stages.is_empty(),
            "a synthetic flow needs at least one stage"
        );
        Self {
            name: name.into(),
            stages,
            is_gate: false,
        }
    }

    /// A manager-owned approval gate with the required approved/rejected routes.
    pub fn gate(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            stages: Vec::new(),
            is_gate: true,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Neutral synthetic workflow builder.
///
/// The default is one flow (`flow-a`) with `start -> check -> finish`, a retry
/// edge from `check` back to `start`, and `_exit` from `finish`.
#[derive(Debug, Clone)]
pub struct SyntheticWorkflow {
    name: String,
    catalog_id: String,
    catalog_label: Option<String>,
    catalog_description: Option<String>,
    catalog_default: bool,
    capabilities: Option<Value>,
    flows: Vec<SyntheticFlow>,
}

impl Default for SyntheticWorkflow {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntheticWorkflow {
    pub fn new() -> Self {
        let flow = SyntheticFlow::with_stages(
            DEFAULT_FLOW,
            vec![
                SyntheticStage::new(DEFAULT_START).route("success", DEFAULT_CHECK),
                SyntheticStage::new(DEFAULT_CHECK)
                    .route("success", DEFAULT_FINISH)
                    .route("retry", DEFAULT_START),
                SyntheticStage::new(DEFAULT_FINISH).route("success", "_exit"),
            ],
        );
        Self {
            name: "synthetic-workflow".into(),
            catalog_id: "synthetic-workflow".into(),
            catalog_label: None,
            catalog_description: None,
            catalog_default: false,
            capabilities: None,
            flows: vec![flow],
        }
    }

    /// Declare a capability contract with fully parameterized neutral names.
    /// Default fixtures omit the block and therefore exercise the explicit
    /// generic contract.
    pub fn with_capabilities(mut self, capabilities: Value) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    pub fn capabilities_json(&self) -> Option<&Value> {
        self.capabilities.as_ref()
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Explicit registry catalog identity; arbitrary neutral ids are supported.
    pub fn with_catalog(mut self, id: impl Into<String>) -> Self {
        self.catalog_id = id.into();
        self
    }

    pub fn catalog_label(mut self, label: impl Into<String>) -> Self {
        self.catalog_label = Some(label.into());
        self
    }

    pub fn catalog_description(mut self, description: impl Into<String>) -> Self {
        self.catalog_description = Some(description.into());
        self
    }

    pub fn catalog_default(mut self, default: bool) -> Self {
        self.catalog_default = default;
        self
    }

    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }

    /// Replace the graph with exactly the supplied flows.
    pub fn with_flows(mut self, flows: Vec<SyntheticFlow>) -> Self {
        assert!(
            !flows.is_empty(),
            "a synthetic workflow needs at least one flow"
        );
        self.flows = flows;
        self
    }

    /// Add a terminal approval gate after the current final flow.
    pub fn with_gate(mut self, gate: impl Into<String>) -> Self {
        self.flows.push(SyntheticFlow::gate(gate));
        self
    }

    /// Append a one-stage executable flow after the current final flow.
    pub fn with_second_flow(mut self, flow: impl Into<String>, stage: impl Into<String>) -> Self {
        let stage = stage.into();
        self.flows
            .push(SyntheticFlow::linear(flow, &[stage.as_str()]));
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn flow_names(&self) -> Vec<String> {
        self.flows.iter().map(|flow| flow.name.clone()).collect()
    }

    pub fn entry(&self) -> String {
        self.flows
            .first()
            .expect("builder requires a flow")
            .name
            .clone()
    }

    pub fn exit(&self) -> String {
        self.flows
            .last()
            .expect("builder requires a flow")
            .name
            .clone()
    }

    /// Stage names for a flow, in declaration order.
    pub fn stage_names(&self, flow: &str) -> Vec<String> {
        self.flows
            .iter()
            .find(|candidate| candidate.name == flow)
            .map(|flow| flow.stages.iter().map(|stage| stage.name.clone()).collect())
            .unwrap_or_default()
    }

    /// Serialize the graph as the JSON document the production loader reads.
    pub fn to_json(&self) -> Value {
        let flow_order: Vec<String> = self.flows.iter().map(|flow| flow.name.clone()).collect();
        let final_flow = flow_order.last().cloned().unwrap_or_default();
        let mut flows = Map::new();

        for flow in &self.flows {
            if flow.is_gate {
                flows.insert(
                    flow.name.clone(),
                    json!({
                        "type": "gate",
                        "routes": {"approved": "_next-flow", "rejected": "_rework"},
                        "stages": {}
                    }),
                );
                continue;
            }

            let is_final = flow.name == final_flow;
            let first = flow
                .stages
                .first()
                .expect("stage flow checked")
                .name
                .clone();
            let last = flow.stages.last().expect("stage flow checked").name.clone();
            let mut stages = Map::new();
            for stage in &flow.stages {
                let mut routes = Map::new();
                for (verdict, target) in &stage.routes {
                    let target = if !is_final && stage.name == last && target == "_exit" {
                        "_next-flow"
                    } else {
                        target.as_str()
                    };
                    routes.insert(verdict.clone(), Value::String(target.into()));
                }
                stages.insert(
                    stage.name.clone(),
                    json!({
                        "agent": agent_path(&flow.name, &stage.name),
                        "interactive": stage.interactive,
                        "routes": Value::Object(routes)
                    }),
                );
            }
            flows.insert(
                flow.name.clone(),
                json!({
                    "type": "flow",
                    "entry": first,
                    "exit": last,
                    "stages": Value::Object(stages)
                }),
            );
        }

        let mut catalog = json!({
            "id": self.catalog_id,
            "default": self.catalog_default,
        });
        if let Some(label) = &self.catalog_label {
            catalog["label"] = Value::String(label.clone());
        }
        if let Some(description) = &self.catalog_description {
            catalog["description"] = Value::String(description.clone());
        }

        let mut document = json!({
            "version": 1,
            "name": self.name,
            "maturity": "experimental",
            "description": "Asset-free synthetic fixture",
            "catalog": catalog,
            "entry": self.entry(),
            "exit": self.exit(),
            "limits": {"maxIterations": 3, "maxSteps": 20, "maxDepth": 5},
            "flow-order": flow_order,
            "flows": Value::Object(flows)
        });
        if let Some(capabilities) = &self.capabilities {
            document["capabilities"] = capabilities.clone();
        }
        document
    }

    /// Stage resources in `(flow, stage, route keys)` form, used by the tree writer.
    fn stage_resources(&self) -> Vec<(String, String, Vec<String>)> {
        let mut resources = Vec::new();
        for flow in &self.flows {
            for stage in &flow.stages {
                let routes = stage
                    .routes
                    .iter()
                    .map(|(verdict, _)| verdict.clone())
                    .collect();
                resources.push((flow.name.clone(), stage.name.clone(), routes));
            }
        }
        resources
    }
}

fn agent_path(flow: &str, stage: &str) -> String {
    format!("config/stages/{flow}/{stage}/agent.json")
}

/// Disposable (or caller-owned) synthetic workflow tree.
///
/// Layout:
/// ```text
/// <root>/config/fixture.json                 config passed to ConfigLoader
/// <root>/config/graph.json                   synthetic SMG document
/// <root>/config/stages/<flow>/<stage>/{agent.json,prompt.md,schema.json}
/// ```
/// `fixture.json` names only relative paths, so it is portable inside the tree
/// and never points at repository assets.
pub struct SyntheticTree {
    root: PathBuf,
    config_path: PathBuf,
    graph_path: PathBuf,
    workflow: SyntheticWorkflow,
    _temp: Option<tempfile::TempDir>,
}

impl Default for SyntheticTree {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntheticTree {
    /// Create a tree under a fresh `tempfile::tempdir()`.
    pub fn new() -> Self {
        Self::with_workflow(SyntheticWorkflow::new())
    }

    pub fn with_workflow(workflow: SyntheticWorkflow) -> Self {
        let temp = tempfile::tempdir().expect("create synthetic temp dir");
        let root = temp.path().to_path_buf();
        let mut tree = Self::write_at(root, workflow);
        tree._temp = Some(temp);
        tree
    }

    /// Write a synthetic tree under an existing caller-owned directory.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self::write_at(root.into(), SyntheticWorkflow::new())
    }

    /// Write a synthetic tree under an existing directory with a custom graph.
    pub fn at_with_workflow(root: impl Into<PathBuf>, workflow: SyntheticWorkflow) -> Self {
        Self::write_at(root.into(), workflow)
    }

    fn write_at(root: PathBuf, workflow: SyntheticWorkflow) -> Self {
        let config_path = root.join("config/fixture.json");
        let graph_path = root.join("config/graph.json");
        let tree = Self {
            root,
            config_path,
            graph_path,
            workflow,
            _temp: None,
        };
        tree.write_graph().expect("write synthetic graph");
        tree.write_config().expect("write synthetic config");
        tree.write_stage_assets()
            .expect("write synthetic stage assets");
        tree
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn graph_path(&self) -> &Path {
        &self.graph_path
    }

    pub fn workflow(&self) -> &SyntheticWorkflow {
        &self.workflow
    }

    pub fn workflow_json(&self) -> Value {
        self.workflow.to_json()
    }

    pub fn write_graph(&self) -> std::io::Result<()> {
        write_file(
            &self.graph_path,
            self.workflow.to_json().to_string().as_bytes(),
        )
    }

    /// Base config. Tests may replace or extend it with `write_config` /
    /// `update_config`; no repository path or shipped workflow is referenced.
    pub fn config_json(&self) -> Value {
        json!({
            "listen_addr": "127.0.0.1:0",
            "log_level": "warn",
            "database_path": self.root.join("state.db").to_string_lossy(),
            "workspace_base": self.root.join("workspaces").to_string_lossy(),
            "workflow_sources": [{"id": "fixture", "path": "config/graph.json", "kind": "file"}],
            "adc_dir": "config/stages",
            "providers": [{
                "id": "fixture-provider",
                "type": "open_ai",
                "api_key_env": "TJ_SYNTHETIC_API_KEY",
                "base_url": "http://127.0.0.1:1",
                "default_model": "fixture-model"
            }],
            "agents": [{
                "id": "fixture-agent",
                "provider_id": "fixture-provider",
                "model": "fixture-model",
                "default": true,
                "enabled": true,
                "tools": [],
                "system_prompt": "synthetic fixture"
            }]
        })
    }

    pub fn write_config(&self) -> std::io::Result<()> {
        write_file(&self.config_path, self.config_json().to_string().as_bytes())
    }

    pub fn write_config_value(&self, config: &Value) -> std::io::Result<()> {
        write_file(&self.config_path, config.to_string().as_bytes())
    }

    pub fn update_config(&self, update: impl FnOnce(&mut Value)) -> std::io::Result<()> {
        let mut config = self.config_json();
        update(&mut config);
        self.write_config_value(&config)
    }

    pub fn write_fixture_file(
        &self,
        relative: impl AsRef<Path>,
        content: impl AsRef<[u8]>,
    ) -> std::io::Result<PathBuf> {
        let path = self.root.join(relative);
        write_file(&path, content.as_ref())?;
        Ok(path)
    }

    fn write_stage_assets(&self) -> std::io::Result<()> {
        for (flow, stage, routes) in self.workflow.stage_resources() {
            let dir = self.root.join("config/stages").join(&flow).join(&stage);
            let schema = json!({
                "type": "object",
                "required": ["verdict"],
                "properties": {
                    "verdict": {"type": "string", "enum": routes},
                    "content": {"type": "string"},
                    "summary": {"type": "string"}
                }
            });
            write_file(
                &dir.join("agent.json"),
                json!({
                    "id": stage,
                    "name": stage,
                    "prompt": "prompt.md",
                    "schema": format!("config/stages/{flow}/{stage}/schema.json"),
                    "timeout": 30,
                    "max_output_retries": 0,
                    "tools": {"read": false, "write": false, "edit": false, "bash": false}
                })
                .to_string()
                .as_bytes(),
            )?;
            write_file(&dir.join("prompt.md"), b"synthetic fixture prompt\n")?;
            write_file(&dir.join("schema.json"), schema.to_string().as_bytes())?;
        }
        Ok(())
    }
}

/// Guard for tests that should never read shipped repository assets. Pass every
/// path a test is about to open; synthetic temp paths are accepted.
pub fn assert_asset_free_paths<'a>(paths: impl IntoIterator<Item = &'a Path>) {
    let violations: Vec<String> = paths
        .into_iter()
        .filter(|path| is_repository_asset_path(path))
        .map(|path| path.display().to_string())
        .collect();
    assert!(
        violations.is_empty(),
        "default tests must not read repository workflow assets: {violations:?}"
    );
}

/// True for the shipped-asset locations removed by `scripts/check-asset-free.sh`.
pub fn is_repository_asset_path(path: &Path) -> bool {
    let normalized = path.to_string_lossy().replace('\\', "/");
    normalized.contains("/config/turbojet.json")
        || normalized.contains("/config/local/")
        || normalized.ends_with("/config/turbojet.json")
        || (normalized.contains("/config/") && normalized.ends_with(".smg.json"))
        || normalized.contains("/workflows/")
        || normalized.contains("/tests/fixtures/packages/")
        || normalized.contains("/examples/")
}

fn write_file(path: &Path, content: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

#[cfg(feature = "scripted-model")]
pub mod scripted {
    //! Minimal scripted OpenAI-compatible HTTP model. Async ownership makes a
    //! shared server awkward inside synchronous tests, so callers that need it
    //! opt into the `scripted-model` feature.

    use serde_json::{Value, json};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    pub struct ScriptedModel {
        port: u16,
        responses: Arc<Mutex<VecDeque<Value>>>,
        requests: Arc<AtomicUsize>,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl ScriptedModel {
        pub async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let responses = Arc::new(Mutex::new(VecDeque::new()));
            let requests = Arc::new(AtomicUsize::new(0));
            let (shutdown, mut stop) = tokio::sync::oneshot::channel::<()>();
            let responses_for_server = Arc::clone(&responses);
            let requests_for_server = Arc::clone(&requests);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let Ok((mut stream, _)) = accepted else { continue; };
                            let responses = Arc::clone(&responses_for_server);
                            let requests = Arc::clone(&requests_for_server);
                            tokio::spawn(async move {
                                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                                let mut buf = vec![0u8; 65536];
                                let _ = stream.read(&mut buf).await;
                                requests.fetch_add(1, Ordering::SeqCst);
                                let response = responses.lock().unwrap().pop_front()
                                    .unwrap_or_else(|| text_response("no scripted response left"));
                                let body = serde_json::to_string(&response).unwrap();
                                let http = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body.len(), body
                                );
                                let _ = stream.write_all(http.as_bytes()).await;
                                let _ = stream.flush().await;
                            });
                        }
                        _ = &mut stop => break,
                    }
                }
            });
            Self {
                port,
                responses,
                requests,
                shutdown: Some(shutdown),
            }
        }

        pub fn base_url(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        pub fn push(&self, response: Value) {
            self.responses.lock().unwrap().push_back(response);
        }

        pub fn request_count(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }
    }

    impl Drop for ScriptedModel {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }

    pub fn text_response(text: &str) -> Value {
        json!({
            "id": "chatcmpl-synthetic",
            "object": "chat.completion",
            "model": "fixture-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
        })
    }

    pub fn tool_response(name: &str, args: Value) -> Value {
        json!({
            "id": "chatcmpl-synthetic-tool",
            "object": "chat.completion",
            "model": "fixture-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{"id": format!("call-{name}"), "type": "function",
                        "function": {"name": name, "arguments": args.to_string()}}]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
        })
    }

    pub fn verdict_response(value: &str) -> Value {
        tool_response(
            "verdict",
            json!({"verdict": value, "content": format!("result {value}"), "summary": "synthetic"}),
        )
    }

    pub fn question_response(text: &str) -> Value {
        tool_response(
            "question",
            json!({
                "question": text,
                "options": [{"id": "a", "label": "A"}, {"id": "b", "label": "B"}],
                "multi_select": false,
                "free_form": true
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_graph_is_valid_shaped_and_asset_free() {
        let tree = SyntheticTree::new();
        let graph = tree.workflow_json();
        assert_eq!(graph["entry"], DEFAULT_FLOW);
        assert_eq!(graph["exit"], DEFAULT_FLOW);
        assert_eq!(graph["flow-order"], json!([DEFAULT_FLOW]));
        assert_eq!(
            graph["flows"][DEFAULT_FLOW]["stages"][DEFAULT_CHECK]["routes"]["retry"],
            DEFAULT_START
        );
        assert!(!is_repository_asset_path(tree.config_path()));
        assert!(!is_repository_asset_path(tree.graph_path()));
        assert!(tree.root().join("config/stages").is_dir());
    }

    #[test]
    fn gate_and_second_flow_rewrite_the_exit_route() {
        let workflow = SyntheticWorkflow::new()
            .with_gate(DEFAULT_GATE)
            .with_second_flow(DEFAULT_SECOND_FLOW, "finish");
        let graph = workflow.to_json();
        assert_eq!(
            graph["flow-order"],
            json!([DEFAULT_FLOW, DEFAULT_GATE, DEFAULT_SECOND_FLOW])
        );
        assert_eq!(
            graph["flows"][DEFAULT_FLOW]["stages"]["finish"]["routes"]["success"],
            "_next-flow"
        );
        assert_eq!(
            graph["flows"][DEFAULT_GATE]["routes"]["approved"],
            "_next-flow"
        );
        assert_eq!(
            graph["flows"][DEFAULT_SECOND_FLOW]["stages"]["finish"]["routes"]["success"],
            "_exit"
        );
    }

    #[test]
    fn shipped_asset_locations_are_rejected() {
        let config = |name: &str| format!("/repo/config/{name}");
        let forbidden = [
            config("turbojet.json"),
            config(&format!("default{}", ".smg.json")),
            config("local/ignored.smg.json"),
            format!("/repo/{}/wf.json", "examples"),
            "/repo/workflows/fixture/workflow.smg.json".into(),
            "/repo/tests/fixtures/packages/example/workflow.smg.json".into(),
        ];
        for path in forbidden {
            assert!(is_repository_asset_path(Path::new(&path)), "{path}");
        }
        assert!(!is_repository_asset_path(Path::new(
            "/tmp/x/config/fixture.json"
        )));
        assert!(!is_repository_asset_path(Path::new(
            "/tmp/x/config/graph.json"
        )));
    }
}
