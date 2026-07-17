//! Agent roles and tool allow/deny matrices for sub-agents.

use serde::{Deserialize, Serialize};

/// Sub-agent capability role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Explore,
    Implement,
    Verify,
}

impl AgentRole {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentRole::Explore => "explore",
            AgentRole::Implement => "implement",
            AgentRole::Verify => "verify",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "explore" | "research" => Some(AgentRole::Explore),
            "implement" | "impl" | "write" => Some(AgentRole::Implement),
            "verify" | "test" => Some(AgentRole::Verify),
            _ => None,
        }
    }

    /// Default max ReAct iterations for this role.
    pub fn max_iterations(self) -> u32 {
        match self {
            AgentRole::Explore => 30,
            AgentRole::Implement => 80,
            AgentRole::Verify => 40,
        }
    }

    /// Short system prompt fragment.
    pub fn system_fragment(self) -> &'static str {
        match self {
            AgentRole::Explore => {
                "You are an EXPLORE sub-agent (READ-ONLY). Search and report findings only. \
                 Do not modify files. End with a concise report of paths and facts."
            }
            AgentRole::Implement => {
                "You are an IMPLEMENT sub-agent. Complete your assigned coding task fully. \
                 Prefer editing existing files. Output a brief confirmation of what you changed."
            }
            AgentRole::Verify => {
                "You are a VERIFY sub-agent. Run tests / inspect diffs. Do not change product code. \
                 Report pass/fail with evidence."
            }
        }
    }
}

/// Build allowlist for a role. `explore_bash` enables do_bash for Explore.
pub fn tool_names_for_role(role: AgentRole, explore_bash: bool) -> RoleToolPolicy {
    match role {
        AgentRole::Explore => {
            let mut allow = vec![
                "do_file_read".to_string(),
                "do_skill_list".to_string(),
                "do_web_fetch".to_string(),
                "do_web_search".to_string(),
            ];
            if explore_bash {
                allow.push("do_bash".to_string());
            }
            RoleToolPolicy::Allowlist(allow)
        }
        AgentRole::Implement => {
            // Full tools except skill install
            RoleToolPolicy::Denylist(vec!["do_skill_install".to_string()])
        }
        AgentRole::Verify => RoleToolPolicy::Allowlist(vec![
            "do_file_read".to_string(),
            "do_bash".to_string(),
            "do_skill_list".to_string(),
            "do_task_status".to_string(),
            "do_web_fetch".to_string(),
            "do_web_search".to_string(),
        ]),
    }
}

#[derive(Debug, Clone)]
pub enum RoleToolPolicy {
    Allowlist(Vec<String>),
    Denylist(Vec<String>),
}

/// Filter a name list for MCP: Implement only may use mcp_*.
pub fn allows_mcp(role: AgentRole) -> bool {
    matches!(role, AgentRole::Implement)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explore_no_write_tools() {
        match tool_names_for_role(AgentRole::Explore, false) {
            RoleToolPolicy::Allowlist(a) => {
                assert!(a.contains(&"do_file_read".into()));
                assert!(!a.iter().any(|n| n.contains("write") || n.contains("edit")));
            }
            _ => panic!("expected allowlist"),
        }
    }

    #[test]
    fn implement_denies_skill_install() {
        match tool_names_for_role(AgentRole::Implement, false) {
            RoleToolPolicy::Denylist(d) => assert!(d.contains(&"do_skill_install".into())),
            _ => panic!("expected denylist"),
        }
    }
}
