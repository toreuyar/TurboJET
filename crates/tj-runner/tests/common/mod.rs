use std::path::Path;
use tj_config::adc::{AgentDefinitionConfig, InputConfig, OutputConfig, ToolsConfig};
use tj_runner::types::{StageLogSink, StageResult, StageStatus, TaskDefinition};

/// Create a minimal TaskDefinition with all tools enabled for testing.
pub fn make_task_definition(workspace: &Path, interactive: bool) -> TaskDefinition {
    TaskDefinition {
        stage_id: "test-stage".to_string(),
        job_id: "test-job".to_string(),
        run_id: "test-run".to_string(),
        prompt: "Execute the test task.".to_string(),
        prompt_text: None,
        input_artifacts: serde_json::Value::Null,
        output_dir: workspace.to_path_buf(),
        output_file: None,
        timeout_secs: 30,
        adc: make_adc(),
        runner_type: String::new(),
        worktree_path: Some(workspace.to_path_buf()),
        interactive,
        response_schema: None,
    }
}

/// Create a minimal AgentDefinitionConfig with all tools enabled.
pub fn make_adc() -> AgentDefinitionConfig {
    AgentDefinitionConfig {
        id: "test-agent".to_string(),
        name: "Test Agent".to_string(),
        prompt: String::new(),
        schema: String::new(),
        input: InputConfig::default(),
        output: OutputConfig::default(),
        tools: ToolsConfig {
            read: true,
            write: true,
            edit: true,
            bash: true,
        },
        timeout: 600,
        provider_id: None,
        model: None,
        max_output_retries: 3,
    }
}

/// Assert that a stage result succeeded and return its output.
pub fn assert_success(result: StageResult) -> serde_json::Value {
    assert!(
        matches!(result.status, StageStatus::Success),
        "Expected Success, got {:?}: {:?}",
        result.status,
        result.error
    );
    result.output.unwrap_or(serde_json::Value::Null)
}

/// Recording [`StageLogSink`] for asserting the runner-reported activity rows
/// (`llm_request`, `output_validation`, ...). Only used by the interactive
/// verdict test binary, hence the allow.
#[allow(dead_code)]
pub struct RecordingSink {
    rows: std::sync::Mutex<Vec<(String, String, String, String)>>,
}

#[allow(dead_code)]
impl RecordingSink {
    pub fn new() -> Self {
        Self {
            rows: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// All recorded rows as `(stage_id, stage_name, kind, content)` tuples.
    pub fn rows(&self) -> Vec<(String, String, String, String)> {
        self.rows.lock().unwrap().clone()
    }

    /// Contents of all rows with the given kind, in report order.
    pub fn by_kind(&self, kind: &str) -> Vec<String> {
        self.rows()
            .into_iter()
            .filter(|(_, _, row_kind, _)| row_kind == kind)
            .map(|(_, _, _, content)| content)
            .collect()
    }
}

#[allow(dead_code)]
impl StageLogSink for RecordingSink {
    fn log(&self, stage_id: &str, stage_name: &str, kind: &str, content: &str) {
        self.rows.lock().unwrap().push((
            stage_id.to_string(),
            stage_name.to_string(),
            kind.to_string(),
            content.to_string(),
        ));
    }
}
