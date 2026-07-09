//! SafetyGuard — command risk classification and path containment.

use regex::Regex;
use std::path::{Component, Path, PathBuf};
use tracing::warn;

/// Result of classifying a shell command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandRisk {
    /// Safe to run without prompting.
    Allow,
    /// High-risk: require user confirmation unless absolute_trust.
    Confirm { reason: String },
    /// Never allowed, even in absolute trust.
    HardBlock { reason: String },
}

/// Guards against dangerous commands and path-escaping writes.
#[derive(Debug, Clone)]
pub struct SafetyGuard {
    blocked_patterns: Vec<Regex>,
    pub allow_write_outside_project: bool,
    /// When true, Confirm-level commands run without UI prompt.
    /// HardBlock still always denied.
    pub absolute_trust: bool,
}

impl SafetyGuard {
    pub fn new(blocked_commands: &[String], allow_write_outside_project: bool) -> Self {
        Self::with_trust(blocked_commands, allow_write_outside_project, false)
    }

    pub fn with_trust(
        blocked_commands: &[String],
        allow_write_outside_project: bool,
        absolute_trust: bool,
    ) -> Self {
        let blocked_patterns = blocked_commands
            .iter()
            .filter_map(|pat| {
                let bounded = format!(r"\b{}\b", pat);
                match Regex::new(&bounded) {
                    Ok(re) => Some(re),
                    Err(e) => {
                        warn!(
                            pattern = %pat,
                            error = %e,
                            "SafetyGuard: skipping invalid blocked_command regex pattern"
                        );
                        None
                    }
                }
            })
            .collect();

        Self {
            blocked_patterns,
            allow_write_outside_project,
            absolute_trust,
        }
    }

    pub fn from_config(config: &crate::config::settings::Config) -> Self {
        Self::with_trust(
            &config.safety.blocked_commands,
            config.safety.allow_write_outside_project,
            config.safety.absolute_trust,
        )
    }

    pub fn from_safety_config(config: &crate::config::settings::SafetyConfig) -> Self {
        Self::with_trust(
            &config.blocked_commands,
            config.allow_write_outside_project,
            config.absolute_trust,
        )
    }

    /// Classify command risk (hard / confirm / allow).
    pub fn classify_command(&self, cmd: &str) -> CommandRisk {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return CommandRisk::Allow;
        }

        // ── Hard blocks (never allowed) ──────────────────────────────────
        const HARD: &[(&str, &str)] = &[
            (
                r"rm\s+(-[a-zA-Z]*f[a-zA-Z]*\s+|--force\s+)*(/|/\*|~|--no-preserve-root)(\s|$)",
                "destructive rm targeting root/home",
            ),
            (r"mkfs\.", "filesystem format"),
            (r"dd\s+if=", "raw disk dd"),
            (
                r":\s*\(\s*\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;\s*:",
                "fork bomb",
            ),
            (r"chmod\s+-R\s+777\s+/", "chmod -R 777 on root"),
            (r">\s*/dev/sd[a-z]", "overwrite block device"),
            (r">\s*/dev/hd[a-z]", "overwrite block device"),
        ];
        for (pat, reason) in HARD {
            if let Ok(re) = Regex::new(pat) {
                if re.is_match(cmd) {
                    return CommandRisk::HardBlock {
                        reason: reason.to_string(),
                    };
                }
            }
        }
        // compact form
        let compact: String = cmd.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains(">/dev/sd") || compact.contains(">/dev/hd") {
            return CommandRisk::HardBlock {
                reason: "overwrite block device".into(),
            };
        }

        // ── Confirm-level (prompt unless absolute_trust) ─────────────────
        const CONFIRM: &[(&str, &str)] = &[
            (r"\bsudo\b", "elevated privileges (sudo)"),
            (r"\brm\s+(-[a-zA-Z]*r[a-zA-Z]*|[a-zA-Z-]*rf)", "recursive/force delete"),
            (r"\brm\s+-rf\b", "force recursive delete"),
            (r"curl\s+.*\|\s*(ba)?sh", "pipe remote script to shell"),
            (r"wget\s+.*\|\s*(ba)?sh", "pipe remote script to shell"),
            (r"\beval\b", "eval of dynamic code"),
            (r"base64\s+(-d|--decode)", "base64 decode (possible obfuscation)"),
            (r"\bchmod\s+-R\b", "recursive chmod"),
            (r"\bchown\s+-R\b", "recursive chown"),
            (r"docker\s+system\s+prune", "docker prune"),
            (r"\bmkfs\b", "filesystem tools"),
            (r"\bshutdown\b|\breboot\b|\bpoweroff\b", "system power control"),
            (r"\bkill\s+-9\s+-1\b", "kill all processes"),
            (r">\s*/etc/", "write under /etc"),
            (r"\bdd\b", "dd utility"),
        ];
        for (pat, reason) in CONFIRM {
            if let Ok(re) = Regex::new(pat) {
                if re.is_match(cmd) {
                    return CommandRisk::Confirm {
                        reason: reason.to_string(),
                    };
                }
            }
        }

        // User-configured blocked patterns → treat as hard block
        for pat in &self.blocked_patterns {
            if pat.is_match(cmd) {
                return CommandRisk::HardBlock {
                    reason: format!("matches configured block '{}'", pat.as_str()),
                };
            }
        }

        CommandRisk::Allow
    }

    /// Legacy API: hard-block only (used by old call sites). Prefer `classify_command`.
    pub fn check_command(&self, cmd: &str) -> Result<(), String> {
        match self.classify_command(cmd) {
            CommandRisk::HardBlock { reason } => Err(format!(
                "Blocked command: '{cmd}' ({reason})"
            )),
            CommandRisk::Confirm { .. } | CommandRisk::Allow => Ok(()),
        }
    }

    /// Full gate: hard block, or confirm unless absolute_trust.
    /// Returns Err message if must not run; Ok if may proceed (caller still
    /// runs interactive confirm when Confirm && !absolute_trust).
    pub fn must_block(&self, cmd: &str) -> Result<(), String> {
        match self.classify_command(cmd) {
            CommandRisk::HardBlock { reason } => Err(format!(
                "Blocked (hard): {reason} — never allowed, even in absolute trust"
            )),
            CommandRisk::Confirm { reason: _ } if self.absolute_trust => Ok(()),
            CommandRisk::Confirm { reason } => {
                // Signal confirm needed via special prefix for tools without hub
                Err(format!("CONFIRM_REQUIRED:{reason}"))
            }
            CommandRisk::Allow => Ok(()),
        }
    }

    pub fn needs_confirm(&self, cmd: &str) -> Option<String> {
        if self.absolute_trust {
            return None;
        }
        match self.classify_command(cmd) {
            CommandRisk::Confirm { reason } => Some(reason),
            _ => None,
        }
    }

    // ── Path validation ───────────────────────────────────────────────────

    pub fn validate_path(&self, path: &Path, project_root: &Path) -> Result<(), String> {
        if self.allow_write_outside_project {
            return Ok(());
        }

        let canonical_root = project_root.canonicalize().map_err(|e| {
            format!(
                "Failed to canonicalize project root '{}': {}",
                project_root.display(),
                e
            )
        })?;

        let canonical_path = path.canonicalize().map_err(|e| {
            format!(
                "Failed to canonicalize path '{}': {}",
                path.display(),
                e
            )
        })?;

        if !canonical_path.starts_with(&canonical_root) {
            return Err(format!(
                "Path '{}' is outside the project root '{}'",
                path.display(),
                project_root.display()
            ));
        }

        Ok(())
    }

    pub fn resolve_safe_path(&self, path_str: &str, project_root: &Path) -> Result<PathBuf, String> {
        let path = Path::new(path_str);

        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_root.join(path)
        };

        let normalized = normalize_path(&resolved);

        if self.allow_write_outside_project {
            return Ok(normalized);
        }

        let canonical_root = project_root.canonicalize().map_err(|e| {
            format!(
                "Failed to canonicalize project root '{}': {}",
                project_root.display(),
                e
            )
        })?;

        match normalized.canonicalize() {
            Ok(canon) => {
                if !canon.starts_with(&canonical_root) {
                    return Err(format!(
                        "Path '{}' resolves outside the project root",
                        path_str
                    ));
                }
                // Reject if any symlink in the chain left the root (canonicalize already resolved)
                Ok(canon)
            }
            Err(_) => {
                let existing_ancestor = find_existing_ancestor(&normalized, project_root);
                let canon_ancestor = existing_ancestor.canonicalize().map_err(|e| {
                    format!(
                        "Failed to canonicalize ancestor '{}': {}",
                        existing_ancestor.display(),
                        e
                    )
                })?;

                if !canon_ancestor.starts_with(&canonical_root) {
                    return Err(format!(
                        "Path '{}' would escape the project root",
                        path_str
                    ));
                }

                let tail = normalized
                    .strip_prefix(&existing_ancestor)
                    .map_err(|e| format!("Path strip error: {}", e))?;
                // Ensure tail has no ".." after strip
                let joined = normalize_path(&canon_ancestor.join(tail));
                if !path_is_under(&joined, &canonical_root) {
                    return Err(format!(
                        "Path '{}' would escape the project root",
                        path_str
                    ));
                }
                Ok(joined)
            }
        }
    }
}

fn path_is_under(path: &Path, root: &Path) -> bool {
    let mut pi = path.components();
    for rc in root.components() {
        match pi.next() {
            Some(c) if c == rc => {}
            _ => return false,
        }
    }
    true
}

fn find_existing_ancestor(path: &Path, project_root: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    while !current.exists() {
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            return project_root.to_path_buf();
        }
    }
    current
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match components.last() {
                None => components.push(comp),
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                Some(Component::ParentDir) => components.push(comp),
                Some(_) => {
                    components.pop();
                }
            },
            other => components.push(other),
        }
    }

    if components.is_empty() {
        return PathBuf::from(".");
    }

    let mut result = PathBuf::new();
    for c in components {
        result.push(c.as_os_str());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_blocks_root_rm() {
        let g = SafetyGuard::new(&[], false);
        assert!(matches!(
            g.classify_command("rm -rf /"),
            CommandRisk::HardBlock { .. }
        ));
        assert!(matches!(
            g.classify_command("rm -rf / --no-preserve-root"),
            CommandRisk::HardBlock { .. }
        ));
    }

    #[test]
    fn confirm_sudo_and_rm_rf_dir() {
        let g = SafetyGuard::new(&[], false);
        assert!(matches!(
            g.classify_command("sudo apt install x"),
            CommandRisk::Confirm { .. }
        ));
        assert!(matches!(
            g.classify_command("rm -rf ./build"),
            CommandRisk::Confirm { .. }
        ));
    }

    #[test]
    fn allow_harmless() {
        let g = SafetyGuard::new(&[], false);
        assert_eq!(g.classify_command("ls -la"), CommandRisk::Allow);
        assert_eq!(g.classify_command("cargo test"), CommandRisk::Allow);
    }

    #[test]
    fn absolute_trust_skips_confirm_not_hard() {
        let g = SafetyGuard::with_trust(&[], false, true);
        assert!(g.needs_confirm("rm -rf ./foo").is_none());
        assert!(matches!(
            g.classify_command("rm -rf /"),
            CommandRisk::HardBlock { .. }
        ));
    }

    #[test]
    fn test_check_command_blocks_dangerous() {
        let guard = SafetyGuard::new(&["rm -rf /".into(), "mkfs\\.".into()], false);
        assert!(guard.check_command("rm -rf / --no-preserve-root").is_err());
        assert!(guard.check_command("mkfs.ext4 /dev/sda").is_err());
        assert!(guard.check_command("echo hello").is_ok());
    }

    #[test]
    fn test_check_command_allow_harmless() {
        let guard = SafetyGuard::new(&["rm -rf /".into()], false);
        assert!(guard.check_command("ls -la").is_ok());
        assert!(guard.check_command("cargo build").is_ok());
    }

    #[test]
    fn path_stays_in_root() {
        let dir = std::env::temp_dir().join(format!("dscode-safe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let g = SafetyGuard::new(&[], false);
        let ok = g.resolve_safe_path("sub/a.txt", &dir).unwrap();
        assert!(ok.starts_with(dir.canonicalize().unwrap()) || path_is_under(&ok, &dir.canonicalize().unwrap()));
        assert!(g.resolve_safe_path("../outside", &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
