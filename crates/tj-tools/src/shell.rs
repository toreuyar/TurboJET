use std::future::Future;
use std::pin::Pin;

use crate::tool::{Tool, ToolContext};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_OUTPUT_BYTES: usize = 102_400; // 100KB

pub struct ShellTool;

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self, ctx: &ToolContext) -> String {
        format!(
            "Execute a shell command. Commands run in the workspace directory: {}. Returns stdout and stderr.",
            ctx.workspace.display()
        )
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": format!("Timeout in seconds (default {})", DEFAULT_TIMEOUT_SECS)
                }
            },
            "required": ["command"]
        })
    }

    fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Pin<Box<dyn Future<Output = serde_json::Value> + Send + '_>> {
        let workspace = ctx.workspace.clone();
        let output_dir = ctx.output_dir.clone();

        Box::pin(async move {
            let command = match args.get("command").and_then(|v| v.as_str()) {
                Some(c) if !c.is_empty() => c.to_string(),
                _ => {
                    return serde_json::json!({
                        "success": false,
                        "error": "No command specified"
                    });
                }
            };

            let timeout_secs = args
                .get("timeout")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_TIMEOUT_SECS);

            tracing::debug!(
                command = %command,
                timeout_secs = timeout_secs,
                workspace = %workspace.display(),
                "Executing shell command"
            );

            // TJ-034: a model-driven shell never runs unconfined. Preparation
            // happens before spawn; if Landlock/seccomp/limits are unavailable
            // the tool fails closed instead of silently weakening the boundary.
            let policy = crate::isolation::IsolationPolicy::new(&workspace, &output_dir);
            let sandbox = match crate::isolation::LinuxSandbox::prepare(&policy) {
                Ok(sandbox) => sandbox,
                Err(error) => {
                    tracing::error!(workspace = %workspace.display(), %error,
                        "Shell execution refused: isolation boundary unavailable");
                    return serde_json::json!({
                        "success": false,
                        "error": format!("Execution isolation unavailable (TJ-034); refusing to run shell unconfined: {error}")
                    });
                }
            };
            let mut std_command = std::process::Command::new("bash");
            std_command.arg("-c").arg(&command).current_dir(&workspace);
            sandbox.configure_command(&mut std_command);
            let process = tokio::process::Command::from(std_command);
            let result =
                crate::processes::output(process, std::time::Duration::from_secs(timeout_secs))
                    .await;

            match result {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let exit_code = output.status.code().unwrap_or(-1);

                    let stdout = truncate_output(&stdout);
                    let stderr = truncate_output(&stderr);

                    tracing::debug!(
                        exit_code = exit_code,
                        stdout_len = stdout.len(),
                        stderr_len = stderr.len(),
                        "Shell command completed"
                    );

                    serde_json::json!({
                        "success": exit_code == 0,
                        "stdout": stdout,
                        "stderr": stderr,
                        "exit_code": exit_code
                    })
                }
                Err(e) => {
                    tracing::debug!(error = %e, "Shell command failed to execute");
                    serde_json::json!({
                        "success": false,
                        "error": format!("Failed to execute command: {}", e)
                    })
                }
            }
        })
    }
}

fn truncate_output(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output.to_string();
    }
    let truncated = &output[..output.floor_char_boundary(MAX_OUTPUT_BYTES)];
    format!(
        "{}\n... (output truncated at {} KB)",
        truncated,
        MAX_OUTPUT_BYTES / 1024
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_simple_command() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "echo hello"}), &ctx)
            .await;
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "hello");
        assert_eq!(result["exit_code"], 0);
    }

    #[tokio::test]
    async fn test_command_with_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "echo error >&2"}), &ctx)
            .await;
        assert_eq!(result["stderr"].as_str().unwrap().trim(), "error");
    }

    #[tokio::test]
    async fn test_nonzero_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "exit 1"}), &ctx)
            .await;
        assert_eq!(result["exit_code"], 1);
    }

    #[tokio::test]
    async fn test_empty_command_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = ShellTool::new();
        let result = tool.execute(serde_json::json!({"command": ""}), &ctx).await;
        assert_eq!(result["success"], false);
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("No command specified")
        );
    }

    #[tokio::test]
    async fn test_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = ShellTool::new();
        let result = tool
            .execute(
                serde_json::json!({"command": "sleep 10", "timeout": 1}),
                &ctx,
            )
            .await;
        assert_eq!(result["success"], false);
        assert!(result["error"].as_str().unwrap().contains("timed out"));
    }
}
