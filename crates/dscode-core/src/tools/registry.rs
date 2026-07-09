//! ToolRegistry — the agent's collection of available tools.
//!
//! Provides registration, lookup, and schema generation for the LLM API
//! (OpenAI-compatible tools array). Supports runtime registration (MCP).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::providers::trait_def::ToolDef;
use crate::tools::trait_def::{Tool, ToolError, ToolResult};

/// A thread-safe registry of all tools available to the agent.
///
/// Tools are stored behind `Arc` so they can be shared across the agent loop.
/// Interior mutability allows MCP tools to be registered after startup.
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<Box<dyn Tool>>>>,
}

impl ToolRegistry {
    /// Create a new, empty tool registry.
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
        }
    }

    /// Register a tool under its `name()`.
    ///
    /// Panics if a tool with the same name is already registered.
    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        let name = tool.name().to_string();
        let mut map = self.tools.write().expect("tool registry lock");
        let prev = map.insert(name.clone(), Arc::new(Box::new(tool)));
        assert!(
            prev.is_none(),
            "Tool '{}' is already registered — each tool must have a unique name",
            name
        );
    }

    /// Register or replace a tool (used for MCP hot-reload).
    pub fn register_or_replace<T: Tool + 'static>(&self, tool: T) {
        let name = tool.name().to_string();
        let mut map = self.tools.write().expect("tool registry lock");
        map.insert(name, Arc::new(Box::new(tool)));
    }

    /// Remove tools matching a predicate (e.g. all `mcp_*`).
    pub fn unregister_where(&self, pred: impl Fn(&str) -> bool) {
        let mut map = self.tools.write().expect("tool registry lock");
        map.retain(|name, _| !pred(name));
    }

    /// Look up a tool by name. Returns `None` if not found.
    pub fn get(&self, name: &str) -> Option<Arc<Box<dyn Tool>>> {
        self.tools.read().expect("tool registry lock").get(name).cloned()
    }

    /// Execute a tool by name with the given arguments and context.
    pub async fn execute(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: &super::trait_def::ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::NotFound(name.to_string()))?;
        tool.execute(args, ctx).await
    }

    /// Return all registered tool names.
    pub fn list_tools(&self) -> Vec<String> {
        let map = self.tools.read().expect("tool registry lock");
        let mut names: Vec<String> = map.keys().cloned().collect();
        names.sort();
        names
    }

    /// Tool name + description pairs for UI.
    pub fn list_tools_detailed(&self) -> Vec<(String, String)> {
        let map = self.tools.read().expect("tool registry lock");
        let mut out: Vec<_> = map
            .values()
            .map(|t| (t.name().to_string(), t.description().to_string()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Produce the OpenAI-compatible `Vec<ToolDef>` for sending in API requests.
    pub fn to_openai_tools(&self) -> Vec<ToolDef> {
        let map = self.tools.read().expect("tool registry lock");
        map.values().map(|t| t.to_openai_tool()).collect()
    }

    /// Register the default set of built-in tools.
    pub fn register_default_tools(&mut self) {
        self.register(crate::tools::bash::DoBash::new());
        self.register(crate::tools::file_ops::DoFileRead::new());
        self.register(crate::tools::file_ops::DoFileWrite::new());
        self.register(crate::tools::file_ops::DoFileEdit::new());
        self.register(crate::tools::skill_ops::DoSkillList::new());
        self.register(crate::tools::skill_ops::DoSkillInstall::new());
    }

    /// Return the number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.read().expect("tool registry lock").len()
    }

    /// Return true if no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.tools.read().expect("tool registry lock").is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::trait_def::ToolContext;
    use async_trait::async_trait;
    use std::path::PathBuf;

    struct StubTool;

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            "do_stub"
        }
        fn description(&self) -> &str {
            "A stub tool for testing"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            })
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, super::ToolError> {
            Ok(ToolResult::ok("stub result"))
        }
    }

    #[tokio::test]
    async fn test_register_and_get() {
        let mut reg = ToolRegistry::new();
        reg.register(StubTool);

        assert!(reg.get("do_stub").is_some());
        assert!(reg.get("nonexistent").is_none());
        assert_eq!(reg.list_tools(), vec!["do_stub"]);
        assert_eq!(reg.len(), 1);
    }

    #[tokio::test]
    async fn test_to_openai_tools() {
        let mut reg = ToolRegistry::new();
        reg.register(StubTool);

        let tools = reg.to_openai_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "do_stub");
    }

    #[tokio::test]
    async fn test_execute_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(StubTool);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: "test".into(),
            tool_call_id: "call_1".into(),
            sender: tx,
            safety_guard: Arc::new(crate::safety::guard::SafetyGuard::new(&[], true)),
        };

        let result = reg.execute("do_stub", serde_json::json!({}), &ctx).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.output, "stub result");
    }

    #[tokio::test]
    async fn test_execute_unknown_tool() {
        let reg = ToolRegistry::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: "test".into(),
            tool_call_id: "call_1".into(),
            sender: tx,
            safety_guard: Arc::new(crate::safety::guard::SafetyGuard::new(&[], true)),
        };

        let result = reg.execute("nonexistent", serde_json::json!({}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::NotFound(name) => assert_eq!(name, "nonexistent"),
            _ => panic!("expected NotFound"),
        }
    }
}
