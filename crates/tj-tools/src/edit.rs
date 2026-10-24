use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use crate::fuzzy::{self, EditError};
use crate::sandbox::PathValidator;
use crate::tool::{Tool, ToolContext};

pub struct EditTool;

impl Default for EditTool {
    fn default() -> Self {
        Self::new()
    }
}

impl EditTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self, ctx: &ToolContext) -> String {
        format!(
            "Edit a file by replacing matched text with new content. Supports fuzzy matching for whitespace and indentation differences. Use this for targeted edits instead of rewriting entire files. All paths must be relative to the workspace root: {}. Only files within the workspace can be edited.",
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
                "old_string": {
                    "type": "string",
                    "description": "The text to find in the file"
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace it with (must be different from old_string)"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default false)"
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Pin<Box<dyn Future<Output = serde_json::Value> + Send + '_>> {
        let workspace = ctx.workspace.clone();
        Box::pin(async move { execute_edit(args, &workspace).await })
    }
}

async fn execute_edit(args: serde_json::Value, workspace: &Path) -> serde_json::Value {
    let path_val = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return serde_json::json!({
                "success": false,
                "error": "Missing required parameter: path"
            });
        }
    };

    let old_string = match args.get("old_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return serde_json::json!({
                "success": false,
                "error": "Missing required parameter: old_string"
            });
        }
    };

    let new_string = match args.get("new_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return serde_json::json!({
                "success": false,
                "error": "Missing required parameter: new_string"
            });
        }
    };

    let replace_all = args
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    tracing::debug!(
        path = %path_val,
        old_len = old_string.len(),
        new_len = new_string.len(),
        replace_all = replace_all,
        "EditTool: starting edit"
    );

    if old_string == new_string {
        tracing::debug!("EditTool: old_string equals new_string, rejecting as no-op");
        return serde_json::json!({
            "success": false,
            "error": "old_string and new_string are identical. No change would be made."
        });
    }

    let validator = match PathValidator::new(workspace.to_path_buf()) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "EditTool: path validator creation failed");
            return serde_json::json!({
                "success": false,
                "error": format!("Failed to create path validator: {}", e)
            });
        }
    };

    let validated_path = match validator.validate(path_val) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "EditTool: path validation failed");
            return serde_json::json!({
                "success": false,
                "error": format!("Path validation failed: {}", e)
            });
        }
    };

    let content = match std::fs::read_to_string(&validated_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(
                path = %validated_path.display(),
                error = %e,
                "EditTool: file not found or unreadable"
            );
            return serde_json::json!({
                "success": false,
                "error": format!("File not found or unreadable: {}", path_val)
            });
        }
    };

    let line_ending = fuzzy::detect_line_ending(&content);
    let normalized_old = fuzzy::convert_line_endings(old_string, line_ending);
    let normalized_new = fuzzy::convert_line_endings(new_string, line_ending);

    tracing::debug!(
        line_ending = if line_ending == "\r\n" { "CRLF" } else { "LF" },
        "EditTool: detected file line ending"
    );

    match fuzzy::replace(&content, &normalized_old, &normalized_new, replace_all) {
        Ok(new_content) => match std::fs::write(&validated_path, &new_content) {
            Ok(()) => {
                tracing::debug!(
                    path = %validated_path.display(),
                    new_len = new_content.len(),
                    "EditTool: file written successfully"
                );
                serde_json::json!({ "success": true })
            }
            Err(e) => {
                tracing::debug!(
                    path = %validated_path.display(),
                    error = %e,
                    "EditTool: failed to write file"
                );
                serde_json::json!({
                    "success": false,
                    "error": format!("Failed to write file: {}", e)
                })
            }
        },
        Err(EditError::NotFound) => {
            tracing::debug!("EditTool: old_string not found in file");
            serde_json::json!({
                "success": false,
                "error": "Could not find old_string in file. It must match exactly, including whitespace, indentation, and line endings."
            })
        }
        Err(EditError::Ambiguous { count }) => {
            tracing::debug!(count = count, "EditTool: ambiguous match");
            serde_json::json!({
                "success": false,
                "error": format!("Found {} matches for old_string. Provide more surrounding context to make the match unique, or set replace_all to true.", count)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Tool, ToolContext};
    use tempfile::TempDir;

    fn make_ctx(dir: &TempDir) -> ToolContext {
        ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        }
    }

    #[tokio::test]
    async fn test_exact_match_edit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello world\n").unwrap();
        let ctx = make_ctx(&dir);
        let tool = EditTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "test.txt", "old_string": "hello", "new_string": "goodbye"}),
                &ctx,
            )
            .await;
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("test.txt")).unwrap(),
            "goodbye world\n"
        );
    }

    #[tokio::test]
    async fn test_fuzzy_match_edit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "  hello world  \n").unwrap();
        let ctx = make_ctx(&dir);
        let tool = EditTool::new();
        // old_string lacks surrounding spaces but LineTrimmed still matches
        let result = tool
            .execute(
                serde_json::json!({"path": "test.txt", "old_string": "hello world", "new_string": "replaced"}),
                &ctx,
            )
            .await;
        assert!(result["success"].as_bool().unwrap());
        let written = std::fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert!(written.contains("replaced"));
    }

    #[tokio::test]
    async fn test_edit_with_line_ending_preservation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), b"hello\r\nworld\r\n").unwrap();
        let ctx = make_ctx(&dir);
        let tool = EditTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "test.txt", "old_string": "hello\nworld", "new_string": "foo\nbar"}),
                &ctx,
            )
            .await;
        assert!(result["success"].as_bool().unwrap());
        let bytes = std::fs::read(dir.path().join("test.txt")).unwrap();
        assert_eq!(bytes, b"foo\r\nbar\r\n");
    }

    #[tokio::test]
    async fn test_edit_nonexistent_file() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(&dir);
        let tool = EditTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "missing.txt", "old_string": "x", "new_string": "y"}),
                &ctx,
            )
            .await;
        assert!(!result["success"].as_bool().unwrap());
        assert!(result["error"].as_str().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn test_edit_ambiguous_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "foo\nfoo\n").unwrap();
        let ctx = make_ctx(&dir);
        let tool = EditTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "test.txt", "old_string": "foo", "new_string": "bar"}),
                &ctx,
            )
            .await;
        assert!(!result["success"].as_bool().unwrap());
        let err = result["error"].as_str().unwrap();
        assert!(
            err.contains("2 matches"),
            "expected ambiguous error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_edit_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "foo bar foo baz foo\n").unwrap();
        let ctx = make_ctx(&dir);
        let tool = EditTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "test.txt", "old_string": "foo", "new_string": "qux", "replace_all": true}),
                &ctx,
            )
            .await;
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("test.txt")).unwrap(),
            "qux bar qux baz qux\n"
        );
    }

    #[tokio::test]
    async fn test_edit_old_equals_new() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello\n").unwrap();
        let ctx = make_ctx(&dir);
        let tool = EditTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "test.txt", "old_string": "hello", "new_string": "hello"}),
                &ctx,
            )
            .await;
        assert!(!result["success"].as_bool().unwrap());
        assert!(result["error"].as_str().unwrap().contains("identical"));
    }

    #[tokio::test]
    async fn test_edit_not_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello world\n").unwrap();
        let ctx = make_ctx(&dir);
        let tool = EditTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "test.txt", "old_string": "does not exist", "new_string": "something"}),
                &ctx,
            )
            .await;
        assert!(!result["success"].as_bool().unwrap());
        assert!(result["error"].as_str().unwrap().contains("Could not find"));
    }
}
