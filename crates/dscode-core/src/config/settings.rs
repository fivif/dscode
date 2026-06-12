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
}

fn default_model() -> String {
    "deepseek-v4-pro".into()
}

fn default_router() -> String {
    "deepseek-v4-flash".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_model: default_model(),
            router_model: default_router(),
            providers: ProviderConfigs::default(),
            session: SessionConfig::default(),
            safety: SafetyConfig::default(),
            generation: GenerationConfig::default(),
            context: ContextConfig::default(),
            extensions: ExtensionConfig::default(),
        }
    }
}

impl Config {
    /// Load config from ~/.dscode/config.toml, creating default if missing.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::config_path()?;
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            let config = Config::default();
            config.save()?;
            Ok(config)
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

    /// Get the active provider's API key and base URL for the given model.
    pub fn provider_for_model(&self, model: &str) -> Option<ProviderConfig> {
        if model.starts_with("deepseek") {
            Some(self.providers.deepseek.clone())
        } else if model.starts_with("openai/") || model.starts_with("gpt-") {
            Some(self.providers.openai.clone())
        } else if model.starts_with("anthropic/") || model.starts_with("claude-") {
            Some(self.providers.anthropic.clone())
        } else if model.starts_with("ollama/") {
            Some(self.providers.ollama.clone())
        } else {
            // Default to DeepSeek (OpenAI-compatible)
            Some(self.providers.deepseek.clone())
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

    pub fn wiki_dir() -> Result<PathBuf, ConfigError> {
        Ok(Self::data_dir()?.join("wiki"))
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
            },
            openai: ProviderConfig {
                api_key: String::new(),
                base_url: "https://api.openai.com/v1".into(),
                enabled: false,
            },
            anthropic: ProviderConfig {
                api_key: String::new(),
                base_url: "https://api.anthropic.com".into(),
                enabled: false,
            },
            ollama: ProviderConfig {
                api_key: String::new(),
                base_url: "http://localhost:11434/v1".into(),
                enabled: false,
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
}

fn default_true() -> bool { true }

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
    /// Command patterns to block (regex)
    #[serde(default)]
    pub blocked_commands: Vec<String>,
    /// Default timeout for tool execution in seconds
    #[serde(default = "default_timeout")]
    pub tool_timeout_secs: u64,
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
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            reasoning_effort: default_reasoning(),
            max_tokens: default_max_tokens(),
            temperature: 0.0,
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
}

fn default_context_window() -> u64 { 1_000_000 }
fn default_compress_threshold() -> f64 { 0.8 }

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            window_tokens: default_context_window(),
            compress_threshold: default_compress_threshold(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExtensionConfig {
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    pub skills_dirs: Vec<String>,
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
