//! Model channels — the four provider configs and their shared defaults.
//!
//! Split out of `settings.rs` for one reason: the default base URLs below are
//! read by **three** places that must agree — Rust's `Default` impl, the desktop
//! settings page's placeholder text, and the "reset to defaults" action. When
//! they were string literals inside the `Default` impl there was nothing to
//! point the other two at. They are constants now so the settings page can be
//! handed them over the `get_config` contract rather than retyping them.

use serde::{Deserialize, Serialize};

/// Channel keys, in the order the settings page lists them.
///
/// The single source of truth for "which channels exist", in the same spirit as
/// [`ProviderConfigs::iter`] — a new channel should not require finding four
/// separate `match` arms and a `const` in the frontend.
pub const CHANNELS: [&str; 4] = ["deepseek", "openai", "anthropic", "ollama"];

pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/v1";
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434/v1";

/// Default base URL for a channel key, or `""` if the key is not one.
pub fn default_base_url(channel: &str) -> &'static str {
    match channel {
        "deepseek" => DEFAULT_DEEPSEEK_BASE_URL,
        "openai" => DEFAULT_OPENAI_BASE_URL,
        "anthropic" => DEFAULT_ANTHROPIC_BASE_URL,
        "ollama" => DEFAULT_OLLAMA_BASE_URL,
        _ => "",
    }
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

impl ProviderConfigs {
    /// Every channel, as `(channel key, config)`.
    ///
    /// The one place the four channel names are listed for iteration, so the
    /// credentials backfill and migration do not each grow their own `match`.
    /// Adding a channel means adding it here, to [`CHANNELS`], and to
    /// `Config::provider_config_by_key`.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &ProviderConfig)> {
        [
            ("deepseek", &self.deepseek),
            ("openai", &self.openai),
            ("anthropic", &self.anthropic),
            ("ollama", &self.ollama),
        ]
        .into_iter()
    }

    /// Mutable counterpart of [`Self::iter`].
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&'static str, &mut ProviderConfig)> {
        [
            ("deepseek", &mut self.deepseek),
            ("openai", &mut self.openai),
            ("anthropic", &mut self.anthropic),
            ("ollama", &mut self.ollama),
        ]
        .into_iter()
    }

    /// `(channel key, config)` for one channel, or `None` for an unknown key.
    ///
    /// Borrowed, unlike `Config::provider_config_by_key`, which clones because
    /// its callers need an owned config to hand to a provider constructor.
    pub fn get(&self, key: &str) -> Option<&ProviderConfig> {
        match key {
            "deepseek" => Some(&self.deepseek),
            "openai" => Some(&self.openai),
            "anthropic" => Some(&self.anthropic),
            "ollama" => Some(&self.ollama),
            _ => None,
        }
    }
}

impl Default for ProviderConfigs {
    fn default() -> Self {
        // `enabled` encodes which channels work out of the box: DeepSeek is the
        // one this app ships for, the other three are opt-in.
        Self {
            deepseek: ProviderConfig {
                base_url: DEFAULT_DEEPSEEK_BASE_URL.into(),
                enabled: true,
                ..Default::default()
            },
            openai: ProviderConfig {
                base_url: DEFAULT_OPENAI_BASE_URL.into(),
                ..Default::default()
            },
            anthropic: ProviderConfig {
                base_url: DEFAULT_ANTHROPIC_BASE_URL.into(),
                ..Default::default()
            },
            ollama: ProviderConfig {
                base_url: DEFAULT_OLLAMA_BASE_URL.into(),
                ..Default::default()
            },
        }
    }
}

/// Note: the `api_key` field does **not** mean what it used to. In memory and on
/// the wire it holds the live secret, backfilled by `Config::load` out of
/// `.credentials.yaml` — but it is stripped before `config.toml` is written (see
/// `strip_secrets` in `settings.rs`), so it never lands in that file.
///
/// It is deliberately **not** `#[serde(skip_serializing)]`. That attribute
/// applies to *every* serializer, not just the TOML one, so it would also drop
/// the field from the `get_config` JSON — and the settings page would then show
/// an empty key box for a channel that has a working key, which reads to the
/// user as "my key is gone". Stripping belongs at the TOML layer, where it is
/// targeted and testable.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    /// The channel's API key.
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
    /// API wire format for this channel: "" (empty) = OpenAI-compatible Chat
    /// Completions; "responses" = OpenAI Responses API (e.g. DeepSeek /responses).
    #[serde(default)]
    pub api_format: String,
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

pub(crate) fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_CHANNELS: [&str; 4] = CHANNELS;

    /// Every channel must be reachable through every accessor. These three
    /// lists (`CHANNELS`, `iter`, `get`) are hand-maintained and only useful if
    /// they agree — this is the test that catches a channel added to one and
    /// forgotten in another.
    #[test]
    fn every_accessor_agrees_on_the_channel_set() {
        let providers = ProviderConfigs::default();
        let from_iter: Vec<&str> = providers.iter().map(|(k, _)| k).collect();
        assert_eq!(from_iter, ALL_CHANNELS);
        for key in ALL_CHANNELS {
            assert!(providers.get(key).is_some(), "{key} missing from get()");
        }
        assert!(providers.get("nope").is_none());
    }

    /// A fresh install with no base_url typed in must still point somewhere
    /// real, or the first "fetch models" click reports an empty-URL error and
    /// looks like a bug in the app rather than a missing setting.
    #[test]
    fn defaults_carry_a_usable_base_url() {
        let p = ProviderConfigs::default();
        for (key, cfg) in p.iter() {
            assert_eq!(cfg.base_url, default_base_url(key), "{key}");
            assert!(!cfg.base_url.is_empty(), "{key} has no default base_url");
            assert!(cfg.api_key.is_empty(), "{key} must not ship a key");
        }
    }

    #[test]
    fn unknown_channel_has_no_default_url() {
        assert_eq!(default_base_url("nope"), "");
    }
}
