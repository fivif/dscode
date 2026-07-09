//! Lead orchestration phase (not the DAG scheduler).

/// Where the Lead (coordinator) currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadPhase {
    Idle,
    Decomposing,
    Researching,
    Synthesizing,
    Implementing,
    Verifying,
    Merging,
    Completed,
    Failed,
}

impl LeadPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            LeadPhase::Idle => "idle",
            LeadPhase::Decomposing => "decomposing",
            LeadPhase::Researching => "researching",
            LeadPhase::Synthesizing => "synthesizing",
            LeadPhase::Implementing => "implementing",
            LeadPhase::Verifying => "verifying",
            LeadPhase::Merging => "merging",
            LeadPhase::Completed => "completed",
            LeadPhase::Failed => "failed",
        }
    }
}
