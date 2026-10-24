use std::future::Future;
use std::pin::Pin;

use crate::binary::is_binary;
use crate::sandbox::PathValidator;
use crate::tool::{Tool, ToolContext};

const DEFAULT_READ_LIMIT: usize = 2000;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
const MAX_OUTPUT_BYTES: usize = 51_200; // 50KB
const SAMPLE_BYTES: usize = 4096;

pub struct ReadTool;

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self, ctx: &ToolContext) -> String {
        format!(
            "Read the contents of a file or directory listing. All paths must be relative to the workspace root: {}. Only files within the workspace can be accessed.",
            ctx.workspace.display()
        )
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file or directory, relative to the workspace root"
                },
                "offset": {
                    "type": "integer",
                    "description": format!("Line number to start reading from (1-indexed, default 1)")
                },
                "limit": {
                    "type": "integer",
                    "description": format!("Maximum number of lines to read (default {})", DEFAULT_READ_LIMIT)
                }
            },
            "required": ["path"]
        })
    }

    fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Pin<Box<dyn Future<Output = serde_json::Value> + Send + '_>> {
        let path_str = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                return Box::pin(async move {
                    serde_json::json!({
                        "success": false,
                        "error": "Missing required parameter: path"
                    })
                });
            }
        };
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_READ_LIMIT as u64) as usize;
        let workspace = ctx.workspace.clone();

        Box::pin(async move {
            tracing::debug!(
                path = %path_str,
                offset = offset,
                limit = limit,
                workspace = %workspace.display(),
                "ReadTool executing"
            );

            let validator = match PathValidator::new(workspace.clone()) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(error = %e, "PathValidator creation failed");
                    return serde_json::json!({
                        "success": false,
                        "error": format!("Invalid workspace: {}", e)
                    });
                }
            };

            let validated_path = match validator.validate(&path_str) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(error = %e, "Path validation failed");
                    return serde_json::json!({
                        "success": false,
                        "error": format!("Path validation failed: {}", e)
                    });
                }
            };

            tracing::debug!(
                validated = %validated_path.display(),
                "Path validated successfully"
            );

            if validated_path.is_dir() {
                read_directory(&validated_path, offset, limit)
            } else if validated_path.exists() {
                read_file(&validated_path, offset, limit)
            } else {
                file_not_found(&validated_path, &path_str, &validator)
            }
        })
    }
}

fn read_directory(path: &std::path::Path, offset: usize, limit: usize) -> serde_json::Value {
    tracing::debug!(path = %path.display(), "Reading directory listing");

    let mut entries: Vec<String> = match std::fs::read_dir(path) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() {
                    format!("{}/", name)
                } else {
                    name
                }
            })
            .collect(),
        Err(e) => {
            tracing::debug!(error = %e, "Failed to read directory");
            return serde_json::json!({
                "success": false,
                "error": format!("Failed to read directory: {}", e)
            });
        }
    };

    entries.sort();
    let total = entries.len();

    let start = offset.saturating_sub(1);
    let end = (start + limit).min(total);
    let selected: Vec<&String> = entries.iter().skip(start).take(limit).collect();

    let mut output = String::new();
    output.push_str(&format!("<path>{}</path>\n", path.display()));
    output.push_str("<type>directory</type>\n");
    output.push_str("<entries>\n");
    for entry in &selected {
        output.push_str(entry);
        output.push('\n');
    }

    if end < total {
        let next = end + 1;
        output.push_str(&format!(
            "(Showing entries {}-{} of {}. Use offset={} to continue.)\n",
            start + 1,
            end,
            total,
            next
        ));
    } else {
        output.push_str(&format!("({} entries)\n", total));
    }
    output.push_str("</entries>");

    serde_json::json!({
        "success": true,
        "content": output
    })
}

fn read_file(path: &std::path::Path, offset: usize, limit: usize) -> serde_json::Value {
    tracing::debug!(path = %path.display(), "Reading file");

    // Read sample for binary detection
    let sample = match std::fs::File::open(path) {
        Ok(mut f) => {
            let mut buf = vec![0u8; SAMPLE_BYTES];
            use std::io::Read;
            match f.read(&mut buf) {
                Ok(n) => buf.truncate(n),
                Err(e) => {
                    tracing::debug!(error = %e, "Failed to read file sample");
                    return serde_json::json!({
                        "success": false,
                        "error": format!("Failed to read file: {}", e)
                    });
                }
            }
            buf
        }
        Err(e) => {
            tracing::debug!(error = %e, "Failed to open file");
            return serde_json::json!({
                "success": false,
                "error": format!("Failed to open file: {}", e)
            });
        }
    };

    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    if is_binary(&ext, &sample) {
        tracing::debug!(ext = %ext, "File detected as binary, rejecting");
        return serde_json::json!({
            "success": false,
            "error": "Cannot read binary file"
        });
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(error = %e, "Failed to read file as text");
            return serde_json::json!({
                "success": false,
                "error": format!("Failed to read file as text: {}", e)
            });
        }
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let start = offset.saturating_sub(1);
    let selected: Vec<&str> = lines.iter().skip(start).take(limit).copied().collect();
    let selected_count = selected.len();
    let _end = start + selected_count;

    let mut numbered_lines = Vec::new();
    let mut byte_count: usize = 0;

    for (i, line) in selected.iter().enumerate() {
        let line_num = start + i + 1;
        let truncated = if line.len() > MAX_LINE_LENGTH {
            format!("{}{}", &line[..MAX_LINE_LENGTH], MAX_LINE_SUFFIX)
        } else {
            line.to_string()
        };
        let formatted = format!("{}: {}", line_num, truncated);

        byte_count += formatted.len() + 1; // +1 for newline
        if byte_count > MAX_OUTPUT_BYTES {
            // Cap hit — report what we have so far
            tracing::debug!(
                byte_count = byte_count,
                max = MAX_OUTPUT_BYTES,
                "Output capped at byte limit"
            );
            break;
        }

        numbered_lines.push(formatted);
    }

    let capped = numbered_lines.len() < selected_count;
    let shown_end = start + numbered_lines.len();

    let mut output = String::new();
    output.push_str(&format!("<path>{}</path>\n", path.display()));
    output.push_str("<type>file</type>\n");
    output.push_str("<content>\n");
    for line in &numbered_lines {
        output.push_str(line);
        output.push('\n');
    }
    output.push('\n');

    if capped {
        output.push_str(&format!(
            "(Output capped at 50 KB. Showing lines {}-{}. Use offset={} to continue.)\n",
            start + 1,
            shown_end,
            shown_end + 1
        ));
    } else if shown_end < total_lines {
        output.push_str(&format!(
            "(Showing lines {}-{} of {}. Use offset={} to continue.)\n",
            start + 1,
            shown_end,
            total_lines,
            shown_end + 1
        ));
    } else {
        output.push_str(&format!("(End of file - total {} lines)\n", total_lines));
    }

    output.push_str("</content>");

    serde_json::json!({
        "success": true,
        "content": output
    })
}

fn file_not_found(
    validated_path: &std::path::Path,
    requested: &str,
    validator: &PathValidator,
) -> serde_json::Value {
    tracing::debug!(
        path = %validated_path.display(),
        requested = %requested,
        "File not found, generating suggestions"
    );

    let basename = validated_path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let parent = validated_path
        .parent()
        .unwrap_or_else(|| validator.workspace());

    let mut suggestions: Vec<String> = Vec::new();

    if let Ok(rd) = std::fs::read_dir(parent) {
        for entry in rd.filter_map(|e| e.ok()) {
            if suggestions.len() >= 3 {
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let name_lower = name.to_lowercase();

            // Case-insensitive: parent basename contains requested basename, or vice versa
            if name_lower.contains(&basename) || basename.contains(&name_lower) {
                suggestions.push(name);
            }
        }
    }

    let mut message = format!("File not found: {}", requested);
    if !suggestions.is_empty() {
        message.push_str("\n\nDid you mean one of these?\n");
        for s in &suggestions {
            message.push_str(s);
            message.push('\n');
        }
    }

    serde_json::json!({
        "success": false,
        "error": message
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_ctx(dir: &TempDir) -> ToolContext {
        ToolContext {
            workspace: dir.path().to_path_buf(),
            output_dir: dir.path().to_path_buf(),
        }
    }

    #[tokio::test]
    async fn test_read_entire_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "line one\nline two\n").unwrap();
        let ctx = make_ctx(&dir);
        let tool = ReadTool::new();
        let result = tool
            .execute(serde_json::json!({"path": "test.txt"}), &ctx)
            .await;
        assert!(result["success"].as_bool().unwrap());
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("line one"));
        assert!(content.contains("line two"));
        assert!(content.contains("(End of file - total 2 lines)"));
    }

    #[tokio::test]
    async fn test_read_with_offset_limit() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=10).map(|i| format!("line {}\n", i)).collect();
        std::fs::write(dir.path().join("num.txt"), content).unwrap();
        let ctx = make_ctx(&dir);
        let tool = ReadTool::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "num.txt", "offset": 3, "limit": 3}),
                &ctx,
            )
            .await;
        assert!(result["success"].as_bool().unwrap());
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("3: line 3"));
        assert!(content.contains("4: line 4"));
        assert!(content.contains("5: line 5"));
        assert!(!content.contains("2: line 2"));
        assert!(!content.contains("6: line 6"));
    }

    #[tokio::test]
    async fn test_read_binary_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin"), b"hello\x00world").unwrap();
        let ctx = make_ctx(&dir);
        let tool = ReadTool::new();
        let result = tool
            .execute(serde_json::json!({"path": "data.bin"}), &ctx)
            .await;
        assert!(!result["success"].as_bool().unwrap());
        assert!(result["error"].as_str().unwrap().contains("binary"));
    }

    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let dir = tempfile::tempdir().unwrap();
        // Create a similar file for suggestion matching
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        let ctx = make_ctx(&dir);
        let tool = ReadTool::new();
        let result = tool
            .execute(serde_json::json!({"path": "Readme.md"}), &ctx)
            .await;
        assert!(!result["success"].as_bool().unwrap());
        let error = result["error"].as_str().unwrap();
        assert!(error.contains("File not found"));
        assert!(error.contains("Did you mean"));
        assert!(error.contains("README.md"));
    }

    #[tokio::test]
    async fn test_read_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        let ctx = make_ctx(&dir);
        let tool = ReadTool::new();
        let result = tool.execute(serde_json::json!({"path": "."}), &ctx).await;
        assert!(result["success"].as_bool().unwrap());
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("<type>directory</type>"));
        assert!(content.contains("src/"));
        assert!(content.contains("Cargo.toml"));
        assert!(content.contains("README.md"));
        // Verify sorted: Cargo.toml, README.md, src/
        let cargo_pos = content.find("Cargo.toml").unwrap();
        let readme_pos = content.find("README.md").unwrap();
        let src_pos = content.find("src/").unwrap();
        assert!(cargo_pos < readme_pos);
        assert!(readme_pos < src_pos);
    }

    #[tokio::test]
    async fn test_line_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let long_line: String = "x".repeat(3000);
        std::fs::write(dir.path().join("long.txt"), format!("{}\n", long_line)).unwrap();
        let ctx = make_ctx(&dir);
        let tool = ReadTool::new();
        let result = tool
            .execute(serde_json::json!({"path": "long.txt"}), &ctx)
            .await;
        assert!(result["success"].as_bool().unwrap());
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("... (line truncated to 2000 chars)"));
    }

    #[tokio::test]
    async fn test_output_truncation() {
        let dir = tempfile::tempdir().unwrap();
        // Create a file with many lines to exceed 50KB
        let line = "A".repeat(200);
        let content: String = (0..500).map(|_| format!("{}\n", line)).collect();
        std::fs::write(dir.path().join("big.txt"), content).unwrap();
        let ctx = make_ctx(&dir);
        let tool = ReadTool::new();
        let result = tool
            .execute(serde_json::json!({"path": "big.txt"}), &ctx)
            .await;
        assert!(result["success"].as_bool().unwrap());
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("Output capped at 50 KB"));
    }
}
