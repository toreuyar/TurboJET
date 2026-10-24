use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

/// Context provided to every tool execution.
pub struct ToolContext {
    pub workspace: PathBuf,
    pub output_dir: PathBuf,
}

/// Configuration for which tools are enabled. Mirrors tj-config::ToolsConfig
/// but without the tj-config dependency.
#[derive(Clone)]
pub struct ToolsetConfig {
    pub read: bool,
    pub write: bool,
    pub edit: bool,
    pub bash: bool,
}

/// Tool trait -- each tool implements this.
/// Returns serde_json::Value directly because each tool has a unique response shape.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self, ctx: &ToolContext) -> String;
    fn schema(&self) -> serde_json::Value;
    fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Pin<Box<dyn Future<Output = serde_json::Value> + Send + '_>>;
}

/// Registry of active tools, built from ToolsetConfig.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new(config: &ToolsetConfig) -> Self {
        let mut tools: Vec<Box<dyn Tool>> = Vec::new();
        if config.read {
            tools.push(Box::new(crate::read::ReadTool::new()));
        }
        if config.write {
            tools.push(Box::new(crate::write::WriteTool::new()));
        }
        if config.edit {
            tools.push(Box::new(crate::edit::EditTool::new()));
        }
        if config.bash {
            tools.push(Box::new(crate::shell::ShellTool::new()));
        }
        Self { tools }
    }

    pub fn declarations(&self, ctx: &ToolContext) -> HashMap<String, serde_json::Value> {
        let mut declarations = HashMap::new();
        for tool in &self.tools {
            let schema = tool.schema();
            let desc = tool.description(ctx);
            let declaration = serde_json::json!({
                "name": tool.name(),
                "description": desc,
                "parameters": schema
            });
            declarations.insert(tool.name().to_string(), declaration);
        }
        declarations
    }

    pub async fn dispatch(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> serde_json::Value {
        for tool in &self.tools {
            if tool.name() == name {
                return tool.execute(args, ctx).await;
            }
        }
        serde_json::json!({
            "success": false,
            "error": format!("Unknown tool: {}", name)
        })
    }

    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }
}
