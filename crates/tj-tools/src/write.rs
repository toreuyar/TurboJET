use std::future::Future;
use std::pin::Pin;

use crate::sandbox::PathValidator;
use crate::tool::{Tool, ToolContext};

pub struct WriteTool;

impl Default for WriteTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self, ctx: &ToolContext) -> String {
        format!(
            "Write (create or overwrite) a file. Creates parent directories if needed. All paths must be relative to the workspace root: {}. Only files within the workspace can be written.",
            ctx.workspace.display()
        )
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the workspace root"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Pin<Box<dyn Future<Output = serde_json::Value> + Send + '_>> {
        let workspace = ctx.workspace.clone();

        Box::pin(async move {
            let path_str = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) if !p.is_empty() => p,
                _ => {
                    return serde_json::json!({
                        "success": false,
                        "error": "Missing or empty 'path' argument"
                    });
                }
            };

            let content = match args.get("content").and_then(|v| v.as_str()) {
                Some(c) => c,
                _ => {
                    return serde_json::json!({
                        "success": false,
                        "error": "Missing 'content' argument"
                    });
                }
            };

            tracing::debug!(
                path = %path_str,
                content_len = content.len(),
                workspace = %workspace.display(),
                "WriteTool: starting file write"
            );

            let validator = match PathValidator::new(workspace.clone()) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(error = %e, "WriteTool: PathValidator creation failed");
                    return serde_json::json!({
                        "success": false,
                        "error": format!("Invalid workspace: {}", e)
                    });
                }
            };

            let joined = validator.workspace().join(path_str);
            if let Some(parent) = joined.parent()
                && !parent.exists()
            {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::debug!(error = %e, "WriteTool: failed to create parent directories");
                    return serde_json::json!({
                        "success": false,
                        "error": format!("Failed to create parent directories: {}", e)
                    });
                }
                tracing::debug!(
                    parent = %parent.display(),
                    "WriteTool: parent directories created"
                );
            }

            let full_path = match validator.validate(path_str) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(error = %e, "WriteTool: path validation failed");
                    return serde_json::json!({
                        "success": false,
                        "error": format!("Path validation failed: {}", e)
                    });
                }
            };

            tracing::debug!(
                full_path = %full_path.display(),
                "WriteTool: path validated"
            );

            if full_path.is_dir() {
                tracing::debug!(
                    path = %full_path.display(),
                    "WriteTool: path is an existing directory"
                );
                return serde_json::json!({
                    "success": false,
                    "error": format!("Path '{}' is a directory, not a file", path_str)
                });
            }

            let final_content = if full_path.exists() {
                match std::fs::read_to_string(&full_path) {
                    Ok(existing) => {
                        let ending = detect_line_ending(&existing);
                        tracing::debug!(
                            line_ending = if ending == "\r\n" { "CRLF" } else { "LF" },
                            "WriteTool: detected line ending of existing file"
                        );
                        convert_line_endings(content, ending)
                    }
                    Err(e) => {
                        tracing::debug!(
                            error = %e,
                            "WriteTool: could not read existing file, using content as-is"
                        );
                        content.to_string()
                    }
                }
            } else {
                content.to_string()
            };

            let bytes = final_content.len();
            if let Err(e) = std::fs::write(&full_path, &final_content) {
                tracing::debug!(error = %e, "WriteTool: failed to write file");
                return serde_json::json!({
                    "success": false,
                    "error": format!("Failed to write file: {}", e)
                });
            }

            tracing::debug!(
                bytes_written = bytes,
                path = %full_path.display(),
                "WriteTool: file written successfully"
            );

            serde_json::json!({
                "success": true,
                "bytes_written": bytes
            })
        })
    }
}

fn detect_line_ending(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

fn convert_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        // First normalize to LF, then convert all to CRLF
        text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        // Normalize to LF
        text.replace("\r\n", "\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_write_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = WriteTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "hello.txt", "content": "hello world"}),
                &ctx,
            )
            .await;
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(result["bytes_written"], 11);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
            "hello world"
        );
    }

    #[tokio::test]
    async fn test_write_nested_directory() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = WriteTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "src/lib/hello.rs", "content": "fn main() {}"}),
                &ctx,
            )
            .await;
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/lib/hello.rs")).unwrap(),
            "fn main() {}"
        );
        assert!(dir.path().join("src/lib").is_dir());
    }

    #[tokio::test]
    async fn test_overwrite_existing() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("existing.txt");
        std::fs::write(&file_path, "old content").unwrap();

        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = WriteTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "existing.txt", "content": "new content"}),
                &ctx,
            )
            .await;
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "new content");
    }

    #[tokio::test]
    async fn test_write_to_directory_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("mydir")).unwrap();

        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = WriteTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "mydir", "content": "data"}),
                &ctx,
            )
            .await;
        assert!(!result["success"].as_bool().unwrap());
        assert!(result["error"].as_str().unwrap().contains("is a directory"));
    }

    #[tokio::test]
    async fn test_line_ending_preservation() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("crlf.txt");
        std::fs::write(&file_path, "line1\r\nline2\r\n").unwrap();

        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = WriteTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "crlf.txt", "content": "alpha\nbeta\n"}),
                &ctx,
            )
            .await;
        assert!(result["success"].as_bool().unwrap());
        let written = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(written, "alpha\r\nbeta\r\n");
    }

    #[tokio::test]
    async fn test_missing_path_argument() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = WriteTool::new();
        let result = tool
            .execute(serde_json::json!({"content": "data"}), &ctx)
            .await;
        assert!(!result["success"].as_bool().unwrap());
        assert!(result["error"].as_str().unwrap().contains("path"));
    }

    #[tokio::test]
    async fn test_missing_content_argument() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        };
        let tool = WriteTool::new();
        let result = tool
            .execute(serde_json::json!({"path": "test.txt"}), &ctx)
            .await;
        assert!(!result["success"].as_bool().unwrap());
        assert!(result["error"].as_str().unwrap().contains("content"));
    }
}
