use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinitionConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub input: InputConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_max_output_retries")]
    pub max_output_retries: u32,
}

fn default_timeout() -> u64 {
    600
}

fn default_max_output_retries() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    #[serde(default)]
    pub artifacts: Vec<ArtifactDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    #[serde(default)]
    pub artifacts: Vec<ArtifactDef>,
    #[serde(default)]
    pub status: Option<StatusDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    #[serde(default = "default_true")]
    pub read: bool,
    #[serde(default = "default_true")]
    pub write: bool,
    #[serde(default)]
    pub bash: bool,
    #[serde(default)]
    pub edit: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDef {
    pub name: String,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub verify: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusDef {
    #[serde(default)]
    pub schema: String,
}

#[cfg(test)]
mod tests {
    use super::AgentDefinitionConfig;

    #[test]
    fn max_output_retries_defaults_to_three_when_key_absent() {
        let adc: AgentDefinitionConfig =
            serde_json::from_str(r#"{"id": "intake", "name": "Intake"}"#).unwrap();
        assert_eq!(adc.max_output_retries, 3);
    }

    #[test]
    fn max_output_retries_parses_from_agent_json() {
        let adc: AgentDefinitionConfig = serde_json::from_str(
            r#"{"id": "evaluate", "name": "Evaluate", "max_output_retries": 5}"#,
        )
        .unwrap();
        assert_eq!(adc.max_output_retries, 5);
    }

    #[test]
    fn max_output_retries_survives_serde_round_trip() {
        let adc: AgentDefinitionConfig =
            serde_json::from_str(r#"{"id": "a", "name": "A", "max_output_retries": 7}"#).unwrap();
        let raw = serde_json::to_string(&adc).unwrap();
        let back: AgentDefinitionConfig = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.max_output_retries, 7);
    }
}
