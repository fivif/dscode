//! SKILL.md loader — cc-switch compatible skill system.
//!
//! Skills are loaded from `~/.dscode/skills/<name>/SKILL.md` files.
//! Compatible with the cc-switch SKILL.md format (YAML frontmatter).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A loaded skill with metadata and instructions.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub hidden: bool,
    pub body: String,
    pub path: PathBuf,
}

/// Manages a collection of loaded skills from a directory tree.
pub struct SkillLoader {
    skills: Vec<Skill>,
}

impl SkillLoader {
    pub fn new() -> Self { Self { skills: vec![] } }

    /// Load all SKILL.md files from a directory recursively.
    /// Directory structure: `<dir>/<skill-name>/SKILL.md`
    pub fn load_from_dir(&mut self, dir: &Path) -> Result<usize, String> {
        if !dir.exists() {
            std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create skills dir: {}", e))?;
            return Ok(0);
        }
        let mut count = 0;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let skill_md = path.join("SKILL.md");
                    if skill_md.exists() {
                        match Self::parse_file(&skill_md) {
                            Ok(skill) => { self.skills.push(skill); count += 1; }
                            Err(e) => tracing::warn!("Failed to load skill {:?}: {}", skill_md, e),
                        }
                    } else {
                        // Recurse into subdirectories for nested skill trees
                        count += self.load_from_dir(&path)?;
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
        // Triggers from description: extract key phrases
        let triggers = extract_triggers(&description);
        let allowed_tools = get_field(&frontmatter, "allowed-tools")
            .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
            .unwrap_or_default();
        let hidden = get_field(&frontmatter, "hidden")
            .map(|s| s == "true")
            .unwrap_or(false);

        Ok(Skill { name, description, triggers, allowed_tools, hidden, body, path: path.to_path_buf() })
    }

    /// Find skills matching a user message (checks all trigger keywords).
    pub fn find_matching(&self, message: &str) -> Vec<&Skill> {
        let msg_lower = message.to_lowercase();
        let mut matches: Vec<&Skill> = self.skills.iter()
            .filter(|s| s.triggers.iter().any(|t| msg_lower.contains(&t.to_lowercase())))
            .collect();
        // Sort by trigger match length (longer = more specific)
        matches.sort_by(|a, b| {
            let a_len = a.triggers.iter().map(|t| t.len()).max().unwrap_or(0);
            let b_len = b.triggers.iter().map(|t| t.len()).max().unwrap_or(0);
            b_len.cmp(&a_len)
        });
        matches
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
        crate::config::settings::Config::data_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("skills")
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
            } else if !current_key.is_empty() && in_multiline {
                if !current_value.is_empty() { current_value.push('\n'); }
                current_value.push_str(trimmed);
            }
        } else {
            // Multi-line value: continue accumulating
            if !current_value.is_empty() { current_value.push('\n'); }
            current_value.push_str(trimmed);
            // Check if next line would be a new key
            in_multiline = true; // stays true until we see a key: value
        }
    }
    if !current_key.is_empty() {
        map.insert(current_key, current_value.trim().to_string());
    }

    Ok((map, body))
}

fn get_field(fm: &HashMap<String, String>, key: &str) -> Option<String> {
    fm.get(key).cloned().filter(|v| !v.is_empty())
}

/// Extract trigger keywords from a description field.
fn extract_triggers(desc: &str) -> Vec<String> {
    let desc_lower = desc.to_lowercase();
    let mut triggers: Vec<String> = vec![];
    // Extract quoted phrases
    let mut in_quote = false;
    let mut current_phrase = String::new();
    for ch in desc_lower.chars() {
        if ch == '"' {
            if in_quote && !current_phrase.is_empty() {
                triggers.push(current_phrase.clone());
                current_phrase.clear();
            }
            in_quote = !in_quote;
        } else if in_quote {
            current_phrase.push(ch);
        }
    }
    // If no quoted phrases, use the first sentence
    if triggers.is_empty() {
        if let Some(first) = desc_lower.split('.').next() {
            // Extract key words (3+ chars)
            triggers.extend(first.split(' ')
                .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
                .filter(|w| w.len() >= 4)
                .map(|w| w.to_string()));
        }
    }
    triggers
}
