//! SafetyGuard — runtime command and path safety checks.
//!
//! Enforces that:
//! - Shell commands do not match blocked regex patterns.
//! - File writes stay inside the project root (unless explicitly allowed).

use regex::Regex;
use std::path::{Component, Path, PathBuf};

/// Guards against dangerous commands and path-escaping writes.
#[derive(Debug, Clone)]
pub struct SafetyGuard {
    /// Compiled regex patterns for blocked commands.
    blocked_patterns: Vec<Regex>,
    /// If true, `validate_path` allows paths outside the project root.
    pub allow_write_outside_project: bool,
}

impl SafetyGuard {
    /// Build a new guard from a list of blocked command regex patterns.
    ///
    /// Invalid patterns are silently skipped.
    pub fn new(blocked_commands: &[String], allow_write_outside_project: bool) -> Self {
        let blocked_patterns = blocked_commands
            .iter()
            .filter_map(|pat| Regex::new(pat).ok())
            .collect();

        Self {
            blocked_patterns,
            allow_write_outside_project,
        }
    }

    /// Create a guard from the active configuration.
    pub fn from_config(config: &crate::config::settings::Config) -> Self {
        Self::new(
            &config.safety.blocked_commands,
            config.safety.allow_write_outside_project,
        )
    }

    /// Create a guard from the raw `SafetyConfig` struct.
    pub fn from_safety_config(config: &crate::config::settings::SafetyConfig) -> Self {
        Self::new(&config.blocked_commands, config.allow_write_outside_project)
    }

    // ── Command checking ──────────────────────────────────────────────────

    /// Check whether `cmd` matches any blocked pattern.
    ///
    /// Returns `Ok(())` if the command is safe, or `Err(msg)` describing
    /// which pattern was matched.
    pub fn check_command(&self, cmd: &str) -> Result<(), String> {
        for pat in &self.blocked_patterns {
            if pat.is_match(cmd) {
                return Err(format!(
                    "Blocked command: '{}' matches forbidden pattern '{}'",
                    cmd,
                    pat.as_str()
                ));
            }
        }
        Ok(())
    }

    // ── Path validation ───────────────────────────────────────────────────

    /// Validate that `path` (after canonicalization) is within `project_root`.
    ///
    /// If `allow_write_outside_project` is true, this always returns `Ok`.
    pub fn validate_path(&self, path: &Path, project_root: &Path) -> Result<(), String> {
        if self.allow_write_outside_project {
            return Ok(());
        }

        let canonical_root = project_root.canonicalize().map_err(|e| {
            format!("Failed to canonicalize project root '{}': {}", project_root.display(), e)
        })?;

        let canonical_path = path.canonicalize().map_err(|e| {
            format!("Failed to canonicalize path '{}': {}", path.display(), e)
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

    /// Resolve `path` relative to `project_root`, enforcing that the result
    /// does not escape the root (denying `..` traversal).
    ///
    /// If `allow_write_outside_project` is true, `path` is resolved normally
    /// (absolute paths accepted, relative resolved against project_root)
    /// without the ancestry check.
    pub fn resolve_safe_path(&self, path_str: &str, project_root: &Path) -> Result<PathBuf, String> {
        let path = Path::new(path_str);

        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_root.join(path)
        };

        // Normalize the path to catch '..' components early.
        let normalized = normalize_path(&resolved);

        if self.allow_write_outside_project {
            return Ok(normalized);
        }

        // Check that the normalized path doesn't escape the project root.
        let canonical_root = project_root.canonicalize().map_err(|e| {
            format!("Failed to canonicalize project root '{}': {}", project_root.display(), e)
        })?;

        // Try canonicalizing the normalized path.
        match normalized.canonicalize() {
            Ok(canon) => {
                if !canon.starts_with(&canonical_root) {
                    return Err(format!(
                        "Path '{}' resolves outside the project root",
                        path_str
                    ));
                }
                Ok(canon)
            }
            Err(_) => {
                // Path doesn't exist on disk yet — find the longest existing
                // ancestor, canonicalize it, then check containment.
                let existing_ancestor = find_existing_ancestor(&normalized);
                let canon_ancestor = existing_ancestor.canonicalize().map_err(|e| {
                    format!("Failed to canonicalize ancestor '{}': {}", existing_ancestor.display(), e)
                })?;

                if !canon_ancestor.starts_with(&canonical_root) {
                    return Err(format!(
                        "Path '{}' would escape the project root",
                        path_str
                    ));
                }

                // Reconstruct: canonical_ancestor + relative tail of normalized.
                let tail = normalized
                    .strip_prefix(&existing_ancestor)
                    .map_err(|e| format!("Path strip error: {}", e))?;
                Ok(canon_ancestor.join(tail))
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Walk up from `path` to find the longest ancestor that exists on disk.
fn find_existing_ancestor(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    while !current.exists() {
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            // Reached filesystem root — fallback to "."
            return PathBuf::from(".");
        }
    }
    current
}

/// Normalize a path by collapsing `.` and `..` components.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => { /* skip */ }
            Component::ParentDir => {
                match components.last() {
                    None => components.push(comp),
                    Some(Component::RootDir) | Some(Component::Prefix(_)) => {
                        // Cannot go above the root — `..` from root is root.
                    }
                    Some(Component::ParentDir) => {
                        components.push(comp);
                    }
                    Some(_) => {
                        components.pop();
                    }
                }
            }
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
    use std::fs;

    #[test]
    fn test_check_command_blocks_dangerous() {
        let guard = SafetyGuard::new(
            &["rm -rf /".into(), "mkfs\\.".into()],
            false,
        );
        assert!(guard.check_command("rm -rf / --no-preserve-root").is_err());
        assert!(guard.check_command("mkfs.ext4 /dev/sda").is_err());
        assert!(guard.check_command("echo hello").is_ok());
    }

    #[test]
    fn test_check_command_allow_harmless() {
        let guard = SafetyGuard::new(&["rm -rf /".into()], false);
        assert!(guard.check_command("ls -la").is_ok());
        assert!(guard.check_command("cargo build").is_ok());
        assert!(guard.check_command("rm somefile.txt").is_ok());
    }

    #[test]
    fn test_validate_path_inside_project() {
        let tmp = tempfile::tempdir().unwrap();
        let guard = SafetyGuard::new(&[], false);

        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();

        assert!(guard.validate_path(&sub, tmp.path()).is_ok());
    }

    #[test]
    fn test_validate_path_outside_project() {
        let tmp = tempfile::tempdir().unwrap();
        let guard = SafetyGuard::new(&[], false);

        let outside = std::env::temp_dir();
        assert!(guard.validate_path(&outside, tmp.path()).is_err());
    }

    #[test]
    fn test_validate_path_allow_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let guard = SafetyGuard::new(&[], true);

        let outside = std::env::temp_dir();
        assert!(guard.validate_path(&outside, tmp.path()).is_ok());
    }

    #[test]
    fn test_resolve_safe_path_normal() {
        let tmp = tempfile::tempdir().unwrap();
        let guard = SafetyGuard::new(&[], false);

        let result = guard.resolve_safe_path("foo/bar.txt", tmp.path()).unwrap();
        // The result should start with the canonical tmp path (which resolves symlinks).
        let canonical_tmp = tmp.path().canonicalize().unwrap();
        assert!(result.starts_with(&canonical_tmp));
        assert!(result.ends_with("foo/bar.txt"));
    }

    #[test]
    fn test_resolve_safe_path_traversal_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let guard = SafetyGuard::new(&[], false);

        let result = guard.resolve_safe_path("../etc/passwd", tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_normalize_path_dots() {
        let p = Path::new("/a/b/../c/./d");
        assert_eq!(normalize_path(p), PathBuf::from("/a/c/d"));
    }

    #[test]
    fn test_normalize_path_traversal() {
        let p = Path::new("/a/../../etc");
        assert_eq!(normalize_path(p), PathBuf::from("/etc"));
    }
}
