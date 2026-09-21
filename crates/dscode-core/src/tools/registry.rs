//! ToolRegistry — the agent's collection of available tools.
//!
//! Provides registration, lookup, and schema generation for the LLM API
//! (OpenAI-compatible tools array). Supports runtime registration (MCP).

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use tracing::warn;

use crate::providers::trait_def::ToolDef;
use crate::tools::trait_def::{Tool, ToolError, ToolResult};

type ToolMap = HashMap<String, Arc<Box<dyn Tool>>>;

/// A thread-safe registry of all tools available to the agent.
///
/// Tools are stored behind `Arc` so they can be shared across the agent loop.
/// Interior mutability allows MCP tools to be registered after startup.
pub struct ToolRegistry {
    tools: RwLock<ToolMap>,
}

impl ToolRegistry {
    /// Create a new, empty tool registry.
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
        }
    }

    /// Read the map, recovering from a poisoned lock.
    ///
    /// A panic while holding the write lock used to poison it, after which
    /// *every* lookup panicked too. A registry is a plain map — there is no
    /// invariant a half-finished write can break that is worth panicking the
    /// whole app over.
    fn read_lock(&self) -> RwLockReadGuard<'_, ToolMap> {
        self.tools.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write_lock(&self) -> RwLockWriteGuard<'_, ToolMap> {
        self.tools.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Register a tool under its `name()`.
    ///
    /// A duplicate name is logged and skipped (never a panic — the old
    /// `assert!` fired while holding the write lock, poisoning it). Use
    /// [`Self::register_or_replace`] when replacing is intended.
    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        let name = tool.name().to_string();
        let mut map = self.write_lock();
        if map.contains_key(&name) {
            warn!(
                tool = %name,
                "tool already registered — skipping duplicate registration"
            );
            return;
        }
        map.insert(name, Arc::new(Box::new(tool)));
    }

    /// Register or replace a tool (used for MCP hot-reload).
    pub fn register_or_replace<T: Tool + 'static>(&self, tool: T) {
        let name = tool.name().to_string();
        let mut map = self.write_lock();
        map.insert(name, Arc::new(Box::new(tool)));
    }

    /// Remove tools matching a predicate (e.g. all `mcp_*`).
    pub fn unregister_where(&self, pred: impl Fn(&str) -> bool) {
        let mut map = self.write_lock();
        map.retain(|name, _| !pred(name));
    }

    /// Look up a tool by name. Returns `None` if not found.
    pub fn get(&self, name: &str) -> Option<Arc<Box<dyn Tool>>> {
        self.read_lock().get(name).cloned()
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
        let map = self.read_lock();
        let mut names: Vec<String> = map.keys().cloned().collect();
        names.sort();
        names
    }

    /// Tool name + description pairs for UI.
    pub fn list_tools_detailed(&self) -> Vec<(String, String)> {
        let map = self.read_lock();
        let mut out: Vec<_> = map
            .values()
            .map(|t| (t.name().to_string(), t.description().to_string()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Produce the OpenAI-compatible `Vec<ToolDef>` for sending in API requests.
    ///
    /// Sorted by tool name on purpose: iterating a `HashMap` yields a different
    /// order per process/instance, and providers that do prefix-based prompt
    /// caching re-bill the whole tools array whenever its order changes.
    pub fn to_openai_tools(&self) -> Vec<ToolDef> {
        let map = self.read_lock();
        let mut tools: Vec<ToolDef> = map.values().map(|t| t.to_openai_tool()).collect();
        tools.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        tools
    }

    /// Register the default set of built-in tools, with image generation on.
    pub fn register_default_tools(&mut self) {
        self.register_default_tools_with_image(true);
    }

    /// Register the default tools, deciding whether `do_image_generate` is among
    /// them.
    ///
    /// `image_enabled = false` has to mean the tool is **absent**, not merely
    /// that it refuses to run: `to_openai_tools` sends every registered tool on
    /// every request, so a disabled-but-registered tool still costs its
    /// definition tokens each turn and still invites a call that can only fail.
    /// Call sites that have the config should pass
    /// `config.generation.image_enabled`.
    pub fn register_default_tools_with_image(&mut self, image_enabled: bool) {
        self.register(crate::tools::bash::DoBash::new());
        self.register(crate::tools::file_ops::DoFileRead::new());
        self.register(crate::tools::file_ops::DoFileWrite::new());
        self.register(crate::tools::file_ops::DoFileEdit::new());
        self.register(crate::tools::skill_ops::DoSkillList::new());
        self.register(crate::tools::skill_ops::DoSkillInstall::new());
        self.register(crate::tools::web::DoWebFetch::new());
        self.register(crate::tools::web::DoWebSearch::new());
        self.register(crate::tools::web::DoDeepSearch::new());
        self.register(crate::tools::github::DoGithubSearch::new());
        self.register(crate::tools::feeds::DoRssRead::new());
        if image_enabled {
            self.register(crate::tools::image::DoImageGenerate::new());
        }
    }

    /// Return the number of registered tools.
    pub fn len(&self) -> usize {
        self.read_lock().len()
    }

    /// Return true if no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.read_lock().is_empty()
    }

    /// Snapshot registry keeping only named tools (shares Arc entries).
    ///
    /// The snapshot is **frozen**: MCP tools registered on the parent after
    /// this call are not visible through the returned registry (team sub-agents
    /// hold these for their whole lifetime, so an MCP hot-reload will not reach
    /// them). Re-snapshot after a config reload if that matters.
    pub fn with_allowlist(&self, names: &[&str]) -> Arc<ToolRegistry> {
        let allow: std::collections::HashSet<&str> = names.iter().copied().collect();
        let map = self.read_lock();
        let mut filtered = HashMap::new();
        for (name, tool) in map.iter() {
            if allow.contains(name.as_str()) {
                filtered.insert(name.clone(), tool.clone());
            }
        }
        Arc::new(ToolRegistry {
            tools: RwLock::new(filtered),
        })
    }

    /// Snapshot registry excluding named tools.
    ///
    /// Frozen for the same reason as [`Self::with_allowlist`]: later MCP
    /// registrations on the parent do not appear here.
    pub fn with_denylist(&self, names: &[&str]) -> Arc<ToolRegistry> {
        let deny: std::collections::HashSet<&str> = names.iter().copied().collect();
        let map = self.read_lock();
        let mut filtered = HashMap::new();
        for (name, tool) in map.iter() {
            if !deny.contains(name.as_str()) {
                filtered.insert(name.clone(), tool.clone());
            }
        }
        Arc::new(ToolRegistry {
            tools: RwLock::new(filtered),
        })
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

    struct ZetaTool;

    #[async_trait]
    impl Tool for ZetaTool {
        fn name(&self) -> &str {
            "do_zeta"
        }
        fn description(&self) -> &str {
            "Another stub"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, super::ToolError> {
            Ok(ToolResult::ok("zeta"))
        }
    }

    #[tokio::test]
    async fn test_duplicate_register_is_skipped_not_panicking() {
        let mut reg = ToolRegistry::new();
        reg.register(StubTool);
        // Used to assert!() while holding the write lock → panic + poisoned lock.
        reg.register(StubTool);
        assert_eq!(reg.len(), 1);
        assert!(reg.get("do_stub").is_some());
    }

    #[tokio::test]
    async fn test_to_openai_tools_is_sorted() {
        let mut reg = ToolRegistry::new();
        reg.register(ZetaTool);
        reg.register(StubTool);
        let names: Vec<String> = reg
            .to_openai_tools()
            .into_iter()
            .map(|t| t.function.name)
            .collect();
        assert_eq!(names, vec!["do_stub", "do_zeta"]);
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
        permission_hub: None,
        permission_timeout_secs: 120,
        team_agent_id: None,
        file_ownership: None,
        ownership_enforced: false,
        ownership_soft_log_only: true,
        read_paths: None,
        read_before_edit: false,
        };

        let result = reg.execute("do_stub", serde_json::json!({}), &ctx).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.output, "stub result");
    }

    #[test]
    fn test_allowlist_denylist_snapshot() {
        let mut reg = ToolRegistry::new();
        reg.register_default_tools();
        let allow = reg.with_allowlist(&["do_file_read", "do_skill_list"]);
        assert!(allow.get("do_file_read").is_some());
        assert!(allow.get("do_bash").is_none());
        let deny = reg.with_denylist(&["do_skill_install"]);
        assert!(deny.get("do_bash").is_some());
        assert!(deny.get("do_skill_install").is_none());
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
        permission_hub: None,
        permission_timeout_secs: 120,
        team_agent_id: None,
        file_ownership: None,
        ownership_enforced: false,
        ownership_soft_log_only: true,
        read_paths: None,
        read_before_edit: false,
        };

        let result = reg.execute("nonexistent", serde_json::json!({}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::NotFound(name) => assert_eq!(name, "nonexistent"),
            _ => panic!("expected NotFound"),
        }
    }
}
