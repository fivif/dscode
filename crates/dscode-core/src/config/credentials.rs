//! Credentials — the secret half of the configuration, kept out of `config.toml`.
//!
//! ## What this is, and is not
//!
//! API keys used to be `ProviderConfig.api_key`, i.e. a plain string in the same
//! world-readable TOML file as the rest of the settings. This module moves them
//! into `~/.dscode/.credentials.yaml` and adds three rules. **It is isolation by
//! compartment, not encryption**, and the distinction matters enough to state
//! plainly: it matches what DeepSeek Harness does (MIT) — dsh keeps
//! `.credentials.yaml` at 0600 and relies on file permissions rather than a
//! cipher, with no keyring. Anyone who can read the user's home directory as
//! that user can read these keys. What the split buys is that the secret is not
//! carried along by every other operation that touches config:
//!
//!   1. **File permissions.** The file is created 0600 on Unix (`atomic::
//!      write_private_atomic`). Windows has no mode bit; there secrecy rests on
//!      the file sitting in the user's own profile under default ACLs.
//!   2. **The path is never handed to the agent.** Nothing in the tool layer
//!      exposes `credentials_path()`, so `do_file_read` cannot reach it through
//!      a path the model knows about. This is the rule that actually does the
//!      work, and it is why the constant is private to this module with only an
//!      explicit accessor beside it — adding a call site should be a decision,
//!      not an accident.
//!   3. **Never materialised into the environment.** Keys are read at call time
//!      and put on the HTTP request, never exported to a child process, never
//!      set as an env var. A key in the environment is inherited by every
//!      command the agent runs; `bash.rs` forwarding the whole environment to a
//!      shell is exactly the leak this avoids.
//!
//! ## Fallback
//!
//! A `api_key` still present in `config.toml` (an existing install that has not
//! saved since the upgrade) is honoured on read and migrated out on the first
//! successful save. See `Config::load` / `Config::save` in `settings.rs`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use super::atomic;
use super::settings::ConfigError;

/// Data directory name under the user's home.
const DATA_DIR: &str = ".dscode";
/// The credentials file. Kept private: see the module docs, rule 2.
const CREDENTIALS_FILE: &str = ".credentials.yaml";

/// Provider key → secret. A flat map rather than a typed struct so that adding
/// a channel is one word here rather than a struct edit plus a serde default,
/// and so unknown channels written by a future build round-trip untouched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(default)]
    pub providers: HashMap<String, String>,
}

impl Credentials {
    /// The key for `channel`, or `""` when unset. Trimmed, so a stray newline
    /// pasted into the settings field does not become a 401.
    pub fn get(&self, channel: &str) -> &str {
        self.providers
            .get(channel)
            .map(|s| s.trim())
            .unwrap_or("")
    }

    /// Store `key` for `channel`. An empty key removes the entry rather than
    /// writing an empty string, so the file reflects "not configured".
    pub fn set(&mut self, channel: &str, key: &str) {
        let key = key.trim();
        if key.is_empty() {
            self.providers.remove(channel);
        } else {
            self.providers
                .insert(channel.to_string(), key.to_string());
        }
    }

    /// Load from disk. A missing file is not an error — it is the normal state
    /// of a fresh install, and of every install until the first save.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        // An empty file parses as nothing; serde_yaml would error on it, so
        // treat whitespace-only as "no credentials" rather than a hard failure
        // that would make the app unusable until the user deleted a file they
        // cannot see.
        if content.trim().is_empty() {
            return Ok(Self::default());
        }
        Ok(serde_yaml::from_str(&content)?)
    }

    /// Write to disk atomically, at 0600 on Unix.
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_yaml::to_string(self)?;
        atomic::write_private_atomic(&path, &content)?;
        Ok(())
    }

    /// `~/.dscode/.credentials.yaml`.
    ///
    /// Public because the settings layer needs it to migrate, but deliberately
    /// **not** surfaced to the tool layer: nothing that builds an agent-visible
    /// path should be able to name this file.
    pub fn path() -> Result<PathBuf, ConfigError> {
        let home = super::settings::home_dir().ok_or(ConfigError::NoHomeDir)?;
        Ok(home.join(DATA_DIR).join(CREDENTIALS_FILE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_key_removes_the_entry() {
        let mut c = Credentials::default();
        c.set("deepseek", "sk-a");
        assert_eq!(c.get("deepseek"), "sk-a");
        c.set("deepseek", "   ");
        assert_eq!(c.get("deepseek"), "");
        assert!(!c.providers.contains_key("deepseek"));
    }

    #[test]
    fn keys_are_trimmed_on_read_and_write() {
        let mut c = Credentials::default();
        c.set("openai", "  sk-b\n");
        assert_eq!(c.get("openai"), "sk-b");
    }

    #[test]
    fn roundtrips_through_yaml() {
        let mut c = Credentials::default();
        c.set("deepseek", "sk-a");
        c.set("anthropic", "sk-c");
        let text = serde_yaml::to_string(&c).unwrap();
        let back: Credentials = serde_yaml::from_str(&text).unwrap();
        assert_eq!(back.get("deepseek"), "sk-a");
        assert_eq!(back.get("anthropic"), "sk-c");
    }

    #[test]
    fn unknown_channel_roundtrips() {
        // A channel a future build adds must not be lost by this one.
        let text = "providers:\n  deepseek: sk-a\n  some_new_channel: sk-z\n";
        let c: Credentials = serde_yaml::from_str(text).unwrap();
        let back = serde_yaml::to_string(&c).unwrap();
        assert!(back.contains("some_new_channel: sk-z"), "{back}");
    }
}
