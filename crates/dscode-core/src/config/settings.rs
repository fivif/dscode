//! Configuration system — single TOML file for all settings.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Main configuration, persisted to ~/.dscode/config.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Default model to use (e.g. "deepseek-v4-pro")
    #[serde(default = "default_model")]
    pub default_model: String,

    /// Routing model (cheap, fast) for simple tasks
    #[serde(default = "default_router")]
    pub router_model: String,

    /// Active channel for the default model (deepseek / openai / anthropic / ollama).
    /// Needed because custom OpenAI-compatible ids often do not match name prefixes.
    #[serde(default = "default_active_provider")]
    pub active_provider: String,

    /// Provider configurations
    #[serde(default)]
    pub providers: ProviderConfigs,

    /// Session settings
    #[serde(default)]
    pub session: SessionConfig,

    /// Safety settings
    #[serde(default)]
    pub safety: SafetyConfig,

    /// Generation settings
    #[serde(default)]
    pub generation: GenerationConfig,

    /// Context window settings
    #[serde(default)]
    pub context: ContextConfig,

    /// Extension settings
    #[serde(default)]
    pub extensions: ExtensionConfig,

    /// HTTP / SOCKS proxy for outbound network
    #[serde(default)]
    pub proxy: ProxyConfig,

    /// Agent behaviour (global system prompt, etc.)
    #[serde(default)]
    pub agent: AgentConfig,

    /// Multi-agent /teams settings (v2 runtime).
    #[serde(default)]
    pub teams: crate::teams::config::TeamsConfig,
}

fn default_model() -> String {
    String::new()
}

fn default_router() -> String {
    String::new()
}

fn default_active_provider() -> String {
    "deepseek".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_model: default_model(),
            router_model: default_router(),
            active_provider: default_active_provider(),
            providers: ProviderConfigs::default(),
            session: SessionConfig::default(),
            safety: SafetyConfig::default(),
            generation: GenerationConfig::default(),
            context: ContextConfig::default(),
            extensions: ExtensionConfig::default(),
            proxy: ProxyConfig::default(),
            agent: AgentConfig::default(),
            teams: crate::teams::config::TeamsConfig::default(),
        }
    }
}

impl Config {
    /// Load config from ~/.dscode/config.toml, creating default if missing.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::config_path()?;
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let mut config: Config = toml::from_str(&content)?;
            // Migrate legacy generation.proxy_url → proxy.url
            if config.proxy.url.trim().is_empty()
                && !config.generation.proxy_url.trim().is_empty()
            {
                config.proxy.url = config.generation.proxy_url.trim().to_string();
            }
            Ok(config)
        } else {
            let config = Config::default();
            config.save()?;
            Ok(config)
        }
    }

    /// Whether a non-empty, well-formed proxy URL is configured.
    pub fn proxy_is_configured(&self) -> bool {
        self.proxy.is_configured()
    }

    /// Effective proxy URL for a model channel (None = direct).
    /// Global force wins when proxy is valid.
    pub fn proxy_for_provider(&self, provider_key: &str) -> Option<&str> {
        if !self.proxy.is_configured() {
            return None;
        }
        if self.proxy.global {
            return Some(self.proxy.url.trim());
        }
        let channel_wants = match provider_key {
            "deepseek" => self.providers.deepseek.use_proxy,
            "openai" => self.providers.openai.use_proxy,
            "anthropic" => self.providers.anthropic.use_proxy,
            "ollama" => self.providers.ollama.use_proxy,
            _ => false,
        };
        if channel_wants {
            Some(self.proxy.url.trim())
        } else {
            None
        }
    }

    /// Effective proxy for the provider that serves `model`.
    pub fn proxy_for_model(&self, model: &str) -> Option<&str> {
        let key = self.provider_key_for_model(model);
        self.proxy_for_provider(&key)
    }

    pub fn proxy_for_mcp(&self) -> Option<&str> {
        if !self.proxy.is_configured() {
            return None;
        }
        if self.proxy.global || self.extensions.mcp_use_proxy {
            Some(self.proxy.url.trim())
        } else {
            None
        }
    }

    pub fn proxy_for_skills(&self) -> Option<&str> {
        if !self.proxy.is_configured() {
            return None;
        }
        if self.proxy.global || self.extensions.skills_use_proxy {
            Some(self.proxy.url.trim())
        } else {
            None
        }
    }

    /// Soft default for web tools when the agent omits `use_proxy`.
    /// Agent can still force direct or proxy per call if a proxy URL exists.
    pub fn proxy_for_web(&self) -> Option<&str> {
        if !self.proxy.is_configured() {
            return None;
        }
        if self.proxy.global || self.proxy.web_use_proxy {
            Some(self.proxy.url.trim())
        } else {
            None
        }
    }

    /// Raw configured proxy URL for web tools (if any), ignoring toggles.
    pub fn web_proxy_url_if_configured(&self) -> Option<&str> {
        if self.proxy.is_configured() {
            Some(self.proxy.url.trim())
        } else {
            None
        }
    }

    /// Save config to ~/.dscode/config.toml
    pub fn save(&self) -> Result<(), ConfigError> {
        let config_path = Self::config_path()?;
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&config_path, content)?;
        Ok(())
    }

    /// Resolve which channel config to use for a model id.
    ///
    /// Prefer explicit `active_provider` when the model is the default (or when
    /// name-prefix inference is ambiguous), so custom OpenAI-compatible gateways
    /// with arbitrary model ids still hit the OpenAI channel credentials.
    pub fn provider_for_model(&self, model: &str) -> Option<ProviderConfig> {
        let key = self.provider_key_for_model(model);
        self.provider_config_by_key(&key)
    }

    /// Channel key: deepseek | openai | anthropic | ollama
    pub fn provider_key_for_model(&self, model: &str) -> String {
        let m = model.trim();
        // If this is the selected default model, trust active_provider first
        // (custom gateway ids rarely match gpt-/claude- prefixes).
        let active = self.active_provider.trim().to_lowercase();
        if !m.is_empty()
            && m == self.default_model.trim()
            && matches!(
                active.as_str(),
                "deepseek" | "openai" | "anthropic" | "ollama"
            )
        {
            return active;
        }

        if m.starts_with("deepseek") {
            return "deepseek".into();
        }
        if m.starts_with("openai/")
            || m.starts_with("gpt-")
            || m.starts_with("o1")
            || m.starts_with("o3")
            || m.starts_with("o4")
            || m.starts_with("chatgpt")
        {
            return "openai".into();
        }
        if m.starts_with("anthropic/") || m.starts_with("claude-") {
            return "anthropic".into();
        }
        if m.starts_with("ollama/") || m.starts_with("llama") {
            return "ollama".into();
        }

        // Fall back to active channel, then deepseek
        if matches!(
            active.as_str(),
            "deepseek" | "openai" | "anthropic" | "ollama"
        ) {
            return active;
        }
        "deepseek".into()
    }

    pub fn provider_config_by_key(&self, key: &str) -> Option<ProviderConfig> {
        match key {
            "deepseek" => Some(self.providers.deepseek.clone()),
            "openai" => Some(self.providers.openai.clone()),
            "anthropic" => Some(self.providers.anthropic.clone()),
            "ollama" => Some(self.providers.ollama.clone()),
            _ => Some(self.providers.deepseek.clone()),
        }
    }

    fn config_path() -> Result<PathBuf, ConfigError> {
        let home = dirs_next().ok_or(ConfigError::NoHomeDir)?;
        Ok(home.join(".dscode").join("config.toml"))
    }

    /// Get the dscode data directory (~/.dscode/)
    pub fn data_dir() -> Result<PathBuf, ConfigError> {
        let home = dirs_next().ok_or(ConfigError::NoHomeDir)?;
        Ok(home.join(".dscode"))
    }

    pub fn sessions_dir() -> Result<PathBuf, ConfigError> {
        Ok(Self::data_dir()?.join("sessions"))
    }

    pub fn tasks_dir() -> Result<PathBuf, ConfigError> {
        Ok(Self::data_dir()?.join("tasks"))
    }
}

fn dirs_next() -> Option<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .or_else(|_| {
            std::env::var("USERPROFILE").map(PathBuf::from)
        })
        .ok()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfigs {
    #[serde(default)]
    pub deepseek: ProviderConfig,
    #[serde(default)]
    pub openai: ProviderConfig,
    #[serde(default)]
    pub anthropic: ProviderConfig,
    #[serde(default)]
    pub ollama: ProviderConfig,
}

impl Default for ProviderConfigs {
    fn default() -> Self {
        Self {
            deepseek: ProviderConfig {
                api_key: String::new(),
                base_url: "https://api.deepseek.com/v1".into(),
                enabled: true,
                use_proxy: false,
                ..Default::default()
            },
            openai: ProviderConfig {
                api_key: String::new(),
                base_url: "https://api.openai.com/v1".into(),
                enabled: false,
                use_proxy: false,
                ..Default::default()
            },
            anthropic: ProviderConfig {
                api_key: String::new(),
                base_url: "https://api.anthropic.com".into(),
                enabled: false,
                use_proxy: false,
                ..Default::default()
            },
            ollama: ProviderConfig {
                api_key: String::new(),
                base_url: "http://localhost:11434/v1".into(),
                enabled: false,
                use_proxy: false,
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Use configured HTTP proxy for this channel (ignored if proxy not configured;
    /// forced on when `proxy.global` is true).
    #[serde(default)]
    pub use_proxy: bool,
    /// Last successful `/models` scan for this channel (persisted). Empty = not scanned.
    /// Full catalog for the settings multi-select UI.
    #[serde(default)]
    pub model_list: Vec<String>,
    /// Models that appear in the global picker (default model + input box).
    /// - `None` / missing in TOML: not curated yet → treat as "all of model_list" (legacy).
    /// - `Some([])`: user cleared selection → contribute nothing to global list.
    /// - `Some([...])`: explicit whitelist.
    #[serde(default)]
    pub enabled_models: Option<Vec<String>>,
    /// Last selected model id for this channel (optional UI hint / fallback).
    #[serde(default)]
    pub model: String,
}

impl ProviderConfig {
    /// Models that should appear in global pickers for this channel.
    pub fn effective_enabled_models(&self) -> Vec<String> {
        match &self.enabled_models {
            Some(v) => v
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            None => self
                .model_list
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }
    }
}

#[cfg(test)]
mod proxy_web_tests {
    use super::{Config, ProxyConfig};

    #[test]
    fn web_proxy_off_by_default() {
        let mut c = Config::default();
        c.proxy = ProxyConfig {
            url: "http://127.0.0.1:7890".into(),
            global: false,
            web_use_proxy: false,
        };
        assert!(c.proxy_for_web().is_none());
    }

    #[test]
    fn web_proxy_toggle() {
        let mut c = Config::default();
        c.proxy = ProxyConfig {
            url: "http://127.0.0.1:7890".into(),
            global: false,
            web_use_proxy: true,
        };
        assert_eq!(c.proxy_for_web(), Some("http://127.0.0.1:7890"));
    }

    #[test]
    fn web_proxy_global_forces() {
        let mut c = Config::default();
        c.proxy = ProxyConfig {
            url: "socks5://127.0.0.1:1080".into(),
            global: true,
            web_use_proxy: false,
        };
        assert_eq!(c.proxy_for_web(), Some("socks5://127.0.0.1:1080"));
    }
}

#[cfg(test)]
mod provider_enabled_models_tests {
    use super::ProviderConfig;

    #[test]
    fn none_falls_back_to_model_list() {
        let p = ProviderConfig {
            model_list: vec!["a".into(), "b".into()],
            enabled_models: None,
            ..Default::default()
        };
        assert_eq!(p.effective_enabled_models(), vec!["a", "b"]);
    }

    #[test]
    fn some_empty_means_nothing() {
        let p = ProviderConfig {
            model_list: vec!["a".into(), "b".into()],
            enabled_models: Some(vec![]),
            ..Default::default()
        };
        assert!(p.effective_enabled_models().is_empty());
    }

    #[test]
    fn some_whitelist() {
        let p = ProviderConfig {
            model_list: vec!["a".into(), "b".into(), "c".into()],
            enabled_models: Some(vec!["b".into()]),
            ..Default::default()
        };
        assert_eq!(p.effective_enabled_models(), vec!["b"]);
    }
}

fn default_true() -> bool { true }

/// Outbound proxy settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Proxy URL, e.g. `http://127.0.0.1:7890` or `socks5://127.0.0.1:1080`.
    /// Empty = not configured (channel/mcp/skill proxy toggles cannot enable).
    #[serde(default)]
    pub url: String,
    /// When true and URL is valid, force proxy for the whole app (LLM / MCP / skills / web).
    /// Individual toggles are treated as on and must not be turned off in UI.
    #[serde(default)]
    pub global: bool,
    /// Built-in web tools (`do_web_search` / `do_web_fetch`) use the proxy.
    /// Forced on when `global` is true. Default false — enable when network needs it.
    #[serde(default)]
    pub web_use_proxy: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            global: false,
            web_use_proxy: false,
        }
    }
}

impl ProxyConfig {
    /// Non-empty URL with a supported scheme.
    pub fn is_configured(&self) -> bool {
        let u = self.url.trim().to_lowercase();
        if u.is_empty() {
            return false;
        }
        u.starts_with("http://")
            || u.starts_with("https://")
            || u.starts_with("socks5://")
            || u.starts_with("socks5h://")
            || u.starts_with("socks4://")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default = "default_30")]
    pub retention_days: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self { retention_days: default_30() }
    }
}

fn default_30() -> u32 { 30 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyConfig {
    #[serde(default)]
    pub allow_write_outside_project: bool,
    /// Command patterns to block (regex) — treated as hard blocks.
    #[serde(default)]
    pub blocked_commands: Vec<String>,
    /// Default timeout for tool execution in seconds
    #[serde(default = "default_timeout")]
    pub tool_timeout_secs: u64,
    /// When true, Confirm-level dangerous commands run without UI prompt.
    /// Hard-blocked commands are still always denied. Default false (Safe mode).
    #[serde(default)]
    pub absolute_trust: bool,
    /// Permission prompt timeout in seconds (default 120).
    #[serde(default = "default_timeout")]
    pub permission_timeout_secs: u64,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            allow_write_outside_project: false,
            blocked_commands: vec![
                "rm -rf /".into(),
                "mkfs\\.".into(),
                "dd if=".into(),
                ":(){ :|:& };:".into(),
            ],
            tool_timeout_secs: default_timeout(),
            absolute_trust: false,
            permission_timeout_secs: default_timeout(),
        }
    }
}

fn default_timeout() -> u64 { 120 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationConfig {
    /// Reasoning effort: low, medium, high, max
    #[serde(default = "default_reasoning")]
    pub reasoning_effort: String,
    /// Max tokens per response
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Temperature
    #[serde(default)]
    pub temperature: f64,
    /// Legacy proxy field — prefer top-level `[proxy].url`. Kept for migration.
    #[serde(default)]
    pub proxy_url: String,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            reasoning_effort: default_reasoning(),
            max_tokens: default_max_tokens(),
            temperature: 0.0,
            proxy_url: String::new(),
        }
    }
}

fn default_reasoning() -> String { "max".into() }
fn default_max_tokens() -> u32 { 8192 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Total context window size in tokens (default: 1M for DeepSeek V4)
    #[serde(default = "default_context_window")]
    pub window_tokens: u64,
    /// Fraction of window used before triggering compression (0.0-1.0)
    #[serde(default = "default_compress_threshold")]
    pub compress_threshold: f64,
    /// Hard cap on ReAct tool/LLM turns per user message (default 120).
    #[serde(default = "default_max_agent_iterations")]
    pub max_agent_iterations: u32,
}

fn default_context_window() -> u64 { 1_000_000 }
fn default_compress_threshold() -> f64 { 0.8 }
fn default_max_agent_iterations() -> u32 { 120 }

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            window_tokens: default_context_window(),
            compress_threshold: default_compress_threshold(),
            max_agent_iterations: default_max_agent_iterations(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExtensionConfig {
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    pub skills_dirs: Vec<String>,
    /// Use proxy when connecting MCP servers (if proxy configured; forced by global).
    #[serde(default)]
    pub mcp_use_proxy: bool,
    /// Use proxy for skill package git clone downloads.
    #[serde(default)]
    pub skills_use_proxy: bool,
}

/// Global agent instructions (system prompt customisation).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    /// User-written global prompt. Empty = use built-in default only.
    #[serde(default)]
    pub global_prompt: String,
    /// When true, inject Scribe memory recall snippets into the system prompt.
    #[serde(default)]
    pub memory_enabled: bool,
    /// When true and `global_prompt` is non-empty, replace the built-in system
    /// prompt entirely. When false, append after the built-in prompt.
    #[serde(default)]
    pub replace_system_prompt: bool,
    /// Require do_file_read before do_file_edit/write on the same path (session).
    #[serde(default)]
    pub read_before_edit: bool,
    /// After a successful turn, optionally extract a short memory note (opt-in).
    #[serde(default)]
    pub memory_auto_ingest: bool,
}

impl AgentConfig {
    /// Build the effective system prompt given the built-in default text.
    pub fn resolve_system_prompt(&self, default_prompt: &str) -> String {
        let custom = self.global_prompt.trim();
        if custom.is_empty() {
            return default_prompt.to_string();
        }
        if self.replace_system_prompt {
            custom.to_string()
        } else {
            format!(
                "{default_prompt}\n\n## User global instructions\n{custom}"
            )
        }
    }
}

#[cfg(test)]
mod agent_config_tests {
    use super::*;

    #[test]
    fn resolve_empty_uses_default() {
        let a = AgentConfig::default();
        assert_eq!(a.resolve_system_prompt("DEFAULT"), "DEFAULT");
    }

    #[test]
    fn resolve_appends_by_default() {
        let a = AgentConfig {
            global_prompt: "  speak Chinese  ".into(),
            replace_system_prompt: false,
            ..Default::default()
        };
        let out = a.resolve_system_prompt("DEFAULT");
        assert!(out.starts_with("DEFAULT"));
        assert!(out.contains("speak Chinese"));
        assert!(out.contains("User global instructions"));
    }

    #[test]
    fn resolve_replace() {
        let a = AgentConfig {
            global_prompt: "ONLY CUSTOM".into(),
            replace_system_prompt: true,
            ..Default::default()
        };
        assert_eq!(a.resolve_system_prompt("DEFAULT"), "ONLY CUSTOM");
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// Build a reqwest Client with optional proxy.
pub fn build_http_client(proxy_url: Option<&str>) -> Result<reqwest::Client, String> {
    use std::time::Duration;
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(180));
    if let Some(url) = proxy_url.map(str::trim).filter(|u| !u.is_empty()) {
        let proxy = reqwest::Proxy::all(url).map_err(|e| format!("无效代理 URL: {e}"))?;
        builder = builder.proxy(proxy);
    } else {
        // Avoid picking up ambient HTTP_PROXY from environment when user wants direct
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

/// Proxy-related env keys we set/clear on child processes.
const PROXY_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// Apply standard proxy env vars to a std process Command (git skill install).
pub fn apply_proxy_env(cmd: &mut std::process::Command, proxy_url: Option<&str>) {
    apply_proxy_env_inner(cmd, proxy_url);
}

/// Apply standard proxy env vars to a tokio process Command (MCP npx).
pub fn apply_proxy_env_tokio(cmd: &mut tokio::process::Command, proxy_url: Option<&str>) {
    apply_proxy_env_inner(cmd, proxy_url);
}

fn apply_proxy_env_inner<C: ProxyEnvCmd>(cmd: &mut C, proxy_url: Option<&str>) {
    if let Some(url) = proxy_url.map(str::trim).filter(|u| !u.is_empty()) {
        for k in &[
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            cmd.set_env(k, url);
        }
    } else {
        for k in PROXY_ENV_KEYS {
            cmd.remove_env(k);
        }
    }
}

trait ProxyEnvCmd {
    fn set_env(&mut self, key: &str, val: &str);
    fn remove_env(&mut self, key: &str);
}

impl ProxyEnvCmd for std::process::Command {
    fn set_env(&mut self, key: &str, val: &str) {
        self.env(key, val);
    }
    fn remove_env(&mut self, key: &str) {
        self.env_remove(key);
    }
}

impl ProxyEnvCmd for tokio::process::Command {
    fn set_env(&mut self, key: &str, val: &str) {
        self.env(key, val);
    }
    fn remove_env(&mut self, key: &str) {
        self.env_remove(key);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Cannot find home directory")]
    NoHomeDir,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),
}

#[cfg(test)]
mod enabled_models_serde_tests {
    use super::{Config, ProviderConfig};

    #[test]
    fn toml_missing_enabled_models_is_none() {
        let raw = r#"
default_model = "m1"
active_provider = "deepseek"

[providers.deepseek]
api_key = "k"
base_url = "https://api.deepseek.com/v1"
enabled = true
model_list = ["m1", "m2"]
model = "m1"
"#;
        let c: Config = toml::from_str(raw).expect("parse");
        assert!(c.providers.deepseek.enabled_models.is_none());
        assert_eq!(
            c.providers.deepseek.effective_enabled_models(),
            vec!["m1", "m2"]
        );
    }

    #[test]
    fn toml_empty_enabled_models_is_some_empty() {
        let raw = r#"
default_model = ""
active_provider = "openai"

[providers.openai]
api_key = "k"
base_url = "https://api.openai.com/v1"
enabled = true
model_list = ["gpt-a", "gpt-b"]
enabled_models = []
"#;
        let c: Config = toml::from_str(raw).expect("parse");
        assert_eq!(c.providers.openai.enabled_models, Some(vec![]));
        assert!(c.providers.openai.effective_enabled_models().is_empty());
    }

    #[test]
    fn toml_whitelist_roundtrip() {
        let mut c = Config::default();
        c.providers.deepseek.model_list = vec!["a".into(), "b".into(), "c".into()];
        c.providers.deepseek.enabled_models = Some(vec!["a".into(), "c".into()]);
        let s = toml::to_string_pretty(&c).expect("ser");
        assert!(s.contains("enabled_models"));
        let back: Config = toml::from_str(&s).expect("de");
        assert_eq!(
            back.providers.deepseek.enabled_models,
            Some(vec!["a".into(), "c".into()])
        );
        assert_eq!(
            back.providers.deepseek.effective_enabled_models(),
            vec!["a", "c"]
        );
    }

    #[test]
    fn json_null_and_array_for_desktop_payload() {
        // Desktop may send enabled_models as array; Option deserializes
        let p: ProviderConfig = serde_json::from_str(
            r#"{"api_key":"","base_url":"","enabled":true,"use_proxy":false,"model_list":["x"],"enabled_models":["x"],"model":"x"}"#,
        )
        .unwrap();
        assert_eq!(p.enabled_models, Some(vec!["x".into()]));

        let p2: ProviderConfig = serde_json::from_str(
            r#"{"api_key":"","base_url":"","enabled":true,"use_proxy":false,"model_list":["x"],"enabled_models":[],"model":""}"#,
        )
        .unwrap();
        assert_eq!(p2.enabled_models, Some(vec![]));

        let p3: ProviderConfig = serde_json::from_str(
            r#"{"api_key":"","base_url":"","enabled":true,"use_proxy":false,"model_list":["x"],"model":"x"}"#,
        )
        .unwrap();
        assert!(p3.enabled_models.is_none());
    }
}
