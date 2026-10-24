mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use adk_model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use serde_json::json;
use tj_runner::adk_runner::ADKRunner;
use tj_runner::types::Runner;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct FakeOpenAIServer {
    port: u16,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FakeOpenAIServer {
    async fn start(responses: Vec<serde_json::Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        if let Ok((mut stream, _)) = accept_result {
                            let responses = responses.clone();
                            tokio::spawn(async move {
                                let mut buf = vec![0u8; 65536];
                                let _ = stream.read(&mut buf).await;

                                let response = responses.lock().unwrap().pop_front();
                                let body = match response {
                                    Some(r) => serde_json::to_string(&r).unwrap(),
                                    None => serde_json::to_string(&default_response()).unwrap(),
                                };

                                let http_response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                    body.len(),
                                    body
                                );
                                let _ = stream.write_all(http_response.as_bytes()).await;
                                let _ = stream.flush().await;
                            });
                        }
                    }
                    _ = &mut shutdown_rx => break,
                }
            }
        });

        Self {
            port,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for FakeOpenAIServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

fn default_response() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-default",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "done"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
    })
}

fn tool_call_response(tool_name: &str, arguments: &str) -> serde_json::Value {
    json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_test_1",
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": arguments
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30}
    })
}

fn text_response(text: &str) -> serde_json::Value {
    json!({
        "id": "chatcmpl-test2",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text
            },
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

#[tokio::test]
async fn test_http_write_file() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let server = FakeOpenAIServer::start(vec![
        tool_call_response(
            "write_file",
            r#"{"path":"hello.txt","content":"Hello World"}"#,
        ),
        text_response(r#"{"status":"ok","message":"File written successfully."}"#),
    ])
    .await;

    let config =
        OpenAICompatibleConfig::new("fake-key", "test-model").with_base_url(server.base_url());
    let client = OpenAICompatible::new(config).unwrap();
    let runner = ADKRunner::new(Arc::new(client));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);

    let written = std::fs::read_to_string(workspace.join("hello.txt")).unwrap();
    assert_eq!(written, "Hello World");
}

#[tokio::test]
async fn test_http_read_file() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::write(workspace.join("data.txt"), "Test content here").unwrap();

    let server = FakeOpenAIServer::start(vec![
        tool_call_response("read_file", r#"{"path":"data.txt"}"#),
        text_response(r#"{"status":"ok","message":"Read complete."}"#),
    ])
    .await;

    let config =
        OpenAICompatibleConfig::new("fake-key", "test-model").with_base_url(server.base_url());
    let client = OpenAICompatible::new(config).unwrap();
    let runner = ADKRunner::new(Arc::new(client));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);
}

#[tokio::test]
async fn test_http_edit_file() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::write(
        workspace.join("code.rs"),
        "fn main() { println!(\"hello\"); }",
    )
    .unwrap();

    let server = FakeOpenAIServer::start(vec![
        tool_call_response(
            "edit_file",
            r#"{"path":"code.rs","old_string":"hello","new_string":"goodbye"}"#,
        ),
        text_response(r#"{"status":"ok","message":"Edit complete."}"#),
    ])
    .await;

    let config =
        OpenAICompatibleConfig::new("fake-key", "test-model").with_base_url(server.base_url());
    let client = OpenAICompatible::new(config).unwrap();
    let runner = ADKRunner::new(Arc::new(client));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);

    let edited = std::fs::read_to_string(workspace.join("code.rs")).unwrap();
    assert!(edited.contains("goodbye"));
    assert!(!edited.contains("hello"));
}

#[tokio::test]
async fn test_http_multi_tool() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let server = FakeOpenAIServer::start(vec![
        tool_call_response(
            "write_file",
            r#"{"path":"output.txt","content":"Multi-tool test content"}"#,
        ),
        tool_call_response("read_file", r#"{"path":"output.txt"}"#),
        text_response(r#"{"status":"ok","message":"Verified file content."}"#),
    ])
    .await;

    let config =
        OpenAICompatibleConfig::new("fake-key", "test-model").with_base_url(server.base_url());
    let client = OpenAICompatible::new(config).unwrap();
    let runner = ADKRunner::new(Arc::new(client));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);

    let written = std::fs::read_to_string(workspace.join("output.txt")).unwrap();
    assert_eq!(written, "Multi-tool test content");
}

#[tokio::test]
async fn test_http_bash_command() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let server = FakeOpenAIServer::start(vec![
        tool_call_response("bash", r#"{"command":"echo hello from bash"}"#),
        text_response(r#"{"status":"ok","message":"Command executed."}"#),
    ])
    .await;

    let config =
        OpenAICompatibleConfig::new("fake-key", "test-model").with_base_url(server.base_url());
    let client = OpenAICompatible::new(config).unwrap();
    let runner = ADKRunner::new(Arc::new(client));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    common::assert_success(result);
}

// === SANDBOX SECURITY TESTS ===

#[tokio::test]
async fn test_http_write_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let server = FakeOpenAIServer::start(vec![
        tool_call_response(
            "write_file",
            r#"{"path":"../../escape_test.txt","content":"should not be written"}"#,
        ),
        text_response(r#"{"status":"ok","message":"Understood."}"#),
    ])
    .await;

    let config =
        OpenAICompatibleConfig::new("fake-key", "test-model").with_base_url(server.base_url());
    let client = OpenAICompatible::new(config).unwrap();
    let runner = ADKRunner::new(Arc::new(client));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
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
async fn test_http_write_rejects_absolute_path() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let server = FakeOpenAIServer::start(vec![
        tool_call_response(
            "write_file",
            r#"{"path":"/tmp/turbojet_escape_test.txt","content":"should not be written"}"#,
        ),
        text_response(r#"{"status":"ok","message":"Understood."}"#),
    ])
    .await;

    let config =
        OpenAICompatibleConfig::new("fake-key", "test-model").with_base_url(server.base_url());
    let client = OpenAICompatible::new(config).unwrap();
    let runner = ADKRunner::new(Arc::new(client));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    assert!(result.output.is_some());

    assert!(
        !std::path::Path::new("/tmp/turbojet_escape_test.txt").exists(),
        "File was written to absolute path outside workspace!"
    );
}

#[tokio::test]
async fn test_http_read_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let server = FakeOpenAIServer::start(vec![
        tool_call_response("read_file", r#"{"path":"../../../etc/passwd"}"#),
        text_response(r#"{"status":"ok","message":"Understood."}"#),
    ])
    .await;

    let config =
        OpenAICompatibleConfig::new("fake-key", "test-model").with_base_url(server.base_url());
    let client = OpenAICompatible::new(config).unwrap();
    let runner = ADKRunner::new(Arc::new(client));
    let task = common::make_task_definition(workspace, false);
    let _result = runner.execute(task).await.unwrap();
    // Tool loop completes but read should have returned an error
}

#[tokio::test]
async fn test_http_edit_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // Create a file OUTSIDE the workspace
    let outside_dir = workspace.parent().unwrap();
    let outside_file = outside_dir.join("outside_edit_target.txt");
    std::fs::write(&outside_file, "sensitive content").unwrap();

    let server = FakeOpenAIServer::start(vec![
        tool_call_response(
            "edit_file",
            r#"{"path":"../outside_edit_target.txt","old_string":"sensitive","new_string":"modified"}"#,
        ),
        text_response(r#"{"status":"ok","message":"Understood."}"#),
    ])
    .await;

    let config =
        OpenAICompatibleConfig::new("fake-key", "test-model").with_base_url(server.base_url());
    let client = OpenAICompatible::new(config).unwrap();
    let runner = ADKRunner::new(Arc::new(client));
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
async fn test_http_write_rejects_symlink_escape() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    // Create a directory outside workspace for the symlink target
    let outside_dir = workspace.parent().unwrap();
    let escape_target = outside_dir.join("symlink_escape_target");
    std::fs::create_dir_all(&escape_target).unwrap();

    // Create symlink inside workspace pointing outside
    std::os::unix::fs::symlink(&escape_target, workspace.join("escape_link")).unwrap();

    let server = FakeOpenAIServer::start(vec![
        tool_call_response(
            "write_file",
            r#"{"path":"escape_link/escaped.txt","content":"should not be written"}"#,
        ),
        text_response(r#"{"status":"ok","message":"Understood."}"#),
    ])
    .await;

    let config =
        OpenAICompatibleConfig::new("fake-key", "test-model").with_base_url(server.base_url());
    let client = OpenAICompatible::new(config).unwrap();
    let runner = ADKRunner::new(Arc::new(client));
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
async fn test_http_write_rejects_deep_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let server = FakeOpenAIServer::start(vec![
        tool_call_response(
            "write_file",
            r#"{"path":"src/../../../tmp/turbojet_deep_escape.txt","content":"should not be written"}"#,
        ),
        text_response(r#"{"status":"ok","message":"Understood."}"#),
    ])
    .await;

    let config =
        OpenAICompatibleConfig::new("fake-key", "test-model").with_base_url(server.base_url());
    let client = OpenAICompatible::new(config).unwrap();
    let runner = ADKRunner::new(Arc::new(client));
    let task = common::make_task_definition(workspace, false);
    let result = runner.execute(task).await.unwrap();
    assert!(result.output.is_some());

    assert!(
        !std::path::Path::new("/tmp/turbojet_deep_escape.txt").exists(),
        "File was written via deep path traversal!"
    );
}
