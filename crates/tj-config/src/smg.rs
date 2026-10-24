use crate::capability::WorkflowCapabilities;
use crate::catalog::WorkflowCatalog;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateMachineGraph {
    pub version: u32,
    pub name: String,
    /// Missing maturity on legacy graphs is treated as experimental. This
    /// label never weakens executable-contract validation.
    #[serde(default)]
    pub maturity: WorkflowMaturity,
    #[serde(default)]
    pub description: String,
    /// Optional registry catalog identity. Required for new source imports;
    /// absent on legacy graphs and frozen bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<WorkflowCatalog>,
    /// Optional versioned capability declaration. Absent means the explicit
    /// generic defaults; it is never inferred from names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<WorkflowCapabilities>,
    pub entry: String,
    pub exit: String,
    pub limits: SmgLimits,
    #[serde(rename = "flow-order")]
    pub flow_order: Vec<String>,
    pub flows: HashMap<String, FlowDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmgLimits {
    #[serde(rename = "maxIterations", default = "default_iteration")]
    pub max_iterations: u32,
    #[serde(rename = "maxSteps", default = "default_steps")]
    pub max_steps: u32,
    #[serde(rename = "maxDepth", default = "default_depth")]
    pub max_depth: u32,
}

fn default_iteration() -> u32 {
    3
}
fn default_steps() -> u32 {
    20
}
fn default_depth() -> u32 {
    5
}

impl StateMachineGraph {
    /// True when this graph is the supported repository-free Discovery
    /// profile: it ends at a manager-owned approval gate and has no later
    /// Engineering/Delivery flow.
    pub fn is_terminal_gate_profile(&self) -> bool {
        self.flows
            .get(&self.exit)
            .is_some_and(|flow| flow.flow_type == FlowType::Gate)
            && self.flow_order.last() == Some(&self.exit)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowMaturity {
    Supported,
    #[default]
    Experimental,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FlowType {
    #[default]
    Flow,
    Gate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowDef {
    #[serde(rename = "type", default)]
    pub flow_type: FlowType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub stages: HashMap<String, StageDef>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub routes: HashMap<String, RouteTarget>,
    /// Recognized legacy metadata; it does not authorize a runtime merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_config: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageDef {
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default)]
    pub routes: HashMap<String, RouteTarget>,
    #[serde(default)]
    pub runner_type: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_timeout() -> u64 {
    600
}

/// A route target can be either a simple string like "success", "_next-flow",
/// "_escalate", or a structured cross-flow reference like {"flow": "planning", "stage": "plan"}.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteTarget {
    Stage(String),
    FlowStage { flow: String, stage: String },
}

impl RouteTarget {
    /// Returns the simple string target if this is a Stage variant.
    pub fn as_simple(&self) -> Option<&str> {
        match self {
            RouteTarget::Stage(s) => Some(s),
            RouteTarget::FlowStage { .. } => None,
        }
    }
}

impl fmt::Display for RouteTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouteTarget::Stage(s) => write!(f, "{}", s),
            RouteTarget::FlowStage { flow, stage } => {
                write!(f, "{{flow: {}, stage: {}}}", flow, stage)
            }
        }
    }
}

impl Serialize for RouteTarget {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            RouteTarget::Stage(s) => serializer.serialize_str(s),
            RouteTarget::FlowStage { flow, stage } => {
                use serde::ser::SerializeStruct;
                let mut s = serializer.serialize_struct("RouteTarget", 2)?;
                s.serialize_field("flow", flow)?;
                s.serialize_field("stage", stage)?;
                s.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for RouteTarget {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{self, MapAccess, Visitor};

        struct RouteTargetVisitor;

        impl<'de> Visitor<'de> for RouteTargetVisitor {
            type Value = RouteTarget;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a string or an object with \"flow\" and \"stage\" fields")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<RouteTarget, E> {
                Ok(RouteTarget::Stage(value.to_string()))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<RouteTarget, E> {
                Ok(RouteTarget::Stage(value))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RouteTarget, A::Error> {
                let mut flow = None;
                let mut stage = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "flow" => {
                            if flow.is_some() {
                                return Err(de::Error::duplicate_field("flow"));
                            }
                            flow = Some(map.next_value()?);
                        }
                        "stage" => {
                            if stage.is_some() {
                                return Err(de::Error::duplicate_field("stage"));
                            }
                            stage = Some(map.next_value()?);
                        }
                        unknown => {
                            return Err(de::Error::unknown_field(unknown, &["flow", "stage"]));
                        }
                    }
                }
                let flow = flow.ok_or_else(|| de::Error::missing_field("flow"))?;
                let stage = stage.ok_or_else(|| de::Error::missing_field("stage"))?;
                Ok(RouteTarget::FlowStage { flow, stage })
            }
        }

        deserializer.deserialize_any(RouteTargetVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_route_target_string() {
        let json = r#""success""#;
        let target: RouteTarget = serde_json::from_str(json).unwrap();
        assert_eq!(target, RouteTarget::Stage("success".to_string()));
        assert_eq!(target.as_simple(), Some("success"));
    }

    #[test]
    fn deserialize_route_target_flow_stage() {
        let json = r#"{"flow": "flow-a", "stage": "start"}"#;
        let target: RouteTarget = serde_json::from_str(json).unwrap();
        assert_eq!(
            target,
            RouteTarget::FlowStage {
                flow: "flow-a".to_string(),
                stage: "start".to_string()
            }
        );
        assert_eq!(target.as_simple(), None);
    }

    #[test]
    fn serialize_route_target_string() {
        let target = RouteTarget::Stage("success".to_string());
        let json = serde_json::to_string(&target).unwrap();
        assert_eq!(json, r#""success""#);
    }

    #[test]
    fn serialize_route_target_flow_stage() {
        let target = RouteTarget::FlowStage {
            flow: "flow-b".to_string(),
            stage: "plan".to_string(),
        };
        let json = serde_json::to_string(&target).unwrap();
        assert_eq!(json, r#"{"flow":"flow-b","stage":"plan"}"#);
    }

    #[test]
    fn parse_full_smg_with_gates() {
        let json = r#"{
            "version": 1,
            "name": "Test Pipeline",
            "description": "Test",
            "entry": "flow-a",
            "exit": "delivery",
            "limits": { "maxIterations": 3, "maxSteps": 20, "maxDepth": 5 },
            "flow-order": ["flow-a", "gate-a", "flow-b"],
            "flows": {
                "flow-a": {
                    "type": "flow",
                    "entry": "start",
                    "exit": "start",
                    "stages": {
                        "start": {
                            "agent": "agent.json",
                            "routes": { "success": "_next-flow" }
                        }
                    }
                },
                "gate-a": {
                    "type": "gate",
                    "routes": {
                        "proceed": "_next-flow",
                        "reject": { "flow": "flow-a", "stage": "start" }
                    }
                },
                "flow-b": {
                    "type": "flow",
                    "entry": "plan",
                    "exit": "plan",
                    "stages": {
                        "plan": {
                            "agent": "plan.json",
                            "routes": { "success": "_next-flow" }
                        }
                    }
                }
            }
        }"#;

        let smg: StateMachineGraph = serde_json::from_str(json).unwrap();
        assert_eq!(smg.flows.len(), 3);

        let first_flow = &smg.flows["flow-a"];
        assert_eq!(first_flow.flow_type, FlowType::Flow);
        assert_eq!(first_flow.entry.as_deref(), Some("start"));
        assert!(first_flow.stages.contains_key("start"));

        let gate = &smg.flows["gate-a"];
        assert_eq!(gate.flow_type, FlowType::Gate);
        assert!(gate.entry.is_none());
        assert!(gate.stages.is_empty());
        assert_eq!(gate.routes.len(), 2);
        assert_eq!(
            gate.routes["proceed"],
            RouteTarget::Stage("_next-flow".to_string())
        );
        assert_eq!(
            gate.routes["reject"],
            RouteTarget::FlowStage {
                flow: "flow-a".to_string(),
                stage: "start".to_string()
            }
        );
    }

    #[test]
    fn stage_def_has_no_gate_field() {
        let json = r#"{
            "agent": "agent.json",
            "routes": { "success": "_next-flow" }
        }"#;
        let stage: StageDef = serde_json::from_str(json).unwrap();
        assert_eq!(stage.agent, "agent.json");
        assert!(!stage.interactive);
    }

    #[test]
    fn flow_type_defaults_to_flow() {
        let json = r#"{
            "entry": "start",
            "exit": "end",
            "stages": {}
        }"#;
        let flow: FlowDef = serde_json::from_str(json).unwrap();
        assert_eq!(flow.flow_type, FlowType::Flow);
    }

    #[test]
    fn legacy_graph_maturity_defaults_to_experimental_and_unknown_fields_fail() {
        let base = serde_json::json!({
            "version":1,"name":"x","entry":"work","exit":"work",
            "limits":{"maxIterations":2,"maxSteps":5,"maxDepth":2},
            "flow-order":["work"],
            "flows":{"work":{"entry":"start","exit":"start","stages":{"start":{"agent":"a.json","routes":{"success":"_exit"}}}}}
        });
        let smg: StateMachineGraph = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(smg.maturity, WorkflowMaturity::Experimental);

        let mut unknown = base.clone();
        unknown["obsoleteMode"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<StateMachineGraph>(unknown)
                .unwrap_err()
                .to_string()
                .contains("obsoleteMode")
        );

        let mut unknown_stage = base;
        unknown_stage["flows"]["work"]["stages"]["start"]["docker"] = serde_json::json!({});
        assert!(
            serde_json::from_value::<StateMachineGraph>(unknown_stage)
                .unwrap_err()
                .to_string()
                .contains("docker")
        );

        let target: serde_json::Result<RouteTarget> =
            serde_json::from_str(r#"{"flow":"a","stage":"b","extra":1}"#);
        assert!(target.unwrap_err().to_string().contains("unknown field"));
        let duplicate: serde_json::Result<RouteTarget> =
            serde_json::from_str(r#"{"flow":"a","flow":"b","stage":"c"}"#);
        assert!(
            duplicate
                .unwrap_err()
                .to_string()
                .contains("duplicate field")
        );
    }
}
