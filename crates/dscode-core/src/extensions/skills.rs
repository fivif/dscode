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
        // name → index into self.skills (for same-name dedupe across roots).
        let mut seen_names: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
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
                            match seen_names.get(&key) {
                                None => {
                                    seen_names.insert(key, self.skills.len());
                                    self.skills.push(s);
                                    total += 1;
                                }
                                Some(&idx) => {
                                    // Same skill name from another search root
                                    // (~/.dscode/skills vs ~/.claude/skills, project
                                    // dirs…). Contract: "first wins" unless a later
                                    // root ships a strictly NEWER package.
                                    //
                                    // Resource count is only a tiebreak on equal
                                    // mtimes: preferring "richer" outright let a
                                    // stale copy with more files silently replace
                                    // the user's edited package. `>=` likewise made
                                    // equal-mtime later roots win, contradicting the
                                    // documented priority.
                                    let existing = &self.skills[idx];
                                    let new_mtime = skill_freshness(&s);
                                    let old_mtime = skill_freshness(existing);
                                    let take = if new_mtime != old_mtime {
                                        new_mtime > old_mtime
                                    } else {
                                        s.resources.len() > existing.resources.len()
                                    };
                                    if take {
                                        tracing::debug!(
                                            skill = %s.name,
                                            from = %existing.root.display(),
                                            to = %s.root.display(),
                                            new_mtime,
                                            old_mtime,
                                            "skills: replaced same-name package from a later root"
                                        );
                                        self.skills[idx] = s;
                                    }
                                }
                            }
                        } else {
                            self.skills.push(s);
                            total += 1;
                        }
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
        #[cfg(unix)]
        {
            if let Ok(meta) = std::fs::metadata(dir) {
                use std::os::unix::fs::MetadataExt;
                let ino = meta.ino();
                if !visited.insert(ino) {
                    tracing::warn!("Symlink cycle detected at {:?}, skipping", dir);
                    return Ok(0);
                }
            }
        }
        #[cfg(not(unix))]
        {
            // On non-Unix, fall back to canonical path tracking. canonicalize
            // failing covers the does-not-exist case the old metadata guard caught.
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
            // A package path from list_skills is absolute and normalized. `..`
            // would let `<skills>/x/..` resolve back to the search root and
            // delete the whole tree.
            if p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(format!("拒绝删除：路径不能包含 `..`\n{r}"));
            }
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
            // Never delete anything that is not a real skill package. This is
            // what stops `root` from pointing at a search root (or any other
            // directory) and wiping the whole tree with remove_dir_all.
            if let Err(e) = validate_skill_target(&dir, name) {
                errors.push(e);
                continue;
            }
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

/// True only when `path` is *strictly inside* `root` (at least one component
/// deeper). Equality is rejected on purpose: a skills search root is never a
/// deletable package, and `starts_with` used to return true for the root
/// itself, which let `delete_skill` recursively delete the whole tree.
///
/// A path containing `..` is never accepted — `<root>/x/..` resolves back to
/// the root once the OS walks it, so it must not count as "inside".
fn is_strictly_under(path: &Path, root: &Path) -> bool {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    path.components().count() > root.components().count() && path.starts_with(root)
}

/// Whether `path` is inside any known skills search root.
///
/// Checks logical path (symlink leaf preserved) first so packages that are
/// symlinks *into* external dirs can still be unlinked from the skills tree.
/// The root itself is never "under" a root — see [`is_strictly_under`].
fn is_under_any_skills_root(path: &Path, roots: &[PathBuf]) -> bool {
    let logical = canonicalize_preserving_symlink_leaf(path);
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let candidates = [path.to_path_buf(), logical, full];

    for root in roots {
        let root_raw = root.clone();
        let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        for c in &candidates {
            if is_strictly_under(c, &root_canon) || is_strictly_under(c, &root_raw) {
                return true;
            }
        }
    }
    false
}

/// Reject delete targets that are not a real skill package.
///
/// A real directory must contain a `SKILL.md` and its frontmatter `name` (or
/// the directory name) must agree with the requested name; a symlink package
/// is only unlinked, so a matching link name is enough. This is the second
/// half of the guard against `delete_skill(root = <search root>)`:
/// even if a path slips past the location check it cannot be deleted unless it
/// actually looks like the package the caller asked for.
fn validate_skill_target(dir: &Path, requested_name: &str) -> Result<(), String> {
    if !path_present(dir) {
        // Already gone — nothing to validate, nothing to delete.
        return Ok(());
    }
    let folder = dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let requested = requested_name.trim().to_lowercase();
    let name_matches_folder = !requested.is_empty() && requested == folder;

    if is_symlink(dir) {
        // A symlink package is only unlinked, never recursed into, so the
        // package check is unnecessary — but the link name must still line up
        // so a wrong `root` cannot unlink an arbitrary entry.
        return if name_matches_folder {
            Ok(())
        } else {
            Err(format!(
                "拒绝删除 {}：符号链接名 `{folder}` 与请求的 `{requested_name}` 不一致",
                dir.display()
            ))
        };
    }

    let md = dir.join("SKILL.md");
    if !md.is_file() {
        return Err(format!(
            "拒绝删除 {}：不是 skill 包（目录中没有 SKILL.md）",
            dir.display()
        ));
    }
    if name_matches_folder {
        return Ok(());
    }
    let fm_name = std::fs::read_to_string(&md)
        .ok()
        .and_then(|c| parse_frontmatter_name(&c))
        .map(|n| n.trim().to_lowercase());
    match fm_name {
        Some(n) if n == requested => Ok(()),
        Some(n) => Err(format!(
            "拒绝删除 {}：SKILL.md 的 name `{n}` 与请求的 `{requested_name}` 不一致",
            dir.display()
        )),
        None => Err(format!(
            "拒绝删除 {}：SKILL.md 缺少 name 字段",
            dir.display()
        )),
    }
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
///
/// Delegates to the real parser so it sees the same (BOM-stripped, quote- and
/// block-scalar-aware) view of the file as [`SkillLoader::parse_file`].
fn parse_frontmatter_name(content: &str) -> Option<String> {
    parse_yaml_frontmatter(content)
        .ok()
        .and_then(|(fm, _)| get_field(&fm, "name"))
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
    if !dir.join("SKILL.md").is_file() {
        // Hard error, not a warning: this function only ever deletes skill
        // packages, and the warn-and-continue version is what turned a bad
        // `root` argument into "recursively delete the skills tree".
        return Err(format!(
            "拒绝删除 {}：目标不是 skill 包（缺少 SKILL.md）",
            dir.display()
        ));
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

/// Parse simple YAML-like frontmatter: `key: value` pairs between `---` lines.
///
/// Supports: plain scalars, single/double quoted scalars, `|` / `>` block
/// scalars, and `- item` sequences (joined with `, ` so the existing
/// comma-splitting consumers keep working).
fn parse_yaml_frontmatter(content: &str) -> Result<(HashMap<String, String>, String), String> {
    // A UTF-8 BOM (Windows editors add one) made `starts_with("---")` fail and
    // silently dropped the whole frontmatter — the skill loaded "dead".
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let Some(after_open) = content.strip_prefix("---") else {
        return Ok((HashMap::new(), content.to_string()));
    };
    // The opening `---` must be a line of its own, not e.g. `--- foo`.
    let (open_rest, rest) = match after_open.find('\n') {
        Some(i) => (&after_open[..i], &after_open[i + 1..]),
        None => (after_open, ""),
    };
    if !open_rest.trim().is_empty() {
        return Ok((HashMap::new(), content.to_string()));
    }

    // The terminator must be a `---` on its own line. `find("---")` matched
    // anywhere, so `description: a---b` truncated the frontmatter mid-value and
    // dropped every following field (name/triggers), and a saved skill could
    // not be reloaded.
    let (fm_end, body_start) = {
        let mut offset = 0usize;
        let mut found = None;
        for line in rest.split_inclusive('\n') {
            if line.trim_end_matches(|c| c == '\r' || c == '\n').trim() == "---" {
                found = Some((offset, offset + line.len()));
                break;
            }
            offset += line.len();
        }
        found.ok_or("Unclosed frontmatter")?
    };
    let fm_text = &rest[..fm_end];
    let body = rest[body_start..].trim().to_string();

    let raw_lines: Vec<&str> = fm_text.lines().collect();
    let mut map: HashMap<String, String> = HashMap::new();
    let mut i = 0usize;
    while i < raw_lines.len() {
        let raw = raw_lines[i];
        let trimmed = raw.trim();
        if trimmed.is_empty() || !is_key_value_line(trimmed) {
            i += 1;
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let Some(pos) = trimmed.find(':') else {
            i += 1;
            continue;
        };
        let key = trimmed[..pos].trim().to_string();
        let val = trimmed[pos + 1..].trim().to_string();

        // Block scalar: `description: |` / `>` (with optional chomping `-`/`+`).
        let indicator = val.trim_end_matches(|c| c == '-' || c == '+');
        if indicator == "|" || indicator == ">" {
            let folded = indicator == ">";
            let mut block: Vec<String> = Vec::new();
            let mut block_indent: Option<usize> = None;
            let mut j = i + 1;
            while j < raw_lines.len() {
                let l = raw_lines[j];
                if l.trim().is_empty() {
                    block.push(String::new());
                    j += 1;
                    continue;
                }
                let li = l.len() - l.trim_start().len();
                if li <= indent {
                    break;
                }
                let bi = *block_indent.get_or_insert(li);
                let cut = if li >= bi { bi } else { li };
                block.push(l[cut..].to_string());
                j += 1;
            }
            while block.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
                block.pop();
            }
            let mut text = if folded {
                let mut t = String::new();
                let mut prev_blank = true;
                for l in &block {
                    if l.trim().is_empty() {
                        t.push('\n');
                        prev_blank = true;
                    } else {
                        if !prev_blank && !t.is_empty() {
                            t.push(' ');
                        }
                        t.push_str(l);
                        prev_blank = false;
                    }
                }
                t
            } else {
                block.join("\n")
            };
            if !val.ends_with('-') {
                // `|-` strips the trailing newline; `|` and `|+` keep one.
                text.push('\n');
            }
            map.insert(key, text);
            i = j;
            continue;
        }

        if val.is_empty() {
            // Sequence under this key: `- item` lines (YAML allows them at the
            // same indentation as the key). Joined with ", " so the existing
            // `split(',')` consumers see real entries instead of one garbage
            // string like "- do_file_read\n- do_bash".
            let mut items: Vec<String> = Vec::new();
            let mut j = i + 1;
            while j < raw_lines.len() {
                let l = raw_lines[j];
                if l.trim().is_empty() {
                    j += 1;
                    continue;
                }
                let li = l.len() - l.trim_start().len();
                let lt = l.trim();
                if li >= indent {
                    if let Some(item) = lt.strip_prefix("- ") {
                        items.push(
                            item.trim()
                                .trim_matches('"')
                                .trim_matches('\'')
                                .to_string(),
                        );
                        j += 1;
                        continue;
                    }
                    if !items.is_empty() && li > indent {
                        if let Some(last) = items.last_mut() {
                            last.push(' ');
                            last.push_str(lt);
                        }
                        j += 1;
                        continue;
                    }
                }
                break;
            }
            if !items.is_empty() {
                map.insert(key, items.join(", "));
                i = j;
            } else {
                map.insert(key, String::new());
                i += 1;
            }
            continue;
        }

        map.insert(key, val);
        i += 1;
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
///
/// Unicode letters and digits are kept (`代码审查` is a perfectly good skill
/// name — the old ASCII-only map turned it into `------`, filtered every empty
/// segment and errored out, so Chinese-speaking users could not create a skill
/// at all). Only separators and everything else collapse to `-`.
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
        .flat_map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c.to_lowercase().collect::<Vec<char>>()
            } else {
                // whitespace and every other separator/punctuation
                vec!['-']
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
    // Count chars, not bytes: 64 CJK characters is a sane name, but 64 *bytes*
    // is only ~21 of them.
    if cleaned.chars().count() > 64 {
        return Err("Skill 名称过长（最多 64 个字符）".into());
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

/// Best-effort freshness of a skill package: SKILL.md mtime in seconds (0 if unknown).
fn skill_freshness(s: &Skill) -> i64 {
    file_mtime_secs(&s.root.join("SKILL.md"))
}

/// File modification time in seconds since epoch (0 if unknown/error).
fn file_mtime_secs(p: &std::path::Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Install skill packages from a GitHub-style spec into `~/.dscode/skills`.
fn install_skill_spec(spec: &str) -> Result<InstallReport, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("请提供包标识，例如 vercel-labs/agent-skills 或 owner/repo/skill-name".into());
    }

    let (owner, repo, subpath) = parse_github_spec(spec)?;
    let source_desc = format!("github.com/{owner}/{repo}");
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
    // Removes the temp clone on every exit path, including `?` — the old code
    // leaked %TEMP%/dscode-skill-install-* whenever copy_dir_recursive failed.
    let _tmp_guard = TempDirGuard(tmp.clone());

    let url = format!("https://github.com/{owner}/{repo}.git");
    let repo_dir = tmp.join("repo");
    // Shallow clone only — we never execute remote scripts during install.
    let (clone_ok, _clone_out, clone_err) = run_git_bounded(
        &[
            "clone",
            "--depth",
            "1",
            "--quiet",
            &url,
            repo_dir.to_str().unwrap_or("repo"),
        ],
        None,
        GIT_CLONE_TIMEOUT_SECS,
    )?;
    if !clone_ok {
        return Err(format!(
            "git clone 失败: {url}{}\n请检查网络与仓库是否存在，或手动: npx skills add {owner}/{repo}",
            tail_suffix(&clone_err)
        ));
    }

    let repo_root = repo_dir;
    let search_root = if let Some(ref sub) = subpath {
        let p = repo_root.join(sub);
        if !p.exists() {
            return Err(format!("仓库内找不到路径: {sub}"));
        }
        // Second line of defence behind parse_github_spec's `..` check:
        // resolve symlinks and require the result to stay inside the clone.
        let root_canon = std::fs::canonicalize(&repo_root)
            .map_err(|e| format!("无法解析仓库目录: {e}"))?;
        let canon = std::fs::canonicalize(&p)
            .map_err(|e| format!("无法解析路径 {sub}: {e}"))?;
        if !canon.starts_with(&root_canon) {
            return Err(format!(
                "拒绝安装：路径 `{sub}` 逃出了仓库目录（{}）",
                canon.display()
            ));
        }
        canon
    } else {
        std::fs::canonicalize(&repo_root).unwrap_or_else(|_| repo_root.clone())
    };

    // Remote revision of the clone — the anchor for "is a reinstall actually
    // newer?". Never compare mtimes: the just-cloned SKILL.md is always
    // "newer" than the installed copy, which silently destroyed local edits.
    let src_commit = run_git_bounded(&["rev-parse", "HEAD"], Some(&repo_root), GIT_META_TIMEOUT_SECS)
        .ok()
        .filter(|(ok, _, _)| *ok)
        .map(|(_, out, _)| out.trim().to_string())
        .filter(|s| !s.is_empty());

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
            // Same-name package already present. Decide from the recorded
            // remote revision, never from mtime, and always keep a backup of
            // whatever was there (the old code removed it outright, so local
            // edits were gone with no way back).
            let prev = read_install_marker(&dest);
            let same_revision = match (&prev, src_commit.as_deref()) {
                (Some(p), Some(cur)) => p.commit == cur,
                _ => false,
            };
            if same_revision {
                skipped.push(format!(
                    "{safe} (远端仍是已安装的版本 {}，未覆盖，保留本地修改)",
                    short_commit(src_commit.as_deref().unwrap_or(""))
                ));
                continue;
            }
            let backup = backup_skill_dir(&dest, &safe)?;
            replace_dir(&dest, pkg)?;
            let note = match (&prev, src_commit.as_deref()) {
                (Some(p), Some(cur)) => format!(
                    "已更新 {} → {}",
                    short_commit(&p.commit),
                    short_commit(cur)
                ),
                (None, Some(cur)) => format!(
                    "已安装 {}（原本地副本没有安装记录，来源未知）",
                    short_commit(cur)
                ),
                _ => "已覆盖（无法确定远端版本）".to_string(),
            };
            if let Some(cur) = src_commit.as_deref() {
                write_install_marker(&dest, &source_desc, cur)?;
            }
            installed.push(format!(
                "{safe} ({note}；本地旧副本已备份到 {})",
                backup.display()
            ));
            continue;
        }
        copy_dir_recursive(pkg, &dest)?;
        if let Some(cur) = src_commit.as_deref() {
            write_install_marker(&dest, &source_desc, cur)?;
        }
        installed.push(safe);
    }

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
        source_dir: source_desc,
        target_dir: target_root.display().to_string(),
        message,
    })
}

/// Removes a temporary directory on drop (all `?` paths included).
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Timeout for the initial `git clone` (network + npx-free, but proxies vary).
const GIT_CLONE_TIMEOUT_SECS: u64 = 180;
/// Timeout for cheap local `git` metadata reads (`rev-parse`).
const GIT_META_TIMEOUT_SECS: u64 = 30;

/// Marker file recording which remote revision an installed package came from.
const INSTALL_MARKER: &str = ".dscode-install.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct InstallMarker {
    source: String,
    commit: String,
}

/// Run `git` with a hard timeout, no credential prompt and no stdin.
///
/// `Command::status()` had none of these: on a private/deleted repo or a
/// black-holing proxy git blocks at the credential prompt (it reads `/dev/tty`,
/// so a null stdin alone does not save you) and, being synchronous inside
/// `async fn execute`, the ReAct loop hung with no way to cancel.
fn run_git_bounded(
    args: &[&str],
    cwd: Option<&Path>,
    timeout_secs: u64,
) -> Result<(bool, String, String), String> {
    let mut git = std::process::Command::new("git");
    git.args(args);
    if let Some(dir) = cwd {
        git.current_dir(dir);
    }
    git.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .env("GCM_INTERACTIVE", "never")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Hide the console window when the desktop app runs `git` on Windows.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        git.creation_flags(crate::tools::bash::CREATE_NO_WINDOW);
    }
    // Optional proxy for skill downloads
    if let Ok(cfg) = crate::config::settings::Config::load() {
        crate::config::settings::apply_proxy_env(&mut git, cfg.proxy_for_skills());
    }
    let mut child = git.spawn().map_err(|e| {
        format!("无法运行 git（安装第三方 skill 需要本机有 git）: {e}")
    })?;
    // Drain both pipes on threads so a chatty git can never block on a full
    // pipe while we poll for exit.
    let out_handle = child.stdout.take().map(|mut p| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut s = String::new();
            let _ = p.read_to_string(&mut s);
            s
        })
    });
    let err_handle = child.stderr.take().map(|mut p| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut s = String::new();
            let _ = p.read_to_string(&mut s);
            s
        })
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let tail = err_handle.and_then(|h| h.join().ok()).unwrap_or_default();
                    return Err(format!(
                        "git {} 超时（{timeout_secs}s）{}",
                        args.first().copied().unwrap_or(""),
                        tail_suffix(&tail)
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(format!("等待 git 进程失败: {e}"));
            }
        }
    };
    let stdout = out_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = err_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok((status.success(), stdout, stderr))
}

/// Last ~400 chars of a command's stderr, prefixed for an error message.
fn tail_suffix(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        return String::new();
    }
    let tail: String = t.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
    format!("\n{tail}")
}

fn short_commit(c: &str) -> String {
    c.chars().take(8).collect()
}

fn read_install_marker(dest: &Path) -> Option<InstallMarker> {
    let raw = std::fs::read_to_string(dest.join(INSTALL_MARKER)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_install_marker(dest: &Path, source: &str, commit: &str) -> Result<(), String> {
    let marker = InstallMarker {
        source: source.to_string(),
        commit: commit.to_string(),
    };
    let raw = serde_json::to_string_pretty(&marker).map_err(|e| e.to_string())?;
    let path = dest.join(INSTALL_MARKER);
    std::fs::write(&path, raw).map_err(|e| format!("无法写入安装标记 {:?}: {e}", path))
}

/// Move an installed package out of the skills tree into `<data>/skill-backups`
/// before it gets replaced. The backup lives outside every search root so it is
/// not itself loaded as a skill.
///
/// A rename is used when possible: it is atomic, keeps every file (including
/// symlinks) byte-for-byte, and fails *before* anything is touched when a file
/// is locked. The copy fallback skips symlinks on purpose — following them
/// would drag arbitrary host trees into the backup.
fn backup_skill_dir(dest: &Path, safe_name: &str) -> Result<PathBuf, String> {
    let base = crate::config::settings::Config::data_dir()
        .map(|d| d.join("skill-backups"))
        .unwrap_or_else(|_| std::env::temp_dir().join("dscode-skill-backups"));
    std::fs::create_dir_all(&base)
        .map_err(|e| format!("无法创建备份目录 {:?}: {e}", base))?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let mut backup = base.join(format!("{safe_name}-{stamp}"));
    let mut n = 1;
    while path_present(&backup) && n <= 100 {
        backup = base.join(format!("{safe_name}-{stamp}-{n}"));
        n += 1;
    }
    match std::fs::rename(dest, &backup) {
        Ok(()) => Ok(backup),
        Err(rename_err) => {
            copy_dir_recursive(dest, &backup).map_err(|e| {
                format!(
                    "备份 {:?} 失败: {e}（rename 也失败: {rename_err}）——已放弃覆盖",
                    dest
                )
            })?;
            Ok(backup)
        }
    }
}

/// Delete the old package (already backed up) and copy the new one in.
///
/// Removal failure is an error: the old `let _ = remove_dir_all(&dest)`
/// swallowed it and then merged the new files into the stale tree on Windows
/// when a file was locked.
fn replace_dir(dest: &Path, src: &Path) -> Result<(), String> {
    if path_present(dest) {
        std::fs::remove_dir_all(dest).map_err(|e| {
            format!(
                "无法删除旧版本 {}：{e}（文件可能被占用）。本地旧版本已备份，未覆盖。",
                dest.display()
            )
        })?;
    }
    copy_dir_recursive(src, dest)
}

/// Parse `owner/repo`, `owner/repo/sub/path`, or GitHub URL.
///
/// The subpath is validated here (and again by canonical containment after the
/// clone) because `repo_root.join(sub)` with `sub = "../../../../home/user"`
/// escapes the temporary clone: an arbitrary host directory that happens to
/// contain a SKILL.md would be packaged into `~/.dscode/skills`.
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
    if owner.contains('\\') || repo.contains('\\') || owner.contains(':') || repo.contains(':') {
        return Err("非法仓库名".into());
    }
    let sub = if parts.len() > 2 {
        let joined = parts[2..].join("/");
        for comp in joined.split('/') {
            if comp == ".." {
                return Err(format!("非法路径（不允许 `..`）: {joined}"));
            }
            if comp.contains('\\') || comp.contains(':') {
                return Err(format!("非法路径: {joined}"));
            }
        }
        Some(joined)
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
        // symlink_metadata, never `path.is_dir()`: a cloned repo can contain
        // `evil -> /home/user/some-dir`, and following it would package an
        // arbitrary host directory into the skills tree.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            tracing::warn!(path = %path.display(), "skip symlink while scanning for skill packages");
            continue;
        }
        if meta.file_type().is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            find_skill_packages(&path, out, depth + 1);
        }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    // Refuse a symlinked package root outright — read_dir() would follow it.
    if is_symlink(src) {
        return Err(format!(
            "拒绝复制符号链接 {}：技能包不能是符号链接",
            src.display()
        ));
    }
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {:?}: {e}", dst))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("read {:?}: {e}", src))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().ok_or("bad name")?;
        let target = dst.join(name);
        // Copy regular files/dirs only. `path.is_dir()` / `is_file()` follow
        // symlinks, which let a malicious repo plant
        // `scripts/keys -> /home/user/.ssh` and have the private keys copied
        // into ~/.dscode/skills/<name>/scripts/.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => return Err(format!("stat {:?}: {e}", path)),
        };
        if meta.file_type().is_symlink() {
            tracing::warn!(path = %path.display(), "skip symlink in skill package copy");
            continue;
        }
        if meta.file_type().is_dir() {
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
        // symlink_metadata: never walk *through* a symlinked scripts/ dir —
        // `scripts -> /home/user/.ssh` would otherwise put the user's private
        // keys (with absolute paths) into the system prompt.
        if !is_real_dir(&base) {
            continue;
        }
        walk_resources(&base, root, kind, &mut out, 0);
    }
    // Also pick up root-level scripts (e.g. run.sh next to SKILL.md)
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() || !meta.file_type().is_file() {
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

/// True only for a real directory entry (symlinks to directories excluded).
fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false)
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
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            tracing::warn!(path = %path.display(), "skip symlink in skill resource scan");
            continue;
        }
        if meta.file_type().is_dir() {
            walk_resources(&path, root, kind, out, depth + 1);
        } else if meta.file_type().is_file() {
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
    let executable = looks_like_script(path.file_name().and_then(|n| n.to_str()).unwrap_or(""));
    // Shadowed under cfg so the Windows build does not see a binding that is
    // never reassigned.
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        executable
            || std::fs::metadata(path)
                .map(|m| (m.permissions().mode() & 0o111) != 0)
                .unwrap_or(false)
    };
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
///
/// Must be the exact inverse of [`yaml_quote`]. The previous implementation
/// ran `.replace("\\n", "\n")` *before* `.replace("\\\\", "\\")` as a chained
/// pass, so a Windows path written as `"C:\\new\\bin"` came back as
/// `C:<newline>ew<newline>bin`. Scanning escapes left-to-right in one pass is
/// order-correct by construction.
fn unquote_yaml(s: &str) -> String {
    // Trim only for *detecting* a quoted scalar. `get_field` funnels every
    // value through here, including block scalars, whose chomped trailing
    // newline a blanket `trim()` silently ate (`|` must keep one).
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        let inner = &t[1..t.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    // Unknown escape — keep it verbatim rather than eating it.
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
        return out;
    }
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        return t[1..t.len() - 1].to_string();
    }
    // Plain scalars are already trimmed by the parser; returning `s` verbatim
    // keeps a block scalar's trailing newline intact.
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
        #[cfg(unix)]
        {
            let link = skills.join("real-skill");
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

    #[test]
    fn sanitize_name_allows_unicode() {
        assert_eq!(sanitize_skill_name("Code Review").unwrap(), "code-review");
        assert_eq!(sanitize_skill_name("代码审查").unwrap(), "代码审查");
        assert_eq!(sanitize_skill_name("代码 审查").unwrap(), "代码-审查");
        assert!(sanitize_skill_name("../x").is_err());
        assert!(sanitize_skill_name("x/y").is_err());
    }

    #[test]
    fn frontmatter_bom_block_scalar_and_dashes_in_value() {
        let content = "\u{feff}---\nname: 代码审查\ndescription: a---b\ntriggers:\n- 代码审查\n- code review\nallowed-tools:\n- do_file_read\n- do_bash\nbody-note: |\n  first line\n  second line\n---\n\nbody text\n";
        let (fm, body) = parse_yaml_frontmatter(content).unwrap();
        assert_eq!(get_field(&fm, "name").as_deref(), Some("代码审查"));
        // `---` inside a value must not terminate the frontmatter
        assert_eq!(get_field(&fm, "description").as_deref(), Some("a---b"));
        assert_eq!(
            get_field(&fm, "triggers").as_deref(),
            Some("代码审查, code review")
        );
        assert_eq!(
            get_field(&fm, "allowed-tools").as_deref(),
            Some("do_file_read, do_bash")
        );
        assert_eq!(
            get_field(&fm, "body-note").as_deref(),
            Some("first line\nsecond line\n")
        );
        assert_eq!(body, "body text");
    }

    #[test]
    fn yaml_quote_unquote_roundtrip() {
        for s in [
            r"Run scripts in C:\new\bin",
            "line1\nline2",
            r#"quote " and backslash \"#,
            r"regex: ^a\\d+$",
        ] {
            assert_eq!(unquote_yaml(&yaml_quote(s)), s, "roundtrip failed for {s:?}");
        }
    }

    #[test]
    fn delete_guard_rejects_search_root() {
        let tmp = env::temp_dir().join(format!("dscode-del-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let skills = tmp.join("skills");
        let pkg = skills.join("good");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("SKILL.md"),
            "---\nname: good\ndescription: t\n---\n\nbody\n",
        )
        .unwrap();

        // The root itself is never "under" itself, so it can never be a target.
        assert!(!is_under_any_skills_root(&skills, &[skills.clone()]));
        assert!(is_under_any_skills_root(&pkg, &[skills.clone()]));
        // `<root>/x/..` resolves back to the root — must not count as inside.
        assert!(!is_strictly_under(
            Path::new("/a/skills/x/.."),
            Path::new("/a/skills")
        ));
        // A directory without SKILL.md is not deletable, whatever the caller says.
        assert!(validate_skill_target(&skills, "good").is_err());
        assert!(validate_skill_target(&pkg, "something-else").is_err());
        assert!(validate_skill_target(&pkg, "good").is_ok());
        assert!(remove_dir_all_robust(&skills).is_err());
        assert!(skills.exists(), "non-skill dir must not be removed");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_github_spec_rejects_escaping_subpath() {
        assert!(parse_github_spec("owner/repo").is_ok());
        assert!(parse_github_spec("owner/repo/skill-name").is_ok());
        assert!(parse_github_spec("owner/repo/../../../../home/user").is_err());
        assert!(parse_github_spec("owner/repo/a/../b").is_err());
        assert!(parse_github_spec("owner/repo/..\\..\\windows").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn copy_and_scan_skip_symlinks() {
        let tmp = env::temp_dir().join(format!("dscode-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let src = tmp.join("src");
        let outside = tmp.join("outside");
        std::fs::create_dir_all(src.join("scripts")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("id_rsa"), "PRIVATE KEY\n").unwrap();
        std::fs::write(
            src.join("SKILL.md"),
            "---\nname: evil\ndescription: t\n---\n\nbody\n",
        )
        .unwrap();
        // Directory symlink and file symlink pointing outside the package.
        std::os::unix::fs::symlink(&outside, src.join("scripts").join("keys")).unwrap();
        std::os::unix::fs::symlink(
            outside.join("id_rsa"),
            src.join("scripts").join("leak.sh"),
        )
        .unwrap();

        let resources = scan_skill_resources(&src);
        assert!(
            resources.iter().all(|r| !r.relative_path.contains("keys")
                && !r.relative_path.contains("leak.sh")),
            "symlinks must not be listed as resources: {resources:?}"
        );

        let dst = tmp.join("dst");
        copy_dir_recursive(&src, &dst).unwrap();
        assert!(!path_present(&dst.join("scripts").join("keys")));
        assert!(!path_present(&dst.join("scripts").join("leak.sh")));
        assert!(!dst.join("scripts").join("id_rsa").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
