//! Runtime-synthesized neutral workflow packages.
//!
//! Tests use these builders instead of reading any stored workflow, example,
//! fixture or ignored-local package. The real production loader still validates
//! the generated graph, capabilities, ADCs, prompts and verdict schemas.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

use crate::write_file;

/// One neutral agent definition for a synthesized package. The verdict list
/// must exactly match the route keys of every stage that references the agent.
#[derive(Debug, Clone)]
pub struct SyntheticPackageAgent {
    id: String,
    name: String,
    verdicts: Vec<String>,
    read: bool,
    write: bool,
    bash: bool,
    edit: bool,
    max_output_retries: u32,
    timeout: u64,
}

impl SyntheticPackageAgent {
    pub fn new(id: impl Into<String>, verdicts: &[&str]) -> Self {
        let id = id.into();
        Self {
            name: format!("{id} agent"),
            id,
            verdicts: verdicts.iter().map(|value| (*value).to_string()).collect(),
            read: true,
            write: false,
            bash: false,
            edit: false,
            max_output_retries: 3,
            timeout: 300,
        }
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn tools(mut self, read: bool, write: bool, bash: bool, edit: bool) -> Self {
        self.read = read;
        self.write = write;
        self.bash = bash;
        self.edit = edit;
        self
    }

    pub fn max_output_retries(mut self, retries: u32) -> Self {
        self.max_output_retries = retries;
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn verdicts(&self) -> &[String] {
        &self.verdicts
    }
}

/// A disposable self-contained package: `workflow.smg.json`, one neutral ADC
/// plus prompt per agent and one response schema per agent.
pub struct SyntheticPackage {
    root: PathBuf,
    _temp: Option<tempfile::TempDir>,
}

impl SyntheticPackage {
    pub fn new(workflow: &Value, agents: &[SyntheticPackageAgent]) -> Self {
        let temp = tempfile::tempdir().expect("create synthetic package temp dir");
        let root = temp.path().to_path_buf();
        write_synthetic_package(&root, workflow, agents).expect("write synthetic package");
        Self {
            root,
            _temp: Some(temp),
        }
    }

    pub fn at(
        root: impl Into<PathBuf>,
        workflow: &Value,
        agents: &[SyntheticPackageAgent],
    ) -> Self {
        let root = root.into();
        write_synthetic_package(&root, workflow, agents).expect("write synthetic package");
        Self { root, _temp: None }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn workflow_path(&self) -> PathBuf {
        self.root.join("workflow.smg.json")
    }
}

/// Write the layout a real package source uses. The manifest is written last so
/// a partially written tree is never mistaken for a complete package.
pub fn write_synthetic_package(
    root: &Path,
    workflow: &Value,
    agents: &[SyntheticPackageAgent],
) -> std::io::Result<()> {
    for agent in agents {
        let directory = root.join("agents").join(&agent.id);
        let schema = json!({
            "description": format!(
                "Neutral {} agent response; verdict is exactly one stage route key.",
                agent.id
            ),
            "type": "object",
            "required": ["verdict", "summary"],
            "properties": {
                "verdict": {"type": "string", "enum": agent.verdicts},
                "summary": {"type": "string"},
                "content": {"type": "string"}
            }
        });
        write_file(
            &directory.join("agent.json"),
            json!({
                "id": agent.id,
                "name": agent.name,
                "prompt": "prompt.md",
                "schema": format!("schemas/{}.json", agent.id),
                "max_output_retries": agent.max_output_retries,
                "output": {
                    "artifacts": [{
                        "name": "output.md",
                        "required": true,
                        "verify": "non-empty"
                    }]
                },
                "tools": {
                    "read": agent.read,
                    "write": agent.write,
                    "bash": agent.bash,
                    "edit": agent.edit
                },
                "timeout": agent.timeout
            })
            .to_string()
            .as_bytes(),
        )?;
        write_file(
            &directory.join("prompt.md"),
            format!("Neutral {} stage prompt.\n", agent.id).as_bytes(),
        )?;
        write_file(
            &root.join("schemas").join(format!("{}.json", agent.id)),
            schema.to_string().as_bytes(),
        )?;
    }
    write_file(
        &root.join("workflow.smg.json"),
        workflow.to_string().as_bytes(),
    )
}

/// Neutral graph shared by the capability journeys. Tests write it into a
/// tempdir; no repository asset is consulted.
pub fn neutral_capability_workflow(catalog_id: &str, include_build: bool) -> Value {
    let mut flow_order = vec!["collect", "collect-gate", "shape", "shape-gate"];
    if include_build {
        flow_order.push("build");
    }
    let mut capabilities = json!({
        "schema_version": 1,
        "workspace": {"kind": "none"},
        "gates": {"collect-gate": "packet", "shape-gate": "plan"},
        "requirements_packet": {
            "produce_flow": "collect",
            "producer_stage": "collector",
            "binding_gate": "collect-gate"
        },
        "task_plan": {
            "produce_flow": "shape",
            "validate_stage": "check",
            "binding_gate": "shape-gate",
            "source_packet": true,
            "task_selection": {"after_gate": "shape-gate", "required": true}
        }
    });
    let mut flows = json!({
        "collect": {
            "type": "flow", "entry": "writer", "exit": "collector",
            "merge_strategy": "none",
            "stages": {
                "writer": {
                    "agent": "writer", "interactive": false,
                    "routes": {"generated": "collector"}
                },
                "collector": {
                    "agent": "writer", "interactive": false,
                    "routes": {"generated": "_next-flow"}
                }
            }
        },
        "collect-gate": {
            "type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"},
            "stages": {}
        },
        "shape": {
            "type": "flow", "entry": "survey", "exit": "check",
            "merge_strategy": "none",
            "stages": {
                "survey": {
                    "agent": "planner",
                    "routes": {"success": "outline", "cannot-complete": "_escalate"}
                },
                "outline": {
                    "agent": "planner",
                    "routes": {"success": "sketch", "cannot-complete": "_escalate"}
                },
                "sketch": {
                    "agent": "planner",
                    "routes": {"success": "align", "cannot-complete": "_escalate"}
                },
                "align": {
                    "agent": "planner",
                    "routes": {"success": "plan", "cannot-complete": "_escalate"}
                },
                "plan": {
                    "agent": "planner",
                    "routes": {"success": "check", "cannot-complete": "_escalate"}
                },
                "check": {
                    "agent": "checker",
                    "routes": {"approved": "_next-flow", "rejected": "plan"}
                }
            }
        },
        "shape-gate": {
            "type": "gate", "routes": {"approved": "_next-flow", "rejected": "_rework"},
            "stages": {}
        }
    });
    if include_build {
        capabilities["workspace"] = json!({"kind": "repository", "provisioning": "optional"});
        capabilities["publication"] = json!({
            "flow": "build",
            "mode": "draft_pr",
            "requires_task_selection": true,
            "required_evidence": [
                {
                    "role": "test", "stage": "test",
                    "execution": "shell", "criterion_mapping": true
                },
                {"role": "review", "stage": "review"},
                {"role": "acceptance", "stage": "accept"}
            ]
        });
        capabilities["external_delivery"] = json!({
            "after_flow": "build",
            "explicit_request_only": true
        });
        flows["build"] = json!({
            "type": "flow", "entry": "scope", "exit": "accept",
            "merge_strategy": "none",
            "stages": {
                "scope": {
                    "agent": "planner",
                    "routes": {"success": "blueprint", "cannot-complete": "_escalate"}
                },
                "blueprint": {
                    "agent": "planner",
                    "routes": {"success": "implement", "cannot-complete": "_escalate"}
                },
                "implement": {
                    "agent": "builder",
                    "routes": {"success": "test", "cannot-complete": "_escalate"}
                },
                "test": {
                    "agent": "tester",
                    "routes": {"success": "review", "failed": "implement", "cannot-complete": "_escalate"}
                },
                "review": {
                    "agent": "reviewer",
                    "routes": {"success": "accept", "failed": "implement", "cannot-complete": "_escalate"}
                },
                "accept": {
                    "agent": "approver",
                    "routes": {"success": "_exit", "failed": "implement", "cannot-complete": "_escalate"}
                }
            }
        });
    }
    json!({
        "version": 1,
        "name": "Neutral capability package",
        "description": "Runtime-synthesized neutral package; no stored domain workflow.",
        "maturity": "experimental",
        "catalog": {"id": catalog_id, "label": "Neutral capability package"},
        "entry": "collect",
        "exit": if include_build { "build" } else { "shape-gate" },
        "limits": {"maxIterations": 3, "maxSteps": 40, "maxDepth": 6},
        "flow-order": flow_order,
        "flows": flows,
        "capabilities": capabilities
    })
}

/// Neutral agents whose verdict space exactly matches the stages using them.
pub fn neutral_capability_package_agents(include_build: bool) -> Vec<SyntheticPackageAgent> {
    let mut agents = vec![
        SyntheticPackageAgent::new("writer", &["generated"])
            .named("Neutral writer")
            .tools(true, true, false, false),
        SyntheticPackageAgent::new("planner", &["success", "cannot-complete"])
            .named("Neutral planner")
            .tools(true, false, false, false),
        SyntheticPackageAgent::new("checker", &["approved", "rejected"])
            .named("Neutral checker")
            .tools(true, false, false, false),
    ];
    if include_build {
        agents.extend([
            SyntheticPackageAgent::new("builder", &["success", "cannot-complete"])
                .named("Neutral builder")
                .tools(true, true, true, true),
            SyntheticPackageAgent::new("tester", &["success", "failed", "cannot-complete"])
                .named("Neutral tester")
                .tools(true, true, true, true),
            SyntheticPackageAgent::new("reviewer", &["success", "failed", "cannot-complete"])
                .named("Neutral reviewer")
                .tools(true, true, true, true),
            SyntheticPackageAgent::new("approver", &["success", "failed", "cannot-complete"])
                .named("Neutral approver")
                .tools(true, true, true, true),
        ]);
    }
    agents
}
