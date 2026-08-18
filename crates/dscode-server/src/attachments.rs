//! Stage user-selected files into the session workspace and build agent context.

use std::path::{Path, PathBuf};

use dscode_core::config::settings::Config;

/// Max total text inlined into the prompt (bytes of UTF-8).
const MAX_INLINE_TOTAL: usize = 200_000;
/// Max single text file inlined.
const MAX_INLINE_FILE: usize = 80_000;
/// Max upload size accepted for staging (bytes).
pub const MAX_UPLOAD_BYTES: usize = 40 * 1024 * 1024;

/// Copy external paths into `<workspace>/.dscode/uploads/<session>/` and build
/// a message block the agent can use (paths + inline text when safe).
pub fn build_message_with_attachments(
    user_text: &str,
    attachment_paths: &[String],
    workspace: &Path,
    session_id: &str,
) -> Result<String, String> {
    if attachment_paths.is_empty() {
        return Ok(user_text.to_string());
    }

    let upload_root = uploads_dir(workspace, session_id)?;
    std::fs::create_dir_all(&upload_root)
        .map_err(|e| format!("Cannot create uploads dir: {e}"))?;

    let mut blocks: Vec<String> = Vec::new();
    let mut inline_budget = MAX_INLINE_TOTAL;

    for (i, raw) in attachment_paths.iter().enumerate() {
        let src = PathBuf::from(raw.trim());
        if !src.exists() {
            blocks.push(format!(
                "### Attachment {}\n- path: `{raw}`\n- error: file not found\n",
                i + 1
            ));
            continue;
        }
        let meta = std::fs::metadata(&src).map_err(|e| format!("stat {raw}: {e}"))?;
        if !meta.is_file() {
            blocks.push(format!(
                "### Attachment {}\n- path: `{raw}`\n- error: not a regular file\n",
                i + 1
            ));
            continue;
        }
        let size = meta.len() as usize;
        if size > MAX_UPLOAD_BYTES {
            blocks.push(format!(
                "### Attachment {}\n- original: `{}`\n- error: file too large ({} > {} bytes limit)\n",
                i + 1,
                src.display(),
                size,
                MAX_UPLOAD_BYTES
            ));
            continue;
        }

        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        let safe_name = sanitize_filename(&name);
        let dest = unique_dest(&upload_root, &safe_name);

        // Prefer copy so originals outside workspace are accessible to tools.
        // If already under workspace and readable, still copy for a stable path.
        std::fs::copy(&src, &dest).map_err(|e| format!("copy {} → {:?}: {e}", src.display(), dest))?;

        let dest_disp = dest.display().to_string();
        let kind = classify_file(&dest, size);
        let mut block = format!(
            "### Attachment {}\n- name: `{name}`\n- path: `{dest_disp}`\n- size: {} bytes\n- kind: {kind}\n",
            i + 1,
            size
        );

        if kind == "text" || kind == "code" {
            let allow = inline_budget.min(MAX_INLINE_FILE).min(size.saturating_add(1024));
            if allow > 0 {
                match std::fs::read(&dest) {
                    Ok(bytes) => {
                        if let Ok(text) = String::from_utf8(bytes) {
                            let (snippet, truncated) = if text.len() > allow {
                                (text.chars().take(allow).collect::<String>(), true)
                            } else {
                                (text, false)
                            };
                            inline_budget = inline_budget.saturating_sub(snippet.len());
                            block.push_str("\n```");
                            block.push_str(lang_hint(&safe_name));
                            block.push('\n');
                            block.push_str(&snippet);
                            if truncated {
                                block.push_str("\n…[truncated — open full file at path above]…\n");
                            }
                            block.push_str("\n```\n");
                        } else {
                            block.push_str(
                                "- note: binary/non-UTF8; use do_file_read or tools on the path.\n",
                            );
                        }
                    }
                    Err(e) => block.push_str(&format!("- read error: {e}\n")),
                }
            }
        } else if kind == "image" {
            block.push_str(
                "- note: image file staged for the agent; describe or process via tools if needed.\n",
            );
        } else {
            block.push_str(
                "- note: binary staged; use shell/tools on the absolute path as needed.\n",
            );
        }
        blocks.push(block);
    }

    let mut out = String::new();
    if !user_text.trim().is_empty() {
        out.push_str(user_text.trim_end());
        out.push_str("\n\n");
    }
    out.push_str("## User attachments\n");
    out.push_str(
        "The user attached the following files. Paths are absolute under the workspace uploads dir. \
         Prefer reading them with tools when content was truncated or is binary.\n\n",
    );
    out.push_str(&blocks.join("\n"));
    Ok(out)
}

/// Save raw bytes (from paste/drag in webview) into staging uploads; return absolute path.
pub fn stage_bytes(
    name: &str,
    data: &[u8],
    workspace: Option<&Path>,
    session_id: &str,
) -> Result<String, String> {
    if data.len() > MAX_UPLOAD_BYTES {
        return Err(format!(
            "File too large ({} bytes, max {})",
            data.len(),
            MAX_UPLOAD_BYTES
        ));
    }
    let ws = workspace
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = uploads_dir(&ws, session_id)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let dest = unique_dest(&dir, &sanitize_filename(name));
    std::fs::write(&dest, data).map_err(|e| format!("write: {e}"))?;
    Ok(dest.display().to_string())
}

fn uploads_dir(workspace: &Path, session_id: &str) -> Result<PathBuf, String> {
    let sid: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(36)
        .collect();
    let sid = if sid.is_empty() { "session".into() } else { sid };
    // Prefer workspace-local uploads so agent tools can access under working_dir
    if workspace.exists() {
        Ok(workspace.join(".dscode").join("uploads").join(sid))
    } else {
        let data = Config::data_dir().map_err(|e| e.to_string())?;
        Ok(data.join("uploads").join(sid))
    }
}

fn unique_dest(dir: &Path, safe_name: &str) -> PathBuf {
    let candidate = dir.join(safe_name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(safe_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = Path::new(safe_name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    for n in 1..1000 {
        let name = if ext.is_empty() {
            format!("{stem}_{n}")
        } else {
            format!("{stem}_{n}.{ext}")
        };
        let p = dir.join(name);
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!(
        "{stem}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ))
}

fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let t = cleaned.trim().trim_start_matches('.');
    if t.is_empty() {
        "upload.bin".into()
    } else {
        t.chars().take(180).collect()
    }
}

fn classify_file(path: &Path, size: usize) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" | "heic" => "image",
        "mp4" | "mov" | "webm" | "avi" | "mkv" => "video",
        "mp3" | "wav" | "ogg" | "flac" | "m4a" => "audio",
        "pdf" | "zip" | "gz" | "tar" | "7z" | "rar" | "wasm" | "exe" | "dll" | "so" | "dylib"
        | "bin" | "dat" | "woff" | "woff2" | "ttf" | "otf" => "binary",
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "kt" | "swift" | "c" | "cc"
        | "cpp" | "h" | "hpp" | "cs" | "rb" | "php" | "scala" | "rsx" | "vue" | "svelte"
        | "css" | "scss" | "less" | "html" | "htm" | "xml" | "json" | "jsonc" | "yaml" | "yml"
        | "toml" | "ini" | "cfg" | "conf" | "md" | "mdx" | "txt" | "log" | "csv" | "tsv"
        | "sql" | "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "env" | "gitignore"
        | "dockerfile" | "makefile" | "cmake" | "graphql" | "proto" | "lock" => "code",
        _ => {
            // Heuristic: small files that look like UTF-8
            if size <= MAX_INLINE_FILE {
                if let Ok(bytes) = std::fs::read(path) {
                    if std::str::from_utf8(&bytes).is_ok() {
                        return "text";
                    }
                }
            }
            "binary"
        }
    }
}

fn lang_hint(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "md" => "markdown",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "sql" => "sql",
        "sh" | "bash" => "bash",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        _ => "",
    }
}
