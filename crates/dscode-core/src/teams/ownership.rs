//! File path ownership for parallel implement agents (K18).

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// Result of a write-path access check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathAccess {
    Allowed,
    Denied {
        holder: Option<String>,
        path: PathBuf,
        reason: &'static str,
    },
}

/// Pre-registered path exclusivity for implement agents.
///
/// Empty reserved set for an agent ⇒ unrestricted writes (K18).
#[derive(Debug, Default, Clone)]
pub struct FileOwnership {
    /// normalized rel path → agent_id
    owners: HashMap<PathBuf, String>,
    /// agent_id → reserved paths (empty set = unrestricted)
    reserved: HashMap<String, HashSet<PathBuf>>,
}

impl FileOwnership {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register owned_paths for agent. Empty paths ⇒ unrestricted marker only.
    /// Conflict with another agent on a non-empty path → Err.
    pub fn reserve(
        &mut self,
        agent_id: &str,
        paths: &[String],
    ) -> Result<(), PathAccess> {
        let normalized: Vec<PathBuf> = paths
            .iter()
            .filter_map(|p| normalize_rel(p).ok())
            .collect();

        if normalized.is_empty() {
            self.reserved.insert(agent_id.to_string(), HashSet::new());
            return Ok(());
        }

        for p in &normalized {
            if let Some(holder) = self.owners.get(p) {
                if holder != agent_id {
                    return Err(PathAccess::Denied {
                        holder: Some(holder.clone()),
                        path: p.clone(),
                        reason: "path already reserved by another agent",
                    });
                }
            }
        }

        let mut set = HashSet::new();
        for p in normalized {
            self.owners.insert(p.clone(), agent_id.to_string());
            set.insert(p);
        }
        self.reserved.insert(agent_id.to_string(), set);
        Ok(())
    }

    pub fn release(&mut self, agent_id: &str) {
        if let Some(paths) = self.reserved.remove(agent_id) {
            for p in paths {
                if self.owners.get(&p).map(|h| h == agent_id).unwrap_or(false) {
                    self.owners.remove(&p);
                }
            }
        }
    }

    /// Check write access.
    ///
    /// - `!enforced` → always Allowed  
    /// - enforced + empty reserved for agent → Allowed (unrestricted)  
    /// - enforced + non-empty reserved → path must be in set and owner==self  
    pub fn check_write(&self, agent_id: &str, path: &str, enforced: bool) -> PathAccess {
        if !enforced {
            return PathAccess::Allowed;
        }
        let Ok(norm) = normalize_rel(path) else {
            return PathAccess::Denied {
                holder: None,
                path: PathBuf::from(path),
                reason: "invalid path",
            };
        };

        let reserved = match self.reserved.get(agent_id) {
            Some(r) => r,
            None => {
                // Agent never reserved — treat as unrestricted for safety of non-teams tools
                return PathAccess::Allowed;
            }
        };

        if reserved.is_empty() {
            return PathAccess::Allowed;
        }

        if reserved.contains(&norm) {
            return PathAccess::Allowed;
        }

        // path not in our set
        let holder = self.owners.get(&norm).cloned();
        PathAccess::Denied {
            holder,
            path: norm,
            reason: "path outside agent owned_paths",
        }
    }
}

/// Normalize relative path: reject `..` escape, strip leading `./`.
pub fn normalize_rel(path: &str) -> Result<PathBuf, ()> {
    let path = path.trim().trim_start_matches("./");
    if path.is_empty() {
        return Err(());
    }
    if Path::new(path).is_absolute() {
        // Allow absolute only if we later map under wd — for ownership keys use as-is stripped
        // Prefer relative keys: strip common roots later in check; for reserve, reject abs
        return Err(());
    }
    let mut out = PathBuf::new();
    for c in Path::new(path).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => return Err(()),
            Component::Normal(s) => out.push(s),
            Component::RootDir | Component::Prefix(_) => return Err(()),
        }
    }
    if out.as_os_str().is_empty() {
        return Err(());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_owned_paths_unrestricted() {
        let mut fo = FileOwnership::new();
        fo.reserve("a", &[]).unwrap();
        assert_eq!(
            fo.check_write("a", "src/main.rs", true),
            PathAccess::Allowed
        );
    }

    #[test]
    fn non_empty_blocks_outside() {
        let mut fo = FileOwnership::new();
        fo.reserve("a", &["src/a.rs".into()]).unwrap();
        assert_eq!(
            fo.check_write("a", "src/a.rs", true),
            PathAccess::Allowed
        );
        assert!(matches!(
            fo.check_write("a", "src/b.rs", true),
            PathAccess::Denied { .. }
        ));
    }

    #[test]
    fn conflict_on_reserve() {
        let mut fo = FileOwnership::new();
        fo.reserve("a", &["src/x.rs".into()]).unwrap();
        assert!(fo.reserve("b", &["src/x.rs".into()]).is_err());
    }

    #[test]
    fn not_enforced_always_ok() {
        let mut fo = FileOwnership::new();
        fo.reserve("a", &["src/a.rs".into()]).unwrap();
        assert_eq!(
            fo.check_write("a", "src/b.rs", false),
            PathAccess::Allowed
        );
    }
}
