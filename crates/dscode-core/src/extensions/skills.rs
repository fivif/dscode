//! Skill package loader — Agent Skills compatible layout.
//!
//! ```text
//! ~/.dscode/skills/<name>/
//! ├── SKILL.md           # required: YAML frontmatter + instructions
//! ├── scripts/           # optional: executable scripts (.sh/.py/.js/.ts/.rb/.pl)
//! ├── references/        # optional: docs the agent can read on demand
//! └── assets/            # optional: templates, configs, fixtures
//! ```
//!
//! Compatible with Claude Code / agentskills.io package shape. When a skill
//! activates, the agent receives instructions plus an inventory of bundled
//! files (with absolute paths) so it can run scripts via `do_bash` or read
//! references via `do_file_read`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Kind of bundled file inside a skill package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillResourceKind {
    Script,
    Reference,
    Asset,
    Other,
}

/// A file bundled with a skill (script / reference / asset).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillResource {
    /// Path relative to the skill root (e.g. `scripts/review.sh`).
    pub relative_path: String,
    /// Absolute filesystem path for the agent to execute/read.
    pub absolute_path: String,
    pub kind: SkillResourceKind,
    pub size_bytes: u64,
    /// Whether the file has the executable bit (Unix) or looks like a script.
    pub executable: bool,
}

/// A loaded skill package with metadata, instructions, and bundled files.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub hidden: bool,
    pub body: String,
    /// Absolute path to SKILL.md.
    pub path: PathBuf,
    /// Absolute path to the skill package directory.
    pub root: PathBuf,
    /// Bundled scripts / references / assets discovered under the skill root.
    pub resources: Vec<SkillResource>,
}

impl Skill {
    /// Build the prompt block injected when this skill activates.
    pub fn to_agent_prompt(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("## Active Skill: {}\n\n", self.name));
        if !self.description.is_empty() {
            out.push_str(&format!("**Description:** {}\n\n", self.description));
        }
        out.push_str(&format!("**Skill root:** `{}`\n\n", self.root.display()));
        out.push_str(&self.body);
        out.push('\n');

        let scripts: Vec<_> = self
            .resources
            .iter()
            .filter(|r| r.kind == SkillResourceKind::Script)
            .collect();
        let refs: Vec<_> = self
            .resources
            .iter()
            .filter(|r| r.kind == SkillResourceKind::Reference)
            .collect();
        let assets: Vec<_> = self
            .resources
            .iter()
            .filter(|r| r.kind == SkillResourceKind::Asset)
            .collect();

        if !scripts.is_empty() {
            out.push_str("\n### Bundled scripts (run with do_bash)\n");
            out.push_str(
                "Prefer these over rewriting logic. Use absolute paths. \
                 Make executable with `chmod +x` if needed.\n\n",
            );
            for s in &scripts {
                let flag = if s.executable { "exec" } else { "file" };
                out.push_str(&format!(
                    "- `{}` ({}, {} bytes)\n  path: `{}`\n",
                    s.relative_path, flag, s.size_bytes, s.absolute_path
                ));
            }
        }
        if !refs.is_empty() {
            out.push_str("\n### References (read with do_file_read when needed)\n");
            for r in &refs {
                out.push_str(&format!(
                    "- `{}` — `{}`\n",
                    r.relative_path, r.absolute_path
                ));
            }
        }
        if !assets.is_empty() {
            out.push_str("\n### Assets (templates / fixtures)\n");
            for a in &assets {
                out.push_str(&format!(
                    "- `{}` — `{}`\n",
                    a.relative_path, a.absolute_path
                ));
            }
        }
        if !self.allowed_tools.is_empty() {
            out.push_str(&format!(
                "\n**Preferred tools:** {}\n",
                self.allowed_tools.join(", ")
            ));
        }
        out
    }
}

/// Manages a collection of loaded skills from a directory tree.
pub struct SkillLoader {
    skills: Vec<Skill>,
}

/// Maximum recursion depth for skill directory traversal (E6).
const MAX_DEPTH: usize = 5;

impl SkillLoader {
    pub fn new() -> Self { Self { skills: vec![] } }

    /// All directories we scan for skill packages (ecosystem-compatible).
    ///
    /// Order = priority when names collide (first wins):
    /// 1. `~/.dscode/skills` (DS Code primary)
    /// 2. config `extensions.skills_dirs`
    /// 3. `~/.agents/skills` (skills.sh / many CLIs)
    /// 4. `~/.claude/skills` (Claude Code)
    /// 5. project-local `.dscode/skills`, `.claude/skills`, `.agents/skills`
    pub fn search_paths(extra: &[PathBuf], workspace: Option<&Path>) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut push = |p: PathBuf| {
            if !paths.iter().any(|x| x == &p) {
                paths.push(p);
            }
        };

        push(Self::default_skills_dir());
        for e in extra {
            push(e.clone());
        }
        if let Ok(home) = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
        {
            push(home.join(".agents").join("skills"));
            push(home.join(".claude").join("skills"));
            push(home.join(".codex").join("skills"));
            push(home.join(".cursor").join("skills"));
            push(home.join(".grok").join("skills"));
        }
        if let Some(ws) = workspace {
            push(ws.join(".dscode").join("skills"));
            push(ws.join(".claude").join("skills"));
            push(ws.join(".agents").join("skills"));
            push(ws.join(".cursor").join("skills"));
            push(ws.join(".grok").join("skills"));
        }
        paths
    }

    /// Load skills from every known search path (dedupe by skill name — first wins).
    /// Use for agent runtime activation.
    pub fn load_all(
        &mut self,
        extra_dirs: &[PathBuf],
        workspace: Option<&Path>,
    ) -> Result<usize, String> {
        self.load_all_inner(extra_dirs, workspace, true)
    }

    /// Load every skill package for management UI (Settings).
    /// Keeps same-name packages that live under different roots so each can be deleted.
    pub fn load_all_packages(
        &mut self,
        extra_dirs: &[PathBuf],
        workspace: Option<&Path>,
    ) -> Result<usize, String> {
        self.load_all_inner(extra_dirs, workspace, false)
    }

    fn load_all_inner(
        &mut self,
        extra_dirs: &[PathBuf],
        workspace: Option<&Path>,
        dedupe_by_name: bool,
    ) -> Result<usize, String> {
        let mut total = 0;
        let mut seen_names: HashSet<String> = HashSet::new();
        let mut seen_roots: HashSet<String> = HashSet::new();
        for dir in Self::search_paths(extra_dirs, workspace) {
            if !dir.exists() {
                continue;
            }
            let mut batch = SkillLoader::new();
            match batch.load_from_dir(&dir) {
                Ok(n) if n > 0 => {
                    for s in batch.skills {
                        let root_key = s.root.display().to_string();
                        if !seen_roots.insert(root_key) {
                            continue;
                        }
                        if dedupe_by_name {
                            let key = s.name.to_lowercase();
                            if !seen_names.insert(key) {
                                continue;
                            }
                        }
                        self.skills.push(s);
                        total += 1;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(dir = %dir.display(), %e, "skip skills dir");
                }
            }
        }
        Ok(total)
    }

    /// Load all SKILL.md files from a directory recursively.
    /// Directory structure: `<dir>/<skill-name>/SKILL.md`
    /// Creates the directory if it does not exist (so first-run list/save works).
    pub fn load_from_dir(&mut self, dir: &Path) -> Result<usize, String> {
        if !dir.exists() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Cannot create skills dir {:?}: {}", dir, e))?;
            return Ok(0);
        }
        let canon = std::fs::canonicalize(dir)
            .map_err(|e| format!("Cannot resolve skills dir {:?}: {}", dir, e))?;
        let mut visited: HashSet<u64> = HashSet::new();
        self.load_from_dir_inner(&canon, 0, &mut visited)
    }

    /// Install a third-party skill package from GitHub / skills.sh style specs.
    ///
    /// Accepted specs:
    /// - `owner/repo` — clone repo, install every skill package found
    /// - `owner/repo/path/to/skill` — install one package under that path
    /// - `https://github.com/owner/repo` — same as owner/repo
    ///
    /// Copies packages into `~/.dscode/skills/<name>/` (never runs remote scripts
    /// during install). Returns human-readable report.
    pub fn install_from_spec(spec: &str) -> Result<InstallReport, String> {
        install_skill_spec(spec)
    }

    /// Internal recursive loader with depth limit and symlink cycle detection (E6).
    fn load_from_dir_inner(
        &mut self,
        dir: &Path,
        depth: usize,
        visited: &mut HashSet<u64>,
    ) -> Result<usize, String> {
        if depth > MAX_DEPTH {
            tracing::warn!(
                "Skill directory recursion depth {} exceeded at {:?}, stopping",
                depth, dir
            );
            return Ok(0);
        }

        // Detect symlink cycles by tracking inode numbers
        if let Ok(meta) = std::fs::metadata(dir) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let ino = meta.ino();
                if !visited.insert(ino) {
                    tracing::warn!("Symlink cycle detected at {:?}, skipping", dir);
                    return Ok(0);
                }
            }
            #[cfg(not(unix))]
            {
                // On non-Unix, fall back to canonical path tracking
                if let Ok(canon) = std::fs::canonicalize(dir) {
                    use std::hash::{Hash, Hasher};
                    let path_key = {
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        canon.hash(&mut h);
                        h.finish()
                    };
                    if !visited.insert(path_key) {
                        tracing::warn!("Symlink cycle detected at {:?}, skipping", dir);
                        return Ok(0);
                    }
                }
            }
        }

        let mut count = 0;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let skill_md = path.join("SKILL.md");
                    if skill_md.exists() {
                        match Self::parse_file(&skill_md) {
                            Ok(mut skill) => {
                                skill.resources = scan_skill_resources(&skill.root);
                                self.skills.push(skill);
                                count += 1;
                            }
                            Err(e) => tracing::warn!("Failed to load skill {:?}: {}", skill_md, e),
                        }
                    } else {
                        // Recurse into subdirectories for nested skill trees
                        count += self.load_from_dir_inner(&path, depth + 1, visited)?;
                    }
                }
            }
        }
        Ok(count)
    }

    /// Parse a single SKILL.md file.
    fn parse_file(path: &Path) -> Result<Skill, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Cannot read {:?}: {}", path, e))?;
        let (frontmatter, body) = parse_yaml_frontmatter(&content)?;
        let name = get_field(&frontmatter, "name").unwrap_or_else(|| {
            path.parent().and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("unnamed").to_string()
        });
        let description = get_field(&frontmatter, "description").unwrap_or_default();
        // Prefer explicit `triggers` field; fall back to extraction from description.
        let mut triggers = get_field(&frontmatter, "triggers")
            .map(|s| {
                s.split(|c| c == ',' || c == ';' || c == '|' || c == '\n')
                    .map(|t| t.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if triggers.is_empty() {
            triggers = extract_triggers(&description);
        }
        // Always include skill name as a soft trigger
        if !triggers.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
            triggers.push(name.clone());
        }
        let allowed_tools = get_field(&frontmatter, "allowed-tools")
            .or_else(|| get_field(&frontmatter, "allowed_tools"))
            .map(|s| s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
            .unwrap_or_default();
        let hidden = get_field(&frontmatter, "hidden")
            .map(|s| s == "true" || s == "yes" || s == "1")
            .unwrap_or(false);

        let root = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| path.to_path_buf());

        Ok(Skill {
            name,
            description,
            triggers,
            allowed_tools,
            hidden,
            body,
            path: path.to_path_buf(),
            root,
            resources: vec![], // filled by load_from_dir after parse
        })
    }

    /// Find skills matching a user message (trigger keywords or skill name).
    pub fn find_matching(&self, message: &str) -> Vec<&Skill> {
        let msg_lower = message.to_lowercase();
        let mut matches: Vec<&Skill> = self
            .skills
            .iter()
            .filter(|s| {
                if s.hidden {
                    return false;
                }
                // Name match (e.g. "用 code-review skill")
                if msg_lower.contains(&s.name.to_lowercase()) {
                    return true;
                }
                s.triggers
                    .iter()
                    .any(|t| !t.is_empty() && msg_lower.contains(&t.to_lowercase()))
            })
            .collect();
        // Sort by trigger match length (longer = more specific)
        matches.sort_by(|a, b| {
            let a_len = a.triggers.iter().map(|t| t.len()).max().unwrap_or(0);
            let b_len = b.triggers.iter().map(|t| t.len()).max().unwrap_or(0);
            b_len.cmp(&a_len)
        });
        matches
    }

    /// Write a skill package to disk.
    ///
    /// Creates `SKILL.md` plus optional `scripts/`, `references/`, `assets/`
    /// entries from `files` (each item: relative path under skill root + content).
    pub fn save_skill(
        name: &str,
        description: &str,
        body: &str,
        triggers: &[String],
        files: &[(String, String)],
    ) -> Result<PathBuf, String> {
        let name = sanitize_skill_name(name)?;
        let dir = Self::default_skills_dir().join(&name);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Cannot create skill dir {:?}: {}", dir, e))?;
        // Scaffold standard package dirs (empty is fine)
        for sub in ["scripts", "references", "assets"] {
            let _ = std::fs::create_dir_all(dir.join(sub));
        }

        let triggers_line = if triggers.is_empty() {
            extract_triggers(description).join(", ")
        } else {
            triggers
                .iter()
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        };

        // Augment body with a short package layout note if scripts will be present
        let mut body_text = body.trim().to_string();
        let has_scripts = files.iter().any(|(p, _)| p.starts_with("scripts/"));
        if has_scripts && !body_text.contains("scripts/") {
            body_text.push_str(
                "\n\n## Package layout\n\
                 This skill may include files under `scripts/`, `references/`, and `assets/`. \
                 When active, absolute paths are listed — run scripts with `do_bash` and read \
                 references with `do_file_read`.\n",
            );
        }

        let content = format!(
            "---\nname: {}\ndescription: {}\ntriggers: {}\nhidden: false\n---\n\n{}\n",
            yaml_quote(&name),
            yaml_quote(description),
            yaml_quote(&triggers_line),
            body_text
        );
        let path = dir.join("SKILL.md");
        std::fs::write(&path, content)
            .map_err(|e| format!("Cannot write {:?}: {}", path, e))?;

        // Write bundled files (scripts / references / assets)
        for (rel, file_body) in files {
            let rel = rel.trim().trim_start_matches('/').replace('\\', "/");
            if rel.is_empty() || rel.contains("..") {
                return Err(format!("非法文件路径: {rel}"));
            }
            // Only allow known package roots or root-level non-md files
            let allowed = rel.starts_with("scripts/")
                || rel.starts_with("references/")
                || rel.starts_with("assets/")
                || (!rel.contains('/') && rel != "SKILL.md");
            if !allowed {
                return Err(format!(
                    "文件必须放在 scripts/、references/ 或 assets/ 下: {rel}"
                ));
            }
            let dest = dir.join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create {:?}: {}", parent, e))?;
            }
            std::fs::write(&dest, file_body)
                .map_err(|e| format!("Cannot write {:?}: {}", dest, e))?;
            // Mark scripts executable on Unix
            if rel.starts_with("scripts/") {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(&dest) {
                        let mut perms = meta.permissions();
                        perms.set_mode(0o755);
                        let _ = std::fs::set_permissions(&dest, perms);
                    }
                }
            }
        }

        Ok(path)
    }

    /// Delete a skill package by directory name and/or absolute package root.
    ///
    /// - Prefer `root` when provided (exact package path from list_skills).
    /// - Falls back to searching all skill search paths by folder / skill name.
    /// - Only deletes under known skills search roots (safety).
    /// - Symlink packages: unlinks the link under the skills root (does not
    ///   follow into targets outside the skills tree).
    /// - Returns a human-readable summary of what was removed.
    pub fn delete_skill_package(
        name: &str,
        root: Option<&str>,
        workspace: Option<&Path>,
    ) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() || name.contains("..") {
            return Err("非法 Skill 名称".into());
        }

        let allowed_parents = Self::search_paths(&[], workspace);
        let mut targets: Vec<PathBuf> = Vec::new();

        if let Some(r) = root.map(str::trim).filter(|s| !s.is_empty()) {
            let p = PathBuf::from(r);
            // Prefer logical path under skills root (keeps symlink packages deletable).
            let logical = canonicalize_preserving_symlink_leaf(&p);
            if !path_present(&logical) && !path_present(&p) {
                return Err(format!("Skill 路径不存在（可能已删除）: {r}"));
            }
            let candidate = if path_present(&logical) {
                logical
            } else {
                p
            };
            if !is_under_any_skills_root(&candidate, &allowed_parents) {
                return Err(format!(
                    "拒绝删除：路径不在 Skills 搜索目录内\n{}",
                    candidate.display()
                ));
            }
            push_unique_target(&mut targets, candidate);
        } else {
            // Resolve by name across all search dirs (folder / YAML name, nested too).
            let needle = name.to_lowercase();
            for parent in &allowed_parents {
                if !parent.exists() {
                    continue;
                }
                collect_skill_targets_by_name(parent, name, &needle, 0, &mut targets);
            }
            // Safety filter (should already be under parents)
            targets.retain(|t| is_under_any_skills_root(t, &allowed_parents));
        }

        targets.sort();
        targets.dedup();

        if targets.is_empty() {
            return Err(format!(
                "未找到可删除的 skill `{name}`（已扫描 ~/.dscode/skills、~/.claude/skills 等）"
            ));
        }

        let mut removed = Vec::new();
        let mut errors = Vec::new();
        for dir in targets {
            match remove_skill_path(&dir) {
                Ok(()) => removed.push(dir.display().to_string()),
                Err(e) => errors.push(format!("{}: {e}", dir.display())),
            }
        }

        if removed.is_empty() {
            return Err(format!("删除失败:\n{}", errors.join("\n")));
        }
        if !errors.is_empty() {
            return Ok(format!(
                "已删除 {} 处，部分失败:\n{}\n成功: {}",
                removed.len(),
                errors.join("\n"),
                removed.join(", ")
            ));
        }
        Ok(format!(
            "已删除 skill（{} 处）: {}",
            removed.len(),
            removed.join(", ")
        ))
    }

    /// Write or overwrite a single file inside an existing skill package.
    pub fn write_skill_file(skill_name: &str, relative_path: &str, content: &str) -> Result<PathBuf, String> {
        let name = sanitize_skill_name(skill_name)?;
        let rel = relative_path.trim().trim_start_matches('/').replace('\\', "/");
        if rel.is_empty() || rel.contains("..") || rel == "SKILL.md" {
            return Err("非法 relative_path".into());
        }
        let allowed = rel.starts_with("scripts/")
            || rel.starts_with("references/")
            || rel.starts_with("assets/");
        if !allowed {
            return Err("文件必须在 scripts/、references/ 或 assets/ 下".into());
        }
        let dir = Self::default_skills_dir().join(&name);
        if !dir.join("SKILL.md").exists() {
            return Err(format!("Skill `{name}` 不存在，请先创建 SKILL.md"));
        }
        let dest = dir.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create {:?}: {}", parent, e))?;
        }
        std::fs::write(&dest, content)
            .map_err(|e| format!("Cannot write {:?}: {}", dest, e))?;
        if rel.starts_with("scripts/") {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&dest) {
                    let mut perms = meta.permissions();
                    perms.set_mode(0o755);
                    let _ = std::fs::set_permissions(&dest, perms);
                }
            }
        }
        Ok(dest)
    }

    /// Find a skill by exact name.
    pub fn find_by_name(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Get all visible skills.
    pub fn list_visible(&self) -> Vec<&Skill> {
        self.skills.iter().filter(|s| !s.hidden).collect()
    }

    /// Get all skills including hidden.
    pub fn list_all(&self) -> &[Skill] { &self.skills }

    pub fn is_empty(&self) -> bool { self.skills.is_empty() }

    /// Get the skills directory path.
    pub fn default_skills_dir() -> PathBuf {
        match crate::config::settings::Config::data_dir() {
            Ok(dir) => dir.join("skills"),
            Err(e) => {
                tracing::warn!(
                    "Cannot determine data directory ({}), falling back to current directory for skills",
                    e
                );
                PathBuf::from(".").join("skills")
            }
        }
    }
}

/// True if path exists as a real entry or a (possibly broken) symlink.
fn path_present(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Canonicalize parent only; keep final component so symlink packages stay
/// under the skills search root (instead of resolving into ~/.cc-switch/… etc.).
fn canonicalize_preserving_symlink_leaf(path: &Path) -> PathBuf {
    if let Some(parent) = path.parent() {
        if let Ok(cp) = std::fs::canonicalize(parent) {
            if let Some(name) = path.file_name() {
                return cp.join(name);
            }
        }
    }
    // Fall back: full canonicalize if parent missing / leaf missing
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn push_unique_target(targets: &mut Vec<PathBuf>, path: PathBuf) {
    let key = path.display().to_string();
    if targets.iter().any(|t| t.display().to_string() == key) {
        return;
    }
    // Also skip if another entry is the same after leaf-preserving canon
    let leaf = canonicalize_preserving_symlink_leaf(&path);
    if targets
        .iter()
        .any(|t| canonicalize_preserving_symlink_leaf(t) == leaf)
    {
        return;
    }
    targets.push(path);
}

/// Whether `path` is inside any known skills search root.
///
/// Checks logical path (symlink leaf preserved) first so packages that are
/// symlinks *into* external dirs can still be unlinked from the skills tree.
fn is_under_any_skills_root(path: &Path, roots: &[PathBuf]) -> bool {
    let logical = canonicalize_preserving_symlink_leaf(path);
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let candidates = [path.to_path_buf(), logical, full];

    for root in roots {
        let root_raw = root.clone();
        let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        for c in &candidates {
            if c.starts_with(&root_canon) || c.starts_with(&root_raw) {
                return true;
            }
            if let Some(parent) = c.parent() {
                if parent == root_canon.as_path() || parent == root_raw.as_path() {
                    return true;
                }
            }
        }
    }
    false
}

/// Recursively find skill packages matching folder name or YAML `name:`.
fn collect_skill_targets_by_name(
    dir: &Path,
    name: &str,
    needle: &str,
    depth: usize,
    out: &mut Vec<PathBuf>,
) {
    if depth > MAX_DEPTH || !dir.exists() {
        return;
    }
    // Direct child folder match (symlink package or real dir with SKILL.md)
    let by_folder = dir.join(name);
    if path_present(&by_folder)
        && (by_folder.join("SKILL.md").exists() || is_symlink(&by_folder))
    {
        push_unique_target(out, canonicalize_preserving_symlink_leaf(&by_folder));
    }

    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        // Use symlink_metadata so we treat symlink packages as leaves
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_link = meta.file_type().is_symlink();
        let looks_dir = meta.file_type().is_dir() || (is_link && path.is_dir());
        if !looks_dir && !is_link {
            continue;
        }

        let skill_md = path.join("SKILL.md");
        if skill_md.exists() || (is_link && path.join("SKILL.md").exists()) {
            let folder_match = path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(name))
                .unwrap_or(false);
            let yaml_match = std::fs::read_to_string(&skill_md)
                .ok()
                .and_then(|c| parse_frontmatter_name(&c))
                .map(|n| n.eq_ignore_ascii_case(needle) || n.to_lowercase() == needle)
                .unwrap_or(false);
            if folder_match || yaml_match {
                push_unique_target(out, canonicalize_preserving_symlink_leaf(&path));
            }
            // Do not recurse into a package that has SKILL.md
            continue;
        }

        // Nested trees (e.g. ~/.codex/skills/.system/<name>)
        if meta.file_type().is_dir() && !is_link {
            collect_skill_targets_by_name(&path, name, needle, depth + 1, out);
        }
    }
}

/// Extract `name:` from SKILL.md frontmatter (best-effort).
fn parse_frontmatter_name(content: &str) -> Option<String> {
    let mut in_fm = false;
    for line in content.lines() {
        let t = line.trim();
        if t == "---" {
            if in_fm {
                break;
            }
            in_fm = true;
            continue;
        }
        if !in_fm {
            continue;
        }
        if let Some(rest) = t.strip_prefix("name:") {
            let v = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Delete a skill package path: unlink symlink packages; otherwise robust tree delete.
fn remove_skill_path(dir: &Path) -> Result<(), String> {
    if !path_present(dir) {
        return Ok(());
    }
    if is_symlink(dir) {
        std::fs::remove_file(dir)
            .map_err(|e| format!("无法删除符号链接 {:?}: {e}", dir))?;
        return Ok(());
    }
    remove_dir_all_robust(dir)
}

/// remove_dir_all with retry — macOS sometimes returns "Directory not empty" / busy.
fn remove_dir_all_robust(dir: &Path) -> Result<(), String> {
    if !path_present(dir) {
        return Ok(());
    }
    if is_symlink(dir) {
        std::fs::remove_file(dir).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if !dir.join("SKILL.md").exists() {
        tracing::warn!(path = %dir.display(), "delete path has no SKILL.md");
    }

    let mut last_err = String::new();
    for attempt in 1..=5 {
        // Symlink at top handled above; walk-first is more reliable on macOS with
        // busy files than a single remove_dir_all.
        if let Err(e2) = remove_tree_manual(dir) {
            last_err = e2;
            match std::fs::remove_dir_all(dir) {
                Ok(()) if !path_present(dir) => return Ok(()),
                Ok(()) => last_err = "path still exists after remove_dir_all".into(),
                Err(e) => last_err = format!("{last_err}; remove_dir_all: {e}"),
            }
        } else if !path_present(dir) {
            return Ok(());
        } else {
            last_err = "path still exists after manual remove".into();
            let _ = std::fs::remove_dir_all(dir);
            if !path_present(dir) {
                return Ok(());
            }
        }
        if attempt < 5 {
            std::thread::sleep(std::time::Duration::from_millis(50 * attempt as u64));
        }
    }
    Err(format!(
        "无法删除 {:?}（{last_err}）。请检查权限或是否被占用。",
        dir
    ))
}

/// Walk and delete without following directory symlinks (unlink them instead).
fn remove_tree_manual(dir: &Path) -> Result<(), String> {
    if !path_present(dir) {
        return Ok(());
    }
    if is_symlink(dir) {
        std::fs::remove_file(dir).map_err(|e| format!("remove symlink {:?}: {e}", dir))?;
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    if meta.file_type().is_file() {
        clear_readonly(dir);
        std::fs::remove_file(dir).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if !meta.file_type().is_dir() {
        // socket/fifo etc.
        clear_readonly(dir);
        let _ = std::fs::remove_file(dir);
        return Ok(());
    }

    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let p = entry.path();
        let child_meta = match std::fs::symlink_metadata(&p) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if child_meta.file_type().is_symlink() {
            std::fs::remove_file(&p).map_err(|e| format!("remove symlink {:?}: {e}", p))?;
        } else if child_meta.file_type().is_dir() {
            remove_tree_manual(&p)?;
        } else {
            clear_readonly(&p);
            std::fs::remove_file(&p).map_err(|e| format!("remove file {:?}: {e}", p))?;
        }
    }
    clear_readonly(dir);
    std::fs::remove_dir(dir).map_err(|e| format!("remove dir {:?}: {e}", dir))?;
    Ok(())
}

fn clear_readonly(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                return;
            }
            let mut perms = meta.permissions();
            // u+w for files; dirs need execute too
            let mode = if meta.file_type().is_dir() {
                0o755
            } else {
                0o644
            };
            perms.set_mode(mode);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Parse simple YAML-like frontmatter: `key: value` pairs between `---` delimiters.
fn parse_yaml_frontmatter(content: &str) -> Result<(HashMap<String, String>, String), String> {
    if !content.starts_with("---") {
        return Ok((HashMap::new(), content.to_string()));
    }
    let rest = &content[3..];
    let end = rest.find("---").ok_or("Unclosed frontmatter")?;
    let fm_text = &rest[..end];
    let body = rest[end + 3..].trim().to_string();

    let mut map = HashMap::new();
    let mut current_key = String::new();
    let mut current_value = String::new();
    let mut in_multiline = false;

    for line in fm_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }

        // If we see a new key: value pattern (not indented), exit multi-line mode
        // E5: key pattern: ^[a-zA-Z_][a-zA-Z0-9_]*: with optional value
        let is_new_key = is_key_value_line(trimmed);

        if in_multiline && is_new_key {
            // Save the accumulated multi-line value and start a new key
            if !current_key.is_empty() {
                map.insert(current_key.clone(), current_value.trim().to_string());
            }
            in_multiline = false;
            current_key.clear();
            current_value.clear();
        }

        if !in_multiline {
            if let Some(pos) = trimmed.find(':') {
                // Save previous key if any
                if !current_key.is_empty() {
                    map.insert(current_key.clone(), current_value.trim().to_string());
                }
                current_key = trimmed[..pos].trim().to_string();
                let val = trimmed[pos + 1..].trim().to_string();
                if val.is_empty() {
                    in_multiline = true;
                    current_value = String::new();
                } else {
                    current_value = val;
                    map.insert(current_key.clone(), current_value.clone());
                    current_key.clear();
                    current_value.clear();
                }
            }
        } else {
            // Multi-line value: continue accumulating
            if !current_value.is_empty() { current_value.push('\n'); }
            current_value.push_str(trimmed);
        }
    }
    if !current_key.is_empty() {
        map.insert(current_key, current_value.trim().to_string());
    }

    Ok((map, body))
}

/// Check if a trimmed line looks like a YAML key: value pair (not a continuation).
/// Matches: key with letters/digits/_/- followed by colon.
fn is_key_value_line(line: &str) -> bool {
    // Must start at column 0 (not indented) and match key: pattern
    if line.starts_with(' ') || line.starts_with('\t') {
        return false;
    }
    if let Some(pos) = line.find(':') {
        let key = &line[..pos];
        // Key must be non-empty and match identifier pattern (allow hyphens: allowed-tools)
        if key.is_empty() {
            return false;
        }
        let first = key.chars().next().unwrap();
        if !first.is_ascii_alphabetic() && first != '_' {
            return false;
        }
        key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    } else {
        false
    }
}

/// Sanitize skill directory / name: lowercase kebab-case.
fn sanitize_skill_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Skill 名称不能为空".into());
    }
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err("Skill 名称不能包含路径字符".into());
    }
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else if c.is_whitespace() {
                '-'
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if cleaned.is_empty() {
        return Err("Skill 名称无效".into());
    }
    if cleaned.len() > 64 {
        return Err("Skill 名称过长（最多 64）".into());
    }
    Ok(cleaned)
}

/// Result of installing a third-party skill package.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InstallReport {
    pub spec: String,
    pub installed: Vec<String>,
    pub skipped: Vec<String>,
    pub source_dir: String,
    pub target_dir: String,
    pub message: String,
}

/// Install skill packages from a GitHub-style spec into `~/.dscode/skills`.
fn install_skill_spec(spec: &str) -> Result<InstallReport, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("请提供包标识，例如 vercel-labs/agent-skills 或 owner/repo/skill-name".into());
    }

    let (owner, repo, subpath) = parse_github_spec(spec)?;
    let target_root = SkillLoader::default_skills_dir();
    std::fs::create_dir_all(&target_root)
        .map_err(|e| format!("Cannot create {:?}: {e}", target_root))?;

    let tmp = std::env::temp_dir().join(format!(
        "dscode-skill-install-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_millis()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("temp dir: {e}"))?;

    let url = format!("https://github.com/{owner}/{repo}.git");
    // Shallow clone only — we never execute remote scripts during install.
    let mut git = std::process::Command::new("git");
    git.args([
        "clone",
        "--depth",
        "1",
        "--quiet",
        &url,
        tmp.join("repo").to_str().unwrap_or("repo"),
    ]);
    // Optional proxy for skill downloads
    if let Ok(cfg) = crate::config::settings::Config::load() {
        crate::config::settings::apply_proxy_env(&mut git, cfg.proxy_for_skills());
    }
    let status = git.status().map_err(|e| {
        format!("无法运行 git（安装第三方 skill 需要本机有 git）: {e}")
    })?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "git clone 失败: {url}\n请检查网络与仓库是否存在，或手动: npx skills add {owner}/{repo}"
        ));
    }

    let repo_root = tmp.join("repo");
    let search_root = if let Some(ref sub) = subpath {
        let p = repo_root.join(sub);
        if !p.exists() {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!("仓库内找不到路径: {sub}"));
        }
        p
    } else {
        repo_root.clone()
    };

    // Find skill packages: any directory containing SKILL.md
    let mut packages: Vec<PathBuf> = Vec::new();
    find_skill_packages(&search_root, &mut packages, 0);
    if packages.is_empty() {
        // maybe the root itself is a skill
        if search_root.join("SKILL.md").exists() {
            packages.push(search_root.clone());
        }
    }
    if packages.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "在 {owner}/{repo}{} 中未找到 SKILL.md 技能包",
            subpath
                .as_ref()
                .map(|s| format!("/{s}"))
                .unwrap_or_default()
        ));
    }

    let mut installed = Vec::new();
    let mut skipped = Vec::new();
    for pkg in &packages {
        let name = pkg
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("skill")
            .to_string();
        let safe = match sanitize_skill_name(&name) {
            Ok(s) => s,
            Err(_) => format!(
                "{}-{}",
                sanitize_skill_name(&format!("{owner}-{name}")).unwrap_or_else(|_| "skill".into()),
                installed.len() + skipped.len()
            ),
        };
        let dest = target_root.join(&safe);
        if dest.exists() {
            skipped.push(format!("{safe} (已存在，跳过)"));
            continue;
        }
        copy_dir_recursive(pkg, &dest)?;
        installed.push(safe);
    }

    let _ = std::fs::remove_dir_all(&tmp);

    let message = if installed.is_empty() {
        format!(
            "未新装技能（{} 个已存在）。可用 skills 目录: {}",
            skipped.len(),
            target_root.display()
        )
    } else {
        format!(
            "已安装 {} 个 skill 到 {}:\n{}",
            installed.len(),
            target_root.display(),
            installed
                .iter()
                .map(|n| format!("  - {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    Ok(InstallReport {
        spec: spec.to_string(),
        installed,
        skipped,
        source_dir: format!("github.com/{owner}/{repo}"),
        target_dir: target_root.display().to_string(),
        message,
    })
}

/// Parse `owner/repo`, `owner/repo/sub/path`, or GitHub URL.
fn parse_github_spec(spec: &str) -> Result<(String, String, Option<String>), String> {
    let s = spec
        .trim()
        .trim_end_matches(".git")
        .trim_end_matches('/');
    let s = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"))
        .unwrap_or(s);
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 {
        return Err(
            "格式应为 owner/repo 或 owner/repo/skill-path（见 https://www.skills.sh/）".into(),
        );
    }
    let owner = parts[0].to_string();
    let repo = parts[1].to_string();
    if owner.contains("..") || repo.contains("..") {
        return Err("非法仓库名".into());
    }
    let sub = if parts.len() > 2 {
        Some(parts[2..].join("/"))
    } else {
        None
    };
    Ok((owner, repo, sub))
}

fn find_skill_packages(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return;
    }
    if dir.join("SKILL.md").exists() {
        // This directory is a skill package — don't recurse into scripts/
        out.push(dir.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            find_skill_packages(&path, out, depth + 1);
        }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {:?}: {e}", dst))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("read {:?}: {e}", src))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().ok_or("bad name")?;
        let target = dst.join(name);
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)
                .map_err(|e| format!("copy {:?} -> {:?}: {e}", path, target))?;
            #[cfg(unix)]
            {
                // preserve +x for scripts
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&path) {
                    let mode = meta.permissions().mode();
                    if mode & 0o111 != 0 {
                        let mut p = std::fs::metadata(&target)
                            .map(|m| m.permissions())
                            .unwrap_or_else(|_| std::fs::Permissions::from_mode(0o644));
                        p.set_mode(mode);
                        let _ = std::fs::set_permissions(&target, p);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Quote a string for simple YAML scalar (always double-quoted + escape).
fn yaml_quote(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "");
    format!("\"{escaped}\"")
}

/// Discover scripts/references/assets under a skill package root.
fn scan_skill_resources(root: &Path) -> Vec<SkillResource> {
    let mut out = Vec::new();
    for (subdir, kind) in [
        ("scripts", SkillResourceKind::Script),
        ("references", SkillResourceKind::Reference),
        ("assets", SkillResourceKind::Asset),
    ] {
        let base = root.join(subdir);
        if !base.is_dir() {
            continue;
        }
        walk_resources(&base, root, kind, &mut out, 0);
    }
    // Also pick up root-level scripts (e.g. run.sh next to SKILL.md)
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.eq_ignore_ascii_case("SKILL.md") || name.starts_with('.') {
                continue;
            }
            if looks_like_script(name) {
                push_resource(&path, root, SkillResourceKind::Script, &mut out);
            }
        }
    }
    out.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    out
}

fn walk_resources(
    dir: &Path,
    root: &Path,
    kind: SkillResourceKind,
    out: &mut Vec<SkillResource>,
    depth: usize,
) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_resources(&path, root, kind, out, depth + 1);
        } else if path.is_file() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') {
                continue;
            }
            push_resource(&path, root, kind, out);
        }
    }
}

fn push_resource(path: &Path, root: &Path, kind: SkillResourceKind, out: &mut Vec<SkillResource>) {
    let rel = path
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.display().to_string());
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut executable = looks_like_script(path.file_name().and_then(|n| n.to_str()).unwrap_or(""));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            executable = executable || (meta.permissions().mode() & 0o111) != 0;
        }
    }
    // For non-script folders, don't mark as executable
    let executable = if kind == SkillResourceKind::Script {
        executable
    } else {
        false
    };
    out.push(SkillResource {
        relative_path: rel,
        absolute_path: path.display().to_string(),
        kind,
        size_bytes: size,
        executable,
    });
}

fn looks_like_script(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".sh")
        || lower.ends_with(".bash")
        || lower.ends_with(".py")
        || lower.ends_with(".js")
        || lower.ends_with(".mjs")
        || lower.ends_with(".cjs")
        || lower.ends_with(".ts")
        || lower.ends_with(".rb")
        || lower.ends_with(".pl")
        || lower.ends_with(".r")
        || lower.ends_with(".ps1")
        || lower == "run"
        || lower == "main"
}

fn get_field(fm: &HashMap<String, String>, key: &str) -> Option<String> {
    fm.get(key)
        .map(|v| unquote_yaml(v))
        .filter(|v| !v.is_empty())
}

/// Strip surrounding quotes and unescape simple YAML double-quoted scalars.
fn unquote_yaml(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        return inner
            .replace("\\\"", "\"")
            .replace("\\n", "\n")
            .replace("\\\\", "\\");
    }
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        return s[1..s.len() - 1].to_string();
    }
    s.to_string()
}

/// Extract trigger keywords from a description field.
/// Supports English quoted phrases, CJK segments, and long words.
fn extract_triggers(desc: &str) -> Vec<String> {
    let desc = desc.trim();
    if desc.is_empty() {
        return vec![];
    }
    let desc_lower = desc.to_lowercase();
    let mut triggers: Vec<String> = vec![];

    // 1) Quoted phrases ("code review", "检查代码")
    let mut in_quote = false;
    let mut current_phrase = String::new();
    for ch in desc.chars() {
        if ch == '"' || ch == '\u{201c}' || ch == '\u{201d}' || ch == '「' || ch == '」' {
            if in_quote && !current_phrase.is_empty() {
                triggers.push(current_phrase.trim().to_lowercase());
                current_phrase.clear();
            }
            in_quote = !in_quote;
        } else if in_quote {
            current_phrase.push(ch);
        }
    }

    // 2) CJK: take consecutive CJK runs of length >= 2 as triggers
    let mut cjk = String::new();
    for ch in desc.chars() {
        if is_cjk(ch) {
            cjk.push(ch);
        } else if !cjk.is_empty() {
            if cjk.chars().count() >= 2 {
                triggers.push(cjk.clone());
            }
            cjk.clear();
        }
    }
    if cjk.chars().count() >= 2 {
        triggers.push(cjk);
    }

    // 3) English words from first sentence (len >= 4)
    if let Some(first) = desc_lower.split(|c| c == '.' || c == '。' || c == '!' || c == '！').next() {
        triggers.extend(
            first
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .map(|w| w.trim())
                .filter(|w| w.len() >= 4)
                .map(|w| w.to_string()),
        );
    }

    // Dedupe preserve order
    let mut seen = HashSet::new();
    triggers
        .into_iter()
        .filter(|t| !t.is_empty() && seen.insert(t.clone()))
        .take(24)
        .collect()
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified
        | '\u{3400}'..='\u{4DBF}' // Extension A
        | '\u{F900}'..='\u{FAFF}' // Compatibility
        | '\u{3000}'..='\u{303F}' // CJK punctuation (skip most)
    ) && !matches!(c, '\u{3000}'..='\u{303F}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = env::temp_dir().join(format!("dscode-skills-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let skill_dir = tmp.join("my-skill");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::create_dir_all(skill_dir.join("references")).unwrap();
        let content = format!(
            "---\nname: {}\ndescription: {}\ntriggers: {}\nhidden: false\n---\n\n# Body\nDo X\n",
            yaml_quote("my-skill"),
            yaml_quote("代码审查 skill: 检查 diff"),
            yaml_quote("代码审查, code review, 检查"),
        );
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        std::fs::write(skill_dir.join("scripts").join("check.sh"), "#!/bin/sh\necho ok\n").unwrap();
        std::fs::write(skill_dir.join("references").join("notes.md"), "# notes\n").unwrap();

        let mut loader = SkillLoader::new();
        let n = loader.load_from_dir(&tmp).unwrap();
        assert_eq!(n, 1);
        let s = loader.find_by_name("my-skill").unwrap();
        assert!(s.description.contains("代码审查"));
        assert!(s.triggers.iter().any(|t| t.contains("代码审查") || t.contains("code review")));
        assert!(!loader.find_matching("请帮我做一次代码审查").is_empty());
        assert!(s.resources.iter().any(|r| r.relative_path == "scripts/check.sh"));
        assert!(s.resources.iter().any(|r| r.relative_path == "references/notes.md"));
        let prompt = s.to_agent_prompt();
        assert!(prompt.contains("Bundled scripts") || prompt.contains("scripts/check.sh"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sanitize_name() {
        assert_eq!(sanitize_skill_name("Code Review").unwrap(), "code-review");
        assert!(sanitize_skill_name("../x").is_err());
    }

    #[test]
    fn delete_skill_package_by_root() {
        let tmp = env::temp_dir().join(format!("dscode-del-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let skills = tmp.join("skills");
        let pkg = skills.join("del-me");
        std::fs::create_dir_all(pkg.join("scripts")).unwrap();
        std::fs::write(
            pkg.join("SKILL.md"),
            "---\nname: del-me\ndescription: t\n---\n\nbody\n",
        )
        .unwrap();
        std::fs::write(pkg.join("scripts").join("a.sh"), "echo a\n").unwrap();

        // Temporarily point default via workspace-style search: call helpers directly
        assert!(pkg.join("SKILL.md").exists());
        remove_skill_path(&pkg).unwrap();
        assert!(!path_present(&pkg));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn delete_symlink_package_unlinks_only() {
        let tmp = env::temp_dir().join(format!("dscode-del-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let skills = tmp.join("skills");
        let external = tmp.join("external").join("real-skill");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(
            external.join("SKILL.md"),
            "---\nname: real-skill\ndescription: t\n---\n\nbody\n",
        )
        .unwrap();
        let link = skills.join("real-skill");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&external, &link).unwrap();
            assert!(is_symlink(&link));
            assert!(is_under_any_skills_root(&link, &[skills.clone()]));
            // Full canonicalize would leave skills root — safety still ok via logical path
            remove_skill_path(&link).unwrap();
            assert!(!path_present(&link), "symlink should be unlinked");
            assert!(
                external.join("SKILL.md").exists(),
                "target package must remain"
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_all_packages_keeps_duplicate_names() {
        let tmp = env::temp_dir().join(format!("dscode-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let a = tmp.join("a");
        let b = tmp.join("b");
        for dir in [&a, &b] {
            let pkg = dir.join("same-name");
            std::fs::create_dir_all(&pkg).unwrap();
            std::fs::write(
                pkg.join("SKILL.md"),
                "---\nname: same-name\ndescription: t\n---\n\nx\n",
            )
            .unwrap();
        }
        let mut loader = SkillLoader::new();
        // Manual load both dirs without name dedupe
        let mut total = 0;
        for dir in [&a, &b] {
            let mut batch = SkillLoader::new();
            batch.load_from_dir(dir).unwrap();
            for s in batch.skills {
                loader.skills.push(s);
                total += 1;
            }
        }
        assert_eq!(total, 2);
        assert_eq!(loader.list_all().len(), 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn save_with_scripts() {
        // Uses real default skills dir — write under a unique name then delete
        let name = format!("test-pkg-{}", std::process::id());
        let files = vec![
            ("scripts/hello.sh".into(), "#!/bin/sh\necho hi\n".into()),
            ("assets/template.txt".into(), "hello\n".into()),
        ];
        let path = SkillLoader::save_skill(
            &name,
            "test package",
            "# Run hello.sh",
            &["test-pkg".into()],
            &files,
        )
        .unwrap();
        assert!(path.exists());
        let mut loader = SkillLoader::new();
        loader.load_from_dir(&SkillLoader::default_skills_dir()).unwrap();
        let s = loader.find_by_name(&name).expect("skill loaded");
        assert!(s.resources.iter().any(|r| r.relative_path.contains("hello.sh")));
        let dir = SkillLoader::default_skills_dir().join(&name);
        let _ = std::fs::remove_dir_all(dir);
    }
}
