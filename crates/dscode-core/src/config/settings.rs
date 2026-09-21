//! Configuration system — TOML for settings, YAML for secrets.
//!
//! Three things about this layer are load-bearing and worth knowing before
//! changing it:
//!
//!   · **A save is an overlay, not a rewrite.** `save()` serialises the config
//!     and merges it over the file that is already there (`patch::overlay`), so
//!     comments survive and so does any key this build does not know about.
//!     Without that, `toml::to_string_pretty` would flatten the file to exactly
//!     the fields declared below and delete the rest — which is the whole reason
//!     the desktop frontend has to read-modify-write the entire config on every
//!     keystroke. See `patch.rs`.
//!   · **Secrets are not in here.** `ProviderConfig::api_key` is read for wire
//!     compatibility with payloads that still carry it, but it is backfilled
//!     from — and migrated into — `~/.dscode/.credentials.yaml` on load, and
//!     stripped from what gets written. See `credentials.rs` for what the split
//!     does and does not promise.
//!   · **Every write is atomic.** `atomic::write_atomic` writes a sibling temp
//!     file, fsyncs, and renames. A crash mid-save leaves the previous config
//!     intact rather than a truncated file that would fail to parse on next
//!     boot, which presents to the user as "all my settings reset".

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::credentials::Credentials;
use super::providers::default_true;
use super::{atomic, patch};

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
    ///
    /// Also loads the credentials file and backfills `provider.api_key` from it,
    /// so every caller downstream keeps reading `provider.api_key` and does not
    /// need to know the two stores exist. A key still sitting in the TOML (an
    /// install that has not saved since the split) is honoured as a fallback and
    /// migrated out on the next save.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::config_path()?;
        let mut credentials = Credentials::load()?;
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let mut config: Config = toml::from_str(&content)?;
            // Migrate legacy generation.proxy_url → proxy.url
            if config.proxy.url.trim().is_empty()
                && !config.generation.proxy_url.trim().is_empty()
            {
                config.proxy.url = config.generation.proxy_url.trim().to_string();
            }
            let migrated = config.apply_credentials(&mut credentials);
            config.migrate_proxy_url_from_legacy(&content);
            if migrated {
                // The keys have moved; persist the credentials file immediately
                // so a crash before the first settings save cannot lose them at
                // the same moment the TOML that still held them gets rewritten.
                credentials.save()?;
            }
            Ok(config)
        } else {
            let config = Config::default();
            config.save()?;
            Ok(config)
        }
    }

    /// Fill every `provider.api_key` from `credentials`, falling back to the key
    /// still present in the TOML and recording it for migration.
    ///
    /// Returns whether anything was taken *from the TOML* and therefore needs to
    /// be written out to the credentials file.
    fn apply_credentials(&mut self, credentials: &mut Credentials) -> bool {
        let mut migrated = false;
        for (key, provider) in self.providers.iter_mut() {
            let stored = credentials.get(key);
            if !stored.is_empty() {
                provider.api_key = stored.to_string();
                continue;
            }
            let legacy = provider.api_key.trim();
            if !legacy.is_empty() {
                credentials.set(key, legacy);
                migrated = true;
            }
        }
        migrated
    }

    /// One-shot fixes applied to the raw TOML before anything writes it back.
    ///
    /// Today: drop the now-duplicated `generation.proxy_url`. It was the legacy
    /// home of the proxy setting and is folded into `proxy.url` above; leaving
    /// it in the file means the next hand-edit changes a value nothing reads,
    /// and it is what let the two copies disagree in the first place.
    fn migrate_proxy_url_from_legacy(&self, raw: &str) {
        let Ok(mut doc) = raw.parse::<toml_edit::Document>() else {
            return;
        };
        if !self.proxy.url.trim().is_empty()
            && patch::remove_path(&mut doc, &["generation", "proxy_url"])
        {
            if let Ok(path) = Self::config_path() {
                let _ = atomic::write_atomic(&path, &doc.to_string());
            }
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

    /// Save config to ~/.dscode/config.toml, and any API keys it carries to
    /// ~/.dscode/.credentials.yaml.
    ///
    /// The TOML write is an **overlay**: the current file is parsed, this
    /// config's non-secret fields are merged over it, and the result is written.
    /// Comments and keys this build does not understand therefore survive. See
    /// the module docs and `patch.rs`.
    ///
    /// Secrets go first. If the credentials write fails, the config file is left
    /// alone — the other order could strip a key from the TOML on a path that
    /// then failed to record it anywhere. The two writes are still not one
    /// transaction; see [`Self::save_with_credentials`] for the caller that can
    /// make them one.
    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_credentials()?;
        self.save_config_only()
    }

    /// Write `config.toml` without touching the credentials store.
    ///
    /// The split exists for callers that hold a config carrying **no** key at
    /// all — a payload read back out of the TOML rather than out of the running
    /// app. `save_credentials` treats a blank key as "not my business" (see its
    /// docs), so the two paths are equivalent *today*; naming this one makes the
    /// distinction explicit rather than relying on that accident.
    pub fn save_config_only(&self) -> Result<(), ConfigError> {
        let mut merged = self.merged_document()?;
        Self::strip_secrets(&mut merged);
        Self::write_config(&merged.to_string())
    }

    /// Write both halves under a lock, re-reading the credentials file inside it.
    ///
    /// True atomicity across two files does not exist, but the window that
    /// matters is narrower than it looks. Consider two saves racing:
    ///
    ///   A: load credentials → B: load credentials → A: write creds → B: write creds
    ///
    /// A key A introduced is silently gone, because B's copy was read before A's
    /// write and B overwrites the whole file. The lock plus the in-lock re-read
    /// closes exactly that: B's read now happens after A's write, so B's merge
    /// starts from A's state and the union survives. A same-key collision still
    /// resolves last-writer-wins, which is the correct semantic for two settings
    /// pages setting the same field.
    ///
    /// The dedicated, *actionable* entry point for the settings page is
    /// [`Self::set_api_key`]; this is for callers that already hold a whole
    /// config and cannot express the change as one field.
    pub fn save_with_credentials(&self) -> Result<(), ConfigError> {
        let _guard = credentials_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut credentials = Credentials::load()?;
        let mut changed = false;
        for (key, provider) in self.providers.iter() {
            let secret = provider.api_key.trim();
            if !secret.is_empty() && credentials.get(key) != secret {
                credentials.set(key, secret);
                changed = true;
            }
        }
        if changed {
            credentials.save()?;
        }
        self.save_config_only()
    }

    /// Set one channel's API key, leaving every other key and the whole of
    /// `config.toml` untouched.
    ///
    /// This is the interface the settings page should hold: changing a key is a
    /// one-field write, not a reason to round-trip an entire config through the
    /// UI and back — which is exactly the shape that made a stray save able to
    /// clobber sections it does not own.
    ///
    /// An empty `key` **removes** the stored secret, so this one method both
    /// sets and clears. (Contrast [`Self::save_with_credentials`], where a blank
    /// key means "unchanged".)
    pub fn set_api_key(&self, channel: &str, key: &str) -> Result<(), ConfigError> {
        let _guard = credentials_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut credentials = Credentials::load()?;
        if credentials.get(channel) == key.trim() {
            return Ok(());
        }
        credentials.set(channel, key);
        credentials.save()
    }

    /// This config as a document overlaid on whatever is currently on disk.
    ///
    /// Serialising at the *document* level this way — rather than to a string
    /// and re-parsing — is what lets `patch::remove_path` drop a single nested
    /// key after the fact.
    fn merged_document(&self) -> Result<toml_edit::Document, ConfigError> {
        let layer: toml_edit::Document = toml::to_string_pretty(self)?.parse()?;
        let path = Self::config_path()?;
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing.trim().is_empty() {
            return Ok(layer);
        }
        // A config file that will not parse is the user's to fix: reporting the
        // error beats silently replacing whatever is in it.
        let mut doc: toml_edit::Document = existing.parse()?;
        patch::merge_document(&mut doc, &layer);
        Ok(doc)
    }

    /// Remove every channel's `api_key` from a document about to be written.
    ///
    /// Two jobs, one pass. It keeps a stray key out of `config.toml` on a
    /// payload that carries one (the settings page round-trips the live key), and
    /// it clears out a key left behind by an install that predates the
    /// credentials split — that value would otherwise sit in the file forever,
    /// since nothing reads it any more now that `.credentials.yaml` is
    /// authoritative.
    ///
    /// Note this is **not** what keeps keys out of the file in the first place;
    /// `Config::load` backfills `provider.api_key` from the credentials store, so
    /// the in-memory config *does* hold the secret and serialising it would write
    /// it. This is the strip that stops that. See `providers.rs` for why the
    /// field is not `skip_serializing`.
    fn strip_secrets(doc: &mut toml_edit::Document) {
        let channels: Vec<&'static str> = super::providers::CHANNELS.to_vec();
        for key in channels {
            patch::remove_path(doc, &["providers", key, "api_key"]);
        }
    }

    fn write_config(content: &str) -> Result<(), ConfigError> {        let config_path = Self::config_path()?;
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        atomic::write_atomic(&config_path, content)?;
        Ok(())
    }

    /// Save only the API keys, leaving every other field of `config.toml` alone.
    ///
    /// This is the target the settings page will move to: a key change should not
    /// require the frontend to read the whole config, spread it, and post it all
    /// back just to avoid clobbering sections it does not own.
    ///
    /// A non-empty `api_key` means "the user set this". An empty one means
    /// "this payload did not carry a key" — NOT "delete it" — because most saves
    /// originate from the frontend's read-modify-write cycle and routing the key
    /// back through it is a lossy round trip we should not depend on. Clearing a
    /// key is therefore a deliberate act (delete the entry in the credentials
    /// file), not a side effect of saving an unrelated setting.
    pub fn save_credentials(&self) -> Result<(), ConfigError> {
        let mut credentials = Credentials::load()?;
        let mut changed = false;
        for (key, provider) in self.providers.iter() {
            let secret = provider.api_key.trim();
            if !secret.is_empty() && credentials.get(key) != secret {
                credentials.set(key, secret);
                changed = true;
            }
        }
        if changed {
            credentials.save()?;
        }
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

        // A channel that *advertises* this model outranks prefix inference.
        //
        // This is the relay case, and it is the difference between a working
        // install and a dead one. A gateway's whole purpose is to serve other
        // people's models, so `[providers.openai]` legitimately lists
        // `deepseek-flash` and a `deepseek-*` id that belongs to the relay is
        // indistinguishable, by name alone, from one that belongs to DeepSeek.
        // `model_list` is the user telling us which — and before this curve,
        // routing never consulted it, so the id went to a channel with no key.
        //
        // Deliberately ordered *after* the exact-default rule above: an explicit
        // `active_provider` is a stronger statement than a published list, and
        // a channel the user disabled must not be resurrected by curation.
        //
        // Ambiguity (two enabled channels claiming one id) falls through to
        // prefix inference rather than picking one arbitrarily.
        let mut claiming = self
            .providers
            .iter()
            .filter(|(_, p)| p.enabled && p.model_list.iter().any(|listed| listed.trim() == m))
            .map(|(name, _)| name);
        if let Some(first) = claiming.next() {
            if claiming.next().is_none() {
                return first.to_string();
            }
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

/// The user's home directory, or `None` when neither `HOME` nor `USERPROFILE`
/// is set. Public because `credentials` needs the same root, and two copies of
/// this resolution would eventually disagree about which one is authoritative.
pub(crate) fn home_dir() -> Option<PathBuf> {
    dirs_next()
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

/// Re-exported so call sites that say `settings::ProviderConfig` keep working;
/// the definitions now live in `config::providers`.
pub use super::providers::{default_base_url, ProviderConfig, ProviderConfigs, CHANNELS};

/// Serialises the read-modify-write of `.credentials.yaml`.
///
/// A module-level mutex is right here rather than a per-instance one: the state
/// being protected is the *file*, and every `Config` loaded from the same home
/// directory contends for it regardless of which struct instance it came from.
/// `unwrap_or_else(|e| e.into_inner())` on the lock is deliberate — a panic in
/// one save must not poison every later one, and the critical section touches
/// only process-local state plus one file write.
fn credentials_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}


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
    /// Enable the `do_image_generate` tool.
    #[serde(default = "default_true")]
    pub image_enabled: bool,
    /// Image model id sent to the provider's `/images/generations` endpoint.
    /// `gpt-image-1` is the default; `dall-e-3` is accepted too and is the id
    /// most other OpenAI-compatible gateways carry (see `tools::image`, which
    /// adapts the request and response shape to whichever one is configured).
    #[serde(default = "default_image_model")]
    pub image_model: String,
    /// Default image size, e.g. `1024x1024` (the tool's `size` arg wins).
    #[serde(default = "default_image_size")]
    pub image_size: String,
    /// Provider channel used for image generation (deepseek / openai /
    /// anthropic / ollama). Empty = follow `active_provider`.
    #[serde(default)]
    pub image_provider: String,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            reasoning_effort: default_reasoning(),
            max_tokens: default_max_tokens(),
            temperature: 0.0,
            proxy_url: String::new(),
            image_enabled: true,
            image_model: default_image_model(),
            image_size: default_image_size(),
            image_provider: String::new(),
        }
    }
}

fn default_reasoning() -> String { "max".into() }
fn default_max_tokens() -> u32 { 8192 }
fn default_image_model() -> String { "gpt-image-1".into() }
fn default_image_size() -> String { "1024x1024".into() }

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
    /// Windows only: absolute path to Git Bash `bash.exe` (equivalent to Claude
    /// Code's `CLAUDE_CODE_GIT_BASH_PATH`). When set and the file exists,
    /// do_bash / do_background prefer it over auto-detected Git for Windows.
    #[serde(default)]
    pub git_bash_path: String,
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
    #[error("TOML edit error: {0}")]
    TomlEdit(#[from] toml_edit::TomlError),
    #[error("credentials error: {0}")]
    Credentials(#[from] serde_yaml::Error),
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

#[cfg(test)]
mod model_routing_tests {
    use super::Config;

    /// A model a channel *advertises* but that its name does not reveal — the
    /// relay case in the wild: `[providers.openai]` lists `deepseek-flash`,
    /// because the whole point of a gateway is to serve other people's models.
    ///
    /// Name-prefix inference alone sends this to `deepseek`, which has no key,
    /// so every turn on that model fails with `NoApiKey`. `model_list` is the
    /// only evidence in the file that the user meant the openai channel, and
    /// before this curve it was ignored entirely by routing.
    #[test]
    fn advertised_model_routes_to_the_channel_that_advertises_it() {
        let raw = r#"
default_model = "deepseek-flash"
active_provider = "openai"

[providers.deepseek]
api_key = ""
base_url = "https://api.deepseek.com/v1"
enabled = true

[providers.openai]
api_key = "k"
base_url = "https://relay.example/v1"
enabled = true
model_list = ["deepseek-flash", "gpt-5.5"]
"#;
        let c: Config = toml::from_str(raw).expect("parse");
        assert_eq!(c.provider_key_for_model("deepseek-flash"), "openai");
        // A model that is genuinely DeepSeek's still goes to DeepSeek.
        let c2: Config = toml::from_str(&raw.replace("deepseek-v4-pro", "x")).unwrap();
        assert_eq!(
            c2.provider_key_for_model("deepseek-v4-chat"),
            "deepseek",
            "an unadvertised deepseek-* id must still infer to the deepseek channel"
        );
    }

    /// The published list must not outrank a disabled channel, or curation
    /// would resurrect a channel the user switched off.
    ///
    /// The probe is an id no prefix rule claims and that only the *disabled*
    /// channel advertises — so if the `enabled` filter were dropped, routing
    /// would hand it to a channel that cannot serve it.
    #[test]
    fn disabled_channel_does_not_win_an_advertised_model() {
        let raw = r#"
default_model = "deepseek-v4-pro"
active_provider = "deepseek"

[providers.deepseek]
api_key = "k"
base_url = "https://api.deepseek.com/v1"
enabled = true

[providers.openai]
api_key = "k"
base_url = "https://relay.example/v1"
enabled = false
model_list = ["mystery-model"]
"#;
        let c: Config = toml::from_str(raw).expect("parse");
        assert_eq!(c.provider_key_for_model("mystery-model"), "deepseek");
    }
}
