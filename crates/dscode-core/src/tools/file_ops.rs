//! File operation tools — read, write, and edit files within the working directory.
//!
//! All path arguments are resolved relative to the working directory in
//! `ToolContext`. Path traversal attempts (e.g. `../../etc/passwd`) are
//! blocked by canonicalizing and checking against the working directory root.

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::tools::trait_def::{Tool, ToolContext, ToolError, ToolResult};

// ---------------------------------------------------------------------------
// Path resolution helpers
// ---------------------------------------------------------------------------

/// Resolve `path` relative to `working_dir` and verify it stays within the
/// working directory boundary (no path-escape attacks).
///
/// T6: For non-existing files, symlinks in parent directories are resolved by
/// checking each ancestor path component with `canonicalize()`, then joining
/// with the non-existent filename.
fn resolve_path(path: &str, working_dir: &Path) -> Result<PathBuf, ToolError> {
    // Resolve relative to the working directory
    let candidate = working_dir.join(path);

    // Canonicalize to resolve symlinks and `..` components.
    let canonical = match candidate.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // T6: Path doesn't exist — resolve symlinks in parent directories first.
            // Walk up from the candidate path finding the nearest existing ancestor,
            // canonicalize it, then join the remaining non-existent components.
            let mut existing_ancestor: Option<PathBuf> = None;
            let mut remaining: Vec<std::path::Component<'_>> = Vec::new();

            for ancestor in candidate.ancestors() {
                if let Ok(canon) = ancestor.canonicalize() {
                    existing_ancestor = Some(canon);
                    break;
                }
                // Prepend components as we walk up
                let comp = ancestor
                    .file_name()
                    .map(|_| ancestor.components().last())
                    .flatten();
                if let Some(c) = comp {
                    remaining.insert(0, c);
                }
            }

            let resolved = if let Some(ancestor) = existing_ancestor {
                let mut result = ancestor;
                for comp in remaining {
                    result.push(comp.as_os_str());
                }
                result
            } else {
                // No existing ancestor found — fall back to manual normalization
                normalize_path(&candidate)
            };

            // Verify the resolved path stays within the working directory
            let wd_canonical = working_dir.canonicalize().unwrap_or_else(|_| working_dir.to_path_buf());
            if !resolved.starts_with(&wd_canonical) {
                return Err(ToolError::PathEscape(format!(
                    "Path '{}' resolves outside working directory '{}'",
                    path,
                    working_dir.display()
                )));
            }
            return Ok(resolved);
        }
    };

    // Existing path — make sure it's within working_dir
    let wd_canonical = working_dir.canonicalize().unwrap_or_else(|_| working_dir.to_path_buf());
    if !canonical.starts_with(&wd_canonical) {
        return Err(ToolError::PathEscape(format!(
            "Path '{}' resolves outside working directory '{}'",
            path,
            working_dir.display()
        )));
    }

    Ok(canonical)
}

/// Normalize a path by resolving `.` and `..` components without touching the
/// filesystem.
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
         Supports reading multiple files by passing an array of paths."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
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

        let content = std::fs::read_to_string(&file_path).map_err(|e| {
            ToolError::Io(e)
        })?;

        // Apply offset/limit for large files
        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let limit = args["limit"].as_u64().map(|v| v as usize);

        let output = if offset > 0 || limit.is_some() {
            let lines: Vec<&str> = content.lines().collect();
            let start = offset.min(lines.len());
            let end = limit
                .map(|l| (start + l).min(lines.len()))
                .unwrap_or(lines.len());
            lines[start..end].join("\n")
        } else {
            content
        };

        Ok(ToolResult::ok(output))
    }
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

        // Create parent directories
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ToolError::Io(e)
            })?;
        }

        std::fs::write(&file_path, content).map_err(|e| {
            ToolError::Io(e)
        })?;

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

        std::fs::write(&file_path, &modified).map_err(|e| {
            ToolError::Io(e)
        })?;

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
    use tempfile::tempdir;

    async fn make_ctx(
        dir: &std::path::Path,
    ) -> ToolContext {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        ToolContext {
            working_dir: dir.to_path_buf(),
            session_id: "test".into(),
            tool_call_id: "call_fops".into(),
            sender: tx,
            safety_guard: Arc::new(crate::safety::guard::SafetyGuard::new(&[], true)),
        }
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
}
