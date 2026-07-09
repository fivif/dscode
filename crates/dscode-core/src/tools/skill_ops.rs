//! Skill tools — list installed packages and install third-party skills
//! (skills.sh / GitHub ecosystem, SKILL.md packages).

use async_trait::async_trait;
use serde_json::json;

use super::trait_def::{Tool, ToolContext, ToolError, ToolResult};
use crate::extensions::skills::SkillLoader;

/// List installed skills from all search paths (dscode / claude / agents / project).
pub struct DoSkillList;

impl DoSkillList {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DoSkillList {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DoSkillList {
    fn name(&self) -> &str {
        "do_skill_list"
    }

    fn description(&self) -> &str {
        "List installed Agent Skills (from ~/.dscode/skills, ~/.claude/skills, \
         ~/.agents/skills, and project .claude/skills). Shows name, triggers, and scripts."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Optional filter substring for name/description/triggers"
                }
            }
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let q = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        let mut loader = SkillLoader::new();
        let n = loader
            .load_all(&[], Some(&ctx.working_dir))
            .map_err(|e| ToolError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

        let mut lines = vec![format!(
            "Installed skills: {n} (search paths include ~/.dscode/skills, ~/.claude/skills, ~/.agents/skills)\n"
        )];
        for s in loader.list_all() {
            if s.hidden {
                continue;
            }
            if !q.is_empty() {
                let blob = format!(
                    "{} {} {}",
                    s.name,
                    s.description,
                    s.triggers.join(" ")
                )
                .to_lowercase();
                if !blob.contains(&q) {
                    continue;
                }
            }
            let scripts = s
                .resources
                .iter()
                .filter(|r| {
                    matches!(
                        r.kind,
                        crate::extensions::skills::SkillResourceKind::Script
                    )
                })
                .count();
            let desc: String = if s.description.is_empty() {
                "(no description)".into()
            } else {
                s.description.chars().take(120).collect()
            };
            let trig = if s.triggers.is_empty() {
                "(none)".into()
            } else {
                s.triggers.join(", ")
            };
            lines.push(format!(
                "- **{}** — {}\n  triggers: {}\n  scripts: {} · root: `{}`",
                s.name,
                desc,
                trig,
                scripts,
                s.root.display()
            ));
        }
        if lines.len() == 1 {
            lines.push(
                "No skills matched. Install from https://www.skills.sh/ with do_skill_install \
                 (e.g. `vercel-labs/agent-skills` or `mattpocock/skills/grill-me`)."
                    .into(),
            );
        }
        Ok(ToolResult::ok(lines.join("\n")))
    }
}

/// Install a third-party skill package from GitHub (skills.sh ecosystem).
pub struct DoSkillInstall;

impl DoSkillInstall {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DoSkillInstall {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DoSkillInstall {
    fn name(&self) -> &str {
        "do_skill_install"
    }

    fn description(&self) -> &str {
        "Install a third-party Agent Skill package from GitHub into ~/.dscode/skills. \
         Spec formats: `owner/repo`, `owner/repo/skill-path`, or GitHub URL. \
         Examples: `vercel-labs/agent-skills`, `mattpocock/skills/grill-me`, \
         `anthropics/skills`. Browse catalog at https://www.skills.sh/ . \
         Does not execute remote scripts during install — only copies SKILL.md packages. \
         Ask the user before installing unless they explicitly requested it."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "string",
                    "description": "Package spec: owner/repo or owner/repo/skill-subdir (from skills.sh)"
                }
            },
            "required": ["package"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let package = args
            .get("package")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::MissingParameter("package".into()))?
            .trim();
        if package.is_empty() {
            return Err(ToolError::InvalidParameter {
                name: "package".into(),
                reason: "must not be empty".into(),
            });
        }

        // Block obviously dangerous specs
        let lower = package.to_lowercase();
        if lower.contains("javascript:") || lower.contains("|") || lower.contains(';') {
            return Err(ToolError::InvalidParameter {
                name: "package".into(),
                reason: "非法 package 字符串".into(),
            });
        }

        let report = SkillLoader::install_from_spec(package).map_err(|e| {
            ToolError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
        })?;

        Ok(ToolResult::ok(format!(
            "{}\n\nInstalled: {:?}\nSkipped: {:?}\nSource: {}\nTarget: {}\n\n\
             Tip: next user messages that match skill triggers will auto-activate. \
             Use do_skill_list to verify. Catalog: https://www.skills.sh/",
            report.message, report.installed, report.skipped, report.source_dir, report.target_dir
        )))
    }
}
