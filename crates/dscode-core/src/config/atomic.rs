//! Atomic file writes, and a private-mode variant for secret material.
//!
//! `std::fs::write` truncates the target in place, so a crash, a full disk or a
//! kill between truncate and write leaves the file empty or half-written. For a
//! config file that is a silent reset to defaults; for a credentials file it is
//! every key gone. Both write paths in this crate go through here instead.

use std::io::Write;
use std::path::Path;

/// Write `content` to `path` atomically: sibling temp file, fsync, rename.
///
/// The temp file is created in the same directory as the target so the rename
/// stays within one filesystem — which is what makes it atomic. On any failure
/// the original file is left untouched.
pub fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    write_atomic_inner(path, content, false)
}

/// `write_atomic`, restricted to the owner (0600) at creation time.
///
/// The mode is set on the temp file *before* the rename, so the target never
/// exists for even an instant with looser bits than intended — the reverse
/// order would leave a readable window. Windows has no POSIX mode; there the
/// call degrades to a plain atomic write and secrecy comes from the file living
/// under the user's own profile with default ACLs, plus the two rules in
/// `credentials.rs` (never hand the path to the agent, never export it into the
/// environment).
pub fn write_private_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    write_atomic_inner(path, content, true)
}

fn write_atomic_inner(path: &Path, content: &str, private: bool) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp = parent.join(format!(".{name}.dscode-{}.tmp", uuid::Uuid::new_v4()));

    let write = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut f = opts.open(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.toml");
        write_atomic(&p, "one").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one");
        write_atomic(&p, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
    }

    #[test]
    fn leaves_no_temp_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.toml");
        write_atomic(&p, "x").unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["a.toml".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn private_write_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secret.yaml");
        write_private_atomic(&p, "k: v").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credentials must not be group/world readable");
    }
}
