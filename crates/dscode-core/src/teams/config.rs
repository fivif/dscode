//! Teams v2 configuration.

use serde::{Deserialize, Serialize};

/// Config section under `[teams]` in config.toml (also nested default on Config).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamsConfig {
    /// Use TeamRuntime instead of legacy run_teams_task_v1.
    #[serde(default = "default_v2_true")]
    pub v2_enabled: bool,
    /// Multi-wave research→implement (requires MULTI_WAVE_IMPLEMENTED).
    #[serde(default)]
    pub waves_enabled: bool,
    /// Enforce owned_paths for implement agents.
    #[serde(default)]
    pub ownership_enforced: bool,
    /// Soft log only (warn) instead of Denied when ownership_enforced.
    #[serde(default = "default_true")]
    pub ownership_soft_log_only: bool,
    /// Allow do_bash for explore agents.
    #[serde(default)]
    pub explore_bash: bool,
    /// Max concurrent sub-agents.
    #[serde(default = "default_max_parallel")]
    pub max_parallel: usize,
    /// Max agents / tasks per board.
    #[serde(default = "default_max_agents")]
    pub max_agents: usize,
    /// Persist TaskBoard JSON for debugging.
    #[serde(default)]
    pub persist_board: bool,
}

fn default_v2_true() -> bool {
    // Always on — v1 path retired; flag retained for config compat only.
    true
}
fn default_true() -> bool {
    true
}
fn default_max_parallel() -> usize {
    6
}
fn default_max_agents() -> usize {
    8
}

impl Default for TeamsConfig {
    fn default() -> Self {
        Self {
            v2_enabled: true,
            waves_enabled: true,
            ownership_enforced: false,
            ownership_soft_log_only: true,
            explore_bash: false,
            max_parallel: 6,
            max_agents: 8,
            persist_board: false,
        }
    }
}

/// Multi-wave is fully implemented in this codebase.
pub const MULTI_WAVE_IMPLEMENTED: bool = true;

impl TeamsConfig {
    pub fn effective_waves(&self) -> bool {
        self.waves_enabled && MULTI_WAVE_IMPLEMENTED
    }

    pub fn max_parallel_capped(&self) -> usize {
        self.max_parallel.clamp(1, 8)
    }

    pub fn max_agents_capped(&self) -> usize {
        self.max_agents.clamp(1, 12)
    }
}
