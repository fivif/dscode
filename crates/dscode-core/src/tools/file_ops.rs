//! File operation tools — read, write, and edit files within the working directory.
//!
//! All path arguments are resolved relative to the working directory in
//! `ToolContext`. Path traversal attempts (e.g. `../../etc/passwd`) are
//! blocked by canonicalizing and checking against the working directory root.

use async_trait::async_trait;
use std::path::{Path, PathBuf};

use crate::tools::trait_def::{Tool, ToolContext, ToolError, ToolResult};

/// Largest file `do_file_read` will materialize in one go (offset/limit windows
/// are bounded by the same budget).
const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Path resolution helpers
// ---------------------------------------------------------------------------

/// Windows reserves a handful of device names in *every* directory: opening
/// `C:\proj\NUL` succeeds and silently discards the bytes, so `do_file_write`
/// would answer "Wrote 42 bytes to C:\proj\NUL" for a file that does not exist
/// and cannot be read back. Reject such names before anything is attempted.
///
/// Windows also ignores trailing dots/spaces and resolves `NUL.txt` to the
/// device, hence the trimming and the stem comparison.
#[cfg(windows)]
fn is_reserved_device_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let trimmed = name.trim_end_matches(&[' ', '.'][..]);
    let stem = trimmed.split('.').next().unwrap_or(trimmed).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    // COM1..COM9 / LPT1..LPT9 — COM0/LPT0 are not devices.
    let bytes = stem.as_bytes();
    bytes.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && matches!(bytes[3], b'1'..=b'9')
}

/// Resolve `path` relative to `working_dir` and verify it stays within the
/// working directory boundary (no path-escape attacks).
///
/// T6: For non-existing files, symlinks in parent directories are resolved by
/// checking each ancestor path component with `canonicalize()`, then joining
/// with the non-existent filename.
fn resolve_path(path: &str, working_dir: &Path) -> Result<PathBuf, ToolError> {
    // Reject absolute paths that clearly leave workspace intent
    let path = path.trim();
    if path.is_empty() {
        return Err(ToolError::InvalidParameter {
            name: "path".into(),
            reason: "empty path".into(),
        });
    }

    // Use SafetyGuard path logic for consistent containment (incl. non-existent files)
    let guard = crate::safety::guard::SafetyGuard::new(&[], false);
    match guard.resolve_safe_path(path, working_dir) {
        Ok(p) => {
            #[cfg(windows)]
            {
                if is_reserved_device_name(&p) {
                    return Err(ToolError::InvalidParameter {
                        name: "path".into(),
                        reason: format!(
                            "'{}' is a reserved Windows device name — writes to it are \
                             accepted and discarded by the OS, so the file would never exist",
                            p.display()
                        ),
                    });
                }
            }
            // Extra: if path exists and is symlink, ensure target still under root
            if p.exists() {
                if let Ok(canon) = p.canonicalize() {
                    let wd = working_dir
                        .canonicalize()
                        .unwrap_or_else(|_| working_dir.to_path_buf());
                    if !canon.starts_with(&wd) {
                        return Err(ToolError::PathEscape(format!(
                            "Path '{}' resolves outside working directory '{}'",
                            path,
                            working_dir.display()
                        )));
                    }
                    return Ok(canon);
                }
            }
            Ok(p)
        }
        Err(e) => Err(ToolError::PathEscape(e)),
    }
}

/// Write `content` to `path` atomically.
///
/// `std::fs::write` truncates the target in place: a crash, a full disk, or the
/// process being killed between truncate and write leaves the file empty or
/// half-written — the data loss `do_file_edit`'s description promises cannot
/// happen. Instead write a sibling temp file, flush it to disk, then rename it
/// over the target. On any failure the original is untouched.
///
/// The same primitive backs `Config::save`, which has the identical problem with
/// a worse blast radius — a truncated config presents as "all my settings reset".
/// It lives in `config::atomic`; this is a re-export so tool code keeps its
/// shorter path.
use crate::config::atomic::write_atomic;

async fn check_write_allowed(ctx: &ToolContext, path_str: &str) -> Result<(), ToolError> {
    let (Some(agent_id), Some(fo)) = (&ctx.team_agent_id, &ctx.file_ownership) else {
        return Ok(());
    };
    let guard = fo.lock().await;
    match guard.check_write(agent_id, path_str, ctx.ownership_enforced) {
        crate::teams::ownership::PathAccess::Allowed => Ok(()),
        crate::teams::ownership::PathAccess::Denied {
            holder,
            path,
            reason,
        } => {
            let msg = format!(
                "path ownership denied for '{}': {reason} (holder={holder:?})",
                path.display()
            );
            if ctx.ownership_soft_log_only {
                tracing::warn!(%msg, "ownership soft deny");
                Ok(())
            } else {
                Err(ToolError::Internal(msg))
            }
        }
    }
}

async fn check_read_before_edit(ctx: &ToolContext, path_str: &str) -> Result<(), ToolError> {
    if !ctx.read_before_edit {
        return Ok(());
    }
    let Some(ref set) = ctx.read_paths else {
        return Ok(());
    };
    let g = set.lock().await;
    let ok = g.contains(path_str)
        || g.iter().any(|p| p.ends_with(path_str) || path_str.ends_with(p));
    if ok {
        Ok(())
    } else {
        Err(ToolError::EditError(format!(
            "read-before-edit: call do_file_read on '{path_str}' before writing/editing"
        )))
    }
}

/// Normalize a path by resolving `.` and `..` without filesystem access (tests + helpers).
#[cfg(test)]
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            c => components.push(c),
        }
    }
    components.iter().collect()
}

// ---------------------------------------------------------------------------
// do_file_read
// ---------------------------------------------------------------------------

/// Read a file's contents at the given path (relative to working directory).
pub struct DoFileRead;

impl DoFileRead {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DoFileRead {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DoFileRead {
    fn name(&self) -> &str {
        "do_file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file at the given path. \
         Path is resolved relative to the project working directory. \
         Returns the file content as a string. \
         Use offset/limit to page through a file that is too large to read at once."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the file to read, relative to the working directory."
                },
                "offset": {
                    "type": "integer",
                    "description": "Optional line number to start reading from (0-indexed)."
                },
                "limit": {
                    "type": "integer",
                    "description": "Optional maximum number of lines to read."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let path_str = args["path"]
            .as_str()
            .ok_or_else(|| ToolError::MissingParameter("path".into()))?;

        let file_path = resolve_path(path_str, &ctx.working_dir)?;

        if !file_path.exists() {
            return Err(ToolError::FileNotFound(format!(
                "File not found: {}",
                file_path.display()
            )));
        }

        if !file_path.is_file() {
            return Ok(ToolResult::err(
                format!("Not a file: {}", file_path.display()),
                "Path is not a regular file",
            ));
        }

        // Track read for read-before-edit
        if let Some(ref set) = ctx.read_paths {
            let mut g = set.lock().await;
            g.insert(path_str.to_string());
            g.insert(file_path.to_string_lossy().to_string());
        }

        // Apply offset/limit for large files
        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let limit = args["limit"].as_u64().map(|v| v as usize);

        let output = if offset > 0 || limit.is_some() {
            read_lines_windowed(&file_path, offset, limit)?
        } else {
            // Size guard: `read_to_string` materializes the whole file, and the
            // offset/limit branch used to build a second Vec of every line on
            // top of it. Refuse rather than pull a 4 GB log into memory.
            let size = std::fs::metadata(&file_path).map_err(ToolError::Io)?.len();
            if size > MAX_READ_BYTES {
                return Ok(ToolResult::err(
                    "",
                    format!(
                        "File is {size} bytes, over the {MAX_READ_BYTES}-byte read limit. \
                         Read it in windows with the offset/limit parameters \
                         (e.g. offset=0, limit=2000)."
                    ),
                ));
            }
            std::fs::read_to_string(&file_path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::InvalidData {
                    // The bare io error ("stream did not contain valid UTF-8")
                    // tells the model nothing about what to do next.
                    ToolError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "{} is not valid UTF-8 text (binary or UTF-16 file?)",
                            file_path.display()
                        ),
                    ))
                } else {
                    ToolError::Io(e)
                }
            })?
        };

        Ok(ToolResult::ok(output))
    }
}

/// Read the `limit` lines starting at `offset` without materializing the file.
///
/// `BufReader::lines()` only reads as far as it is asked to, so paging into a
/// multi-GB log stays cheap. The output is still bounded by `MAX_READ_BYTES`
/// (an `offset` with no `limit` runs to the end of the file).
fn read_lines_windowed(
    path: &Path,
    offset: usize,
    limit: Option<usize>,
) -> Result<String, ToolError> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).map_err(ToolError::Io)?;
    let reader = std::io::BufReader::new(file);

    let mut out = String::new();
    let mut pushed = 0usize;
    let mut truncated = false;
    for line in reader.lines().skip(offset).take(limit.unwrap_or(usize::MAX)) {
        let line = line.map_err(|e| {
            if e.kind() == std::io::ErrorKind::InvalidData {
                ToolError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "{} contains bytes that are not valid UTF-8 \
                         (binary or UTF-16 file?)",
                        path.display()
                    ),
                ))
            } else {
                ToolError::Io(e)
            }
        })?;
        if out.len() + line.len() + 1 > MAX_READ_BYTES as usize {
            truncated = true;
            break;
        }
        if pushed > 0 {
            out.push('\n');
        }
        out.push_str(&line);
        pushed += 1;
    }
    if truncated {
        out.push_str("\n[output truncated at 10MB]\n");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// do_file_write
// ---------------------------------------------------------------------------

/// Write content to a file at the given path (relative to working directory).
pub struct DoFileWrite;

impl DoFileWrite {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DoFileWrite {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DoFileWrite {
    fn name(&self) -> &str {
        "do_file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file at the given path. \
         Creates parent directories if they don't exist. \
         Path is resolved relative to the project working directory. \
         Overwrites the file if it already exists."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to write to, relative to the working directory."
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let path_str = args["path"]
            .as_str()
            .ok_or_else(|| ToolError::MissingParameter("path".into()))?;

        let content = args["content"]
            .as_str()
            .ok_or_else(|| ToolError::MissingParameter("content".into()))?;

        // Resolve the path (will normalize without requiring file to exist)
        let file_path = resolve_path(path_str, &ctx.working_dir)?;

        check_write_allowed(ctx, path_str).await?;
        check_read_before_edit(ctx, path_str).await?;

        // Create parent directories
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ToolError::Io(e)
            })?;
        }

        write_atomic(&file_path, content).map_err(ToolError::Io)?;

        Ok(ToolResult::ok(format!(
            "Wrote {} bytes to {}",
            content.len(),
            file_path.display()
        )))
    }
}

// ---------------------------------------------------------------------------
// do_file_edit
// ---------------------------------------------------------------------------

/// Perform exact string replacement in a file (old_string → new_string).
///
/// The `old_string` must be unique in the file — if it appears zero times or
/// more than once, the edit is rejected with a descriptive error.
pub struct DoFileEdit;

impl DoFileEdit {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DoFileEdit {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DoFileEdit {
    fn name(&self) -> &str {
        "do_file_edit"
    }

    fn description(&self) -> &str {
        "Perform exact string replacement in an existing file. \
         The old_string must appear exactly once in the file. \
         If it appears multiple times, include more surrounding context to \
         make it unique. The edit is atomic — the file is only modified if \
         the match is unambiguous."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to edit, relative to the working directory."
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to replace. Must appear exactly once in the file."
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace it with (must be different from old_string)."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace all occurrences of old_string instead of requiring uniqueness.",
                    "default": false
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let path_str = args["file_path"]
            .as_str()
            .ok_or_else(|| ToolError::MissingParameter("file_path".into()))?;

        let old_string = args["old_string"]
            .as_str()
            .ok_or_else(|| ToolError::MissingParameter("old_string".into()))?;

        let new_string = args["new_string"]
            .as_str()
            .ok_or_else(|| ToolError::MissingParameter("new_string".into()))?;

        check_write_allowed(ctx, path_str).await?;
        check_read_before_edit(ctx, path_str).await?;

        if old_string == new_string {
            return Ok(ToolResult::err(
                "",
                "old_string and new_string are identical — no change needed",
            ));
        }

        if old_string.is_empty() {
            return Ok(ToolResult::err(
                "",
                "old_string must not be empty",
            ));
        }

        let replace_all = args["replace_all"].as_bool().unwrap_or(false);

        let file_path = resolve_path(path_str, &ctx.working_dir)?;

        if !file_path.exists() {
            return Err(ToolError::FileNotFound(format!(
                "File not found: {}",
                file_path.display()
            )));
        }

        let original = std::fs::read_to_string(&file_path).map_err(|e| {
            ToolError::Io(e)
        })?;

        let occurrences = original.matches(old_string).count();

        if occurrences == 0 {
            return Err(ToolError::EditError(format!(
                "old_string was not found in the file. \
                 Verify the exact whitespace and indentation match the file content."
            )));
        }

        if !replace_all && occurrences > 1 {
            return Err(ToolError::EditError(format!(
                "old_string appears {} times in the file (must appear exactly once). \
                 Include more surrounding context to make it unique, or set \
                 replace_all to true.",
                occurrences
            )));
        }

        let modified = if replace_all {
            original.replace(old_string, new_string)
        } else {
            original.replacen(old_string, new_string, 1)
        };

        write_atomic(&file_path, &modified).map_err(ToolError::Io)?;

        let count = if replace_all { occurrences } else { 1 };
        Ok(ToolResult::ok(format!(
            "Successfully replaced {} occurrence(s) in {}",
            count,
            file_path.display()
        )))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    async fn make_ctx(dir: &std::path::Path) -> ToolContext {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        ToolContext::simple(
            dir.to_path_buf(),
            "test",
            "call_fops",
            tx,
            Arc::new(crate::safety::guard::SafetyGuard::new(&[], true)),
        )
    }

    // -- do_file_read --

    #[tokio::test]
    async fn test_read_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("hello.txt");
        std::fs::write(&file_path, "Hello, world!\n").unwrap();

        let tool = DoFileRead::new();
        let ctx = make_ctx(dir.path()).await;
        let result = tool
            .execute(serde_json::json!({ "path": "hello.txt" }), &ctx)
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "Hello, world!\n");
    }

    #[tokio::test]
    async fn test_read_file_with_offset_limit() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("lines.txt");
        std::fs::write(&file_path, "line0\nline1\nline2\nline3\nline4\n").unwrap();

        let tool = DoFileRead::new();
        let ctx = make_ctx(dir.path()).await;

        let result = tool
            .execute(
                serde_json::json!({ "path": "lines.txt", "offset": 1, "limit": 2 }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "line1\nline2");
    }

    #[tokio::test]
    async fn test_read_file_not_found() {
        let dir = tempdir().unwrap();
        let tool = DoFileRead::new();
        let ctx = make_ctx(dir.path()).await;
        let result = tool
            .execute(serde_json::json!({ "path": "nonexistent.txt" }), &ctx)
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::FileNotFound(_) => {}
            other => panic!("Expected FileNotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_read_path_escape_blocked() {
        let dir = tempdir().unwrap();
        let tool = DoFileRead::new();
        let ctx = make_ctx(dir.path()).await;
        let result = tool
            .execute(serde_json::json!({ "path": "../etc/passwd" }), &ctx)
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::PathEscape(_) => {}
            other => panic!("Expected PathEscape, got {:?}", other),
        }
    }

    // -- do_file_write --

    #[tokio::test]
    async fn test_write_file() {
        let dir = tempdir().unwrap();
        let tool = DoFileWrite::new();
        let ctx = make_ctx(dir.path()).await;

        let result = tool
            .execute(
                serde_json::json!({ "path": "output.txt", "content": "new content" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.success);
        let written = std::fs::read_to_string(dir.path().join("output.txt")).unwrap();
        assert_eq!(written, "new content");
    }

    #[tokio::test]
    async fn test_write_file_creates_parent_dir() {
        let dir = tempdir().unwrap();
        let tool = DoFileWrite::new();
        let ctx = make_ctx(dir.path()).await;

        let result = tool
            .execute(
                serde_json::json!({ "path": "sub/dir/deep/output.txt", "content": "deep" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.success);
        let written =
            std::fs::read_to_string(dir.path().join("sub/dir/deep/output.txt")).unwrap();
        assert_eq!(written, "deep");
    }

    #[tokio::test]
    async fn test_write_path_escape_blocked() {
        let dir = tempdir().unwrap();
        let tool = DoFileWrite::new();
        let ctx = make_ctx(dir.path()).await;

        let result = tool
            .execute(
                serde_json::json!({ "path": "../outside.txt", "content": "evil" }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::PathEscape(_) => {}
            other => panic!("Expected PathEscape, got {:?}", other),
        }
    }

    // -- do_file_edit --

    #[tokio::test]
    async fn test_edit_single_occurrence() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("src.rs");
        std::fs::write(&file_path, "fn main() {\n    let x = 1;\n}\n").unwrap();

        let tool = DoFileEdit::new();
        let ctx = make_ctx(dir.path()).await;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": "src.rs",
                    "old_string": "let x = 1;",
                    "new_string": "let x = 42;"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.success);
        let modified = std::fs::read_to_string(&file_path).unwrap();
        assert!(modified.contains("let x = 42;"));
        assert!(!modified.contains("let x = 1;"));
    }

    #[tokio::test]
    async fn test_edit_multiple_occurrences_rejected() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("dup.txt");
        std::fs::write(&file_path, "hello\nworld\nhello\n").unwrap();

        let tool = DoFileEdit::new();
        let ctx = make_ctx(dir.path()).await;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": "dup.txt",
                    "old_string": "hello",
                    "new_string": "bye"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::EditError(msg) => {
                assert!(msg.contains("appears 2 times"));
            }
            other => panic!("Expected EditError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_edit_replace_all() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("dup.txt");
        std::fs::write(&file_path, "hello\nworld\nhello\n").unwrap();

        let tool = DoFileEdit::new();
        let ctx = make_ctx(dir.path()).await;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": "dup.txt",
                    "old_string": "hello",
                    "new_string": "bye",
                    "replace_all": true
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.success);
        let modified = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(modified, "bye\nworld\nbye\n");
    }

    #[tokio::test]
    async fn test_edit_not_found() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("src.txt");
        std::fs::write(&file_path, "content").unwrap();

        let tool = DoFileEdit::new();
        let ctx = make_ctx(dir.path()).await;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": "src.txt",
                    "old_string": "does not exist",
                    "new_string": "irrelevant"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::EditError(msg) => {
                assert!(msg.contains("was not found"));
            }
            other => panic!("Expected EditError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_edit_identical_strings() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("src.txt");
        std::fs::write(&file_path, "same").unwrap();

        let tool = DoFileEdit::new();
        let ctx = make_ctx(dir.path()).await;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": "src.txt",
                    "old_string": "same",
                    "new_string": "same"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("identical"));
    }

    #[tokio::test]
    async fn test_normalize_path() {
        let normalized = normalize_path(Path::new("/foo/bar/../baz/./qux"));
        assert_eq!(normalized, PathBuf::from("/foo/baz/qux"));
    }

    /// The write must go through a sibling temp file, so no temp may survive a
    /// successful edit (and the content must still land).
    #[tokio::test]
    async fn test_edit_is_atomic_and_leaves_no_temp_files() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("src.txt");
        std::fs::write(&file_path, "alpha\nbeta\n").unwrap();

        let tool = DoFileEdit::new();
        let ctx = make_ctx(dir.path()).await;
        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": "src.txt",
                    "old_string": "beta",
                    "new_string": "gamma"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.success);

        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "alpha\ngamma\n");
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "src.txt")
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
    }

    /// Reserved device names are valid paths on Windows but discard whatever is
    /// written, so reporting success would be a lie.
    #[cfg(windows)]
    #[tokio::test]
    async fn test_reserved_device_names_are_rejected() {
        let dir = tempdir().unwrap();
        let ctx = make_ctx(dir.path()).await;

        for name in ["NUL", "nul", "con", "AUX", "COM1", "lpt9", "NUL.txt", "con."] {
            let result = DoFileWrite::new()
                .execute(serde_json::json!({ "path": name, "content": "x" }), &ctx)
                .await;
            assert!(
                matches!(&result, Err(ToolError::InvalidParameter { .. })),
                "{name} should be rejected, got {result:?}"
            );
        }

        // Ordinary names that merely share a prefix are unaffected.
        for name in ["console.txt", "null.md", "com0.txt"] {
            let result = DoFileWrite::new()
                .execute(serde_json::json!({ "path": name, "content": "x" }), &ctx)
                .await
                .unwrap();
            assert!(result.success, "{name} should be writable");
        }
    }

    /// `read_lines_windowed` must agree with the old offset/limit math.
    #[tokio::test]
    async fn test_read_offset_without_limit_reads_to_eof() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("lines.txt"), "a\nb\nc\nd\n").unwrap();

        let tool = DoFileRead::new();
        let ctx = make_ctx(dir.path()).await;
        let result = tool
            .execute(
                serde_json::json!({ "path": "lines.txt", "offset": 2 }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result.output, "c\nd");
    }
}
