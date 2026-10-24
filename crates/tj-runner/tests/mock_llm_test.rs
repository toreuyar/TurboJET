mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use adk_core::{Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part};
use async_trait::async_trait;
use serde_json::json;
use tj_runner::adk_runner::ADKRunner;
use tj_runner::types::Runner;

struct SequentialMockLlm {
    name: String,
    responses: Arc<Mutex<VecDeque<LlmResponse>>>,
}

impl SequentialMockLlm {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            responses: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    fn with_response(self, response: LlmResponse) -> Self {
        self.responses.lock().unwrap().push_back(response);
        self
    }
}

#[async_trait]
impl Llm for SequentialMockLlm {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("SequentialMockLlm ran out of responses");
        let stream = async_stream::stream! {
            yield Ok(response);
        };
        Ok(Box::pin(stream))
    }
}

fn tool_call_response(tool_name: &str, args: serde_json::Value) -> LlmResponse {
    LlmResponse::new(Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: tool_name.to_string(),
            args,
            id: Some("call_test_123".to_string()),
            thought_signature: None,
        }],
    })
}

fn text_response(text: &str) -> LlmResponse {
    LlmResponse::new(Content::new("model").with_text(text))
}

#[tokio::test]
async fn test_write_file_via_tool_loop() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = SequentialMockLlm::new("test-model")
        .with_response(tool_call_response(
            "write_file",
            json!({
                "path": "hello.txt",
                "content": "Hello World"
            }),
        ))
        .with_response(text_response(
            r#"{"status":"ok","message":"File written successfully."}"#,
        ));

    let runner = ADKRunner::new(Arc::new(mock));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);

    let written = std::fs::read_to_string(workspace.join("hello.txt")).unwrap();
    assert_eq!(written, "Hello World");
}

#[tokio::test]
async fn test_read_file_via_tool_loop() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::write(workspace.join("data.txt"), "Test content here").unwrap();

    let mock = SequentialMockLlm::new("test-model")
        .with_response(tool_call_response(
            "read_file",
            json!({
                "path": "data.txt"
            }),
        ))
        .with_response(text_response(
            r#"{"status":"ok","message":"Read complete."}"#,
        ));

    let runner = ADKRunner::new(Arc::new(mock));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);
}

#[tokio::test]
async fn test_edit_file_via_tool_loop() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::write(
        workspace.join("code.rs"),
        "fn main() { println!(\"hello\"); }",
    )
    .unwrap();

    let mock = SequentialMockLlm::new("test-model")
        .with_response(tool_call_response(
            "edit_file",
            json!({
                "path": "code.rs",
                "old_string": "hello",
                "new_string": "goodbye"
            }),
        ))
        .with_response(text_response(
            r#"{"status":"ok","message":"Edit complete."}"#,
        ));

    let runner = ADKRunner::new(Arc::new(mock));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);

    let edited = std::fs::read_to_string(workspace.join("code.rs")).unwrap();
    assert!(edited.contains("goodbye"));
    assert!(!edited.contains("hello"));
}

#[tokio::test]
async fn test_multi_tool_write_then_read() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = SequentialMockLlm::new("test-model")
        .with_response(tool_call_response(
            "write_file",
            json!({
                "path": "output.txt",
                "content": "Multi-tool test content"
            }),
        ))
        .with_response(tool_call_response(
            "read_file",
            json!({
                "path": "output.txt"
            }),
        ))
        .with_response(text_response(
            r#"{"status":"ok","message":"Verified file content."}"#,
        ));

    let runner = ADKRunner::new(Arc::new(mock));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);

    let written = std::fs::read_to_string(workspace.join("output.txt")).unwrap();
    assert_eq!(written, "Multi-tool test content");
}

#[tokio::test]
async fn test_bash_command() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = SequentialMockLlm::new("test-model")
        .with_response(tool_call_response(
            "bash",
            json!({
                "command": "echo hello from bash"
            }),
        ))
        .with_response(text_response(
            r#"{"status":"ok","message":"Command executed."}"#,
        ));

    let runner = ADKRunner::new(Arc::new(mock));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);
}

// === SANDBOX SECURITY TESTS ===

#[tokio::test]
async fn test_write_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = SequentialMockLlm::new("test-model")
        .with_response(tool_call_response(
            "write_file",
            json!({
                "path": "../../escape_test.txt",
                "content": "should not be written"
            }),
        ))
        .with_response(text_response(r#"{"status":"ok","message":"Understood."}"#));

    let runner = ADKRunner::new(Arc::new(mock));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    // Tool loop completes but the tool returns an error; the LLM sees it and responds
    assert!(result.output.is_some());

    let escape_path = workspace
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("escape_test.txt");
    assert!(!escape_path.exists(), "File was written outside workspace!");
}

#[tokio::test]
async fn test_write_rejects_absolute_path() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = SequentialMockLlm::new("test-model")
        .with_response(tool_call_response(
            "write_file",
            json!({
                "path": "/tmp/turbojet_escape_test.txt",
                "content": "should not be written"
            }),
        ))
        .with_response(text_response(r#"{"status":"ok","message":"Understood."}"#));

    let runner = ADKRunner::new(Arc::new(mock));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    assert!(result.output.is_some());

    assert!(
        !std::path::Path::new("/tmp/turbojet_escape_test.txt").exists(),
        "File was written to absolute path outside workspace!"
    );
}

#[tokio::test]
async fn test_read_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = SequentialMockLlm::new("test-model")
        .with_response(tool_call_response(
            "read_file",
            json!({
                "path": "../../../etc/passwd"
            }),
        ))
        .with_response(text_response(r#"{"status":"ok","message":"Understood."}"#));

    let runner = ADKRunner::new(Arc::new(mock));
    let task = common::make_task_definition(workspace, false);
    let _result = runner.execute(task).await.unwrap();
    // Tool loop completes but read should have returned an error
}

#[tokio::test]
async fn test_edit_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // Create a file OUTSIDE the workspace
    let outside_dir = workspace.parent().unwrap();
    let outside_file = outside_dir.join("outside_edit_target.txt");
    std::fs::write(&outside_file, "sensitive content").unwrap();

    let mock = SequentialMockLlm::new("test-model")
        .with_response(tool_call_response(
            "edit_file",
            json!({
                "path": "../outside_edit_target.txt",
                "old_string": "sensitive",
                "new_string": "modified"
            }),
        ))
        .with_response(text_response(r#"{"status":"ok","message":"Understood."}"#));

    let runner = ADKRunner::new(Arc::new(mock));
    let task = common::make_task_definition(workspace, false);
    let _result = runner.execute(task).await.unwrap();

    // The outside file should NOT have been modified
    let content = std::fs::read_to_string(&outside_file).unwrap();
    assert_eq!(
        content, "sensitive content",
        "File outside workspace was modified!"
    );

    // Clean up
    let _ = std::fs::remove_file(&outside_file);
}

#[tokio::test]
#[cfg(unix)]
async fn test_write_rejects_symlink_escape() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // Create a directory outside workspace for the symlink target
    let outside_dir = workspace.parent().unwrap();
    let escape_target = outside_dir.join("symlink_escape_target");
    std::fs::create_dir_all(&escape_target).unwrap();

    // Create symlink inside workspace pointing outside
    std::os::unix::fs::symlink(&escape_target, workspace.join("escape_link")).unwrap();

    let mock = SequentialMockLlm::new("test-model")
        .with_response(tool_call_response(
            "write_file",
            json!({
                "path": "escape_link/escaped.txt",
                "content": "should not be written"
            }),
        ))
        .with_response(text_response(r#"{"status":"ok","message":"Understood."}"#));

    let runner = ADKRunner::new(Arc::new(mock));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    assert!(result.output.is_some());

    // File should NOT exist in the escape target directory
    assert!(
        !escape_target.join("escaped.txt").exists(),
        "File was written through symlink outside workspace!"
    );

    // Clean up
    let _ = std::fs::remove_dir_all(&escape_target);
}

#[tokio::test]
async fn test_write_rejects_deep_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let mock = SequentialMockLlm::new("test-model")
        .with_response(tool_call_response(
            "write_file",
            json!({
                "path": "src/../../../tmp/turbojet_deep_escape.txt",
                "content": "should not be written"
            }),
        ))
        .with_response(text_response(r#"{"status":"ok","message":"Understood."}"#));

    let runner = ADKRunner::new(Arc::new(mock));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    assert!(result.output.is_some());

    assert!(
        !std::path::Path::new("/tmp/turbojet_deep_escape.txt").exists(),
        "File was written via deep path traversal!"
    );
}
