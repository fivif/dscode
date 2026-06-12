//! Plan phase state machine — five distinct stages from initial understanding
//! through final approval. The state machine governs the planning interview
//! process and persists intermediate state so long-running plans survive restarts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The five phases of the plan engine.
///
/// Progression is linear but phases can cycle back on revisions:
/// `Review → Design` when the review uncovers flaws, or
/// `Approved → Design` when a human rejects the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPhase {
    /// Gather requirements, ask clarifying questions, explore the codebase.
    InitialUnderstanding,
    /// Architect the solution: component design, data flow, interface surfaces.
    Design,
    /// Validate the design against requirements and constraints.
    Review,
    /// Produce the final, concrete PRD with file paths, estimates, and steps.
    FinalPlan,
    /// The plan has been accepted (by human or auto-approval).
    Approved,
}

impl PlanPhase {
    /// Returns the next phase in the default progression.
    pub fn next(self) -> Option<PlanPhase> {
        match self {
            PlanPhase::InitialUnderstanding => Some(PlanPhase::Design),
            PlanPhase::Design => Some(PlanPhase::Review),
            PlanPhase::Review => Some(PlanPhase::FinalPlan),
            PlanPhase::FinalPlan => Some(PlanPhase::Approved),
            PlanPhase::Approved => None,
        }
    }

    /// Returns whether this phase has a natural successor.
    pub fn can_advance(self) -> bool {
        self.next().is_some()
    }

    /// Human-readable phase label.
    pub fn label(self) -> &'static str {
        match self {
            PlanPhase::InitialUnderstanding => "Initial Understanding",
            PlanPhase::Design => "Design",
            PlanPhase::Review => "Review",
            PlanPhase::FinalPlan => "Final Plan",
            PlanPhase::Approved => "Approved",
        }
    }
}

impl std::fmt::Display for PlanPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The complete state of a running plan session.
///
/// This struct is the source of truth for the plan engine. It tracks which
/// phase we are in, how many questions have been asked, how many remain, and
/// carries the draft PRD once one has been generated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanState {
    /// Current phase of the planning process.
    pub phase: PlanPhase,

    /// Unique identifier for this plan (typically a UUID).
    pub plan_id: String,

    /// Brief description of what the plan aims to accomplish.
    pub title: String,

    /// Total number of questions asked so far (across all phases).
    pub questions_asked: u32,

    /// Estimated number of remaining questions before the current phase can complete.
    pub questions_remaining: u32,

    /// The draft PRD document, once the FinalPlan phase has produced one.
    pub draft_prd: Option<super::prd::PrdDocument>,

    /// Arbitrary metadata gathered during the interview (key-value pairs).
    pub metadata: HashMap<String, String>,

    /// Timestamp when the plan was created.
    pub created_at: DateTime<Utc>,

    /// Timestamp of the last state mutation.
    pub updated_at: DateTime<Utc>,

    /// Parent task ID from the task system, if this plan was spawned from a task.
    pub task_id: Option<String>,
}

impl PlanState {
    /// Create a new plan state in the InitialUnderstanding phase.
    pub fn new(plan_id: String, title: String) -> Self {
        let now = Utc::now();
        Self {
            phase: PlanPhase::InitialUnderstanding,
            plan_id,
            title,
            questions_asked: 0,
            questions_remaining: 5,
            draft_prd: None,
            metadata: HashMap::new(),
            created_at: now,
            updated_at: now,
            task_id: None,
        }
    }

    /// Advance to the next phase, returning the new phase.
    ///
    /// Returns `None` if already in `Approved`.
    pub fn advance_phase(&mut self) -> Option<PlanPhase> {
        let next = self.phase.next()?;
        self.phase = next;
        self.updated_at = Utc::now();
        Some(next)
    }

    /// Go back to a specific phase (e.g., from Review back to Design).
    pub fn retreat_to(&mut self, phase: PlanPhase) {
        self.phase = phase;
        self.updated_at = Utc::now();
    }

    /// Record that a question was asked.
    pub fn question_asked(&mut self) {
        self.questions_asked += 1;
        self.questions_remaining = self.questions_remaining.saturating_sub(1);
        self.updated_at = Utc::now();
    }

    /// Set the expected number of remaining questions.
    pub fn set_remaining_questions(&mut self, count: u32) {
        self.questions_remaining = count;
        self.updated_at = Utc::now();
    }

    /// Attach a task ID to this plan.
    pub fn with_task_id(mut self, task_id: String) -> Self {
        self.task_id = Some(task_id);
        self
    }

    /// Store arbitrary metadata.
    pub fn set_meta(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.metadata.insert(key.into(), value.into());
        self.updated_at = Utc::now();
    }

    /// Returns true if the plan has produced a PRD.
    pub fn has_prd(&self) -> bool {
        self.draft_prd.is_some()
    }

    /// Returns true if the plan is complete (Approved).
    pub fn is_complete(&self) -> bool {
        self.phase == PlanPhase::Approved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phase_progression() {
        assert_eq!(
            PlanPhase::InitialUnderstanding.next(),
            Some(PlanPhase::Design)
        );
        assert_eq!(PlanPhase::Design.next(), Some(PlanPhase::Review));
        assert_eq!(PlanPhase::Review.next(), Some(PlanPhase::FinalPlan));
        assert_eq!(PlanPhase::FinalPlan.next(), Some(PlanPhase::Approved));
        assert_eq!(PlanPhase::Approved.next(), None);
    }

    #[test]
    fn test_can_advance() {
        assert!(PlanPhase::InitialUnderstanding.can_advance());
        assert!(PlanPhase::Design.can_advance());
        assert!(PlanPhase::Review.can_advance());
        assert!(PlanPhase::FinalPlan.can_advance());
        assert!(!PlanPhase::Approved.can_advance());
    }

    #[test]
    fn test_plan_state_new() {
        let state = PlanState::new("plan-1".into(), "Test Plan".into());
        assert_eq!(state.phase, PlanPhase::InitialUnderstanding);
        assert_eq!(state.questions_asked, 0);
        assert_eq!(state.questions_remaining, 5);
        assert!(state.draft_prd.is_none());
        assert!(!state.is_complete());
    }

    #[test]
    fn test_plan_state_advance() {
        let mut state = PlanState::new("plan-1".into(), "Test Plan".into());
        assert_eq!(state.advance_phase(), Some(PlanPhase::Design));
        assert_eq!(state.advance_phase(), Some(PlanPhase::Review));
        assert_eq!(state.advance_phase(), Some(PlanPhase::FinalPlan));
        assert_eq!(state.advance_phase(), Some(PlanPhase::Approved));
        assert!(state.is_complete());
        assert_eq!(state.advance_phase(), None);
    }

    #[test]
    fn test_plan_state_retreat() {
        let mut state = PlanState::new("plan-1".into(), "Test Plan".into());
        state.advance_phase(); // Design
        state.advance_phase(); // Review
        state.retreat_to(PlanPhase::Design);
        assert_eq!(state.phase, PlanPhase::Design);
    }

    #[test]
    fn test_question_tracking() {
        let mut state = PlanState::new("plan-1".into(), "Test Plan".into());
        assert_eq!(state.questions_remaining, 5);
        state.question_asked();
        assert_eq!(state.questions_asked, 1);
        assert_eq!(state.questions_remaining, 4);
        state.question_asked();
        assert_eq!(state.questions_asked, 2);
        assert_eq!(state.questions_remaining, 3);
    }

    #[test]
    fn test_metadata() {
        let mut state = PlanState::new("plan-1".into(), "Test Plan".into());
        state.set_meta("language", "rust");
        state.set_meta("framework", "tokio");
        assert_eq!(state.metadata.get("language").unwrap(), "rust");
        assert_eq!(state.metadata.get("framework").unwrap(), "tokio");
    }

    #[test]
    fn test_plan_state_display() {
        assert_eq!(PlanPhase::InitialUnderstanding.to_string(), "Initial Understanding");
        assert_eq!(PlanPhase::Design.to_string(), "Design");
        assert_eq!(PlanPhase::Review.to_string(), "Review");
        assert_eq!(PlanPhase::FinalPlan.to_string(), "Final Plan");
        assert_eq!(PlanPhase::Approved.to_string(), "Approved");
    }
}
