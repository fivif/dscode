//! Plan interview engine — structured requirements gathering before execution.
//!
//! The Plan engine implements a five-phase state machine that walks the user
//! through a "grill-me" interview — one question at a time — to gather
//! requirements, explore the codebase, make architecture decisions, and
//! produce a structured PRD (Plan Requirements Document).
//!
//! # Architecture
//!
//! ```text
//! InitialUnderstanding → Design → Review → FinalPlan → Approved
//!       ↑                  ↑         ↓                     |
//!       └──────────────────┴─────────┘ (retreat on revision)
//! ```
//!
//! # Usage
//!
//! ```no_run
//! use dscode_core::plan::{PlanState, InterviewEngine, PrdGenerator};
//! use dscode_core::plan::phases::PlanPhase;
//! use std::path::PathBuf;
//!
//! # async fn example() {
//! let mut engine = InterviewEngine::new(PathBuf::from("/project"));
//! // Add questions...
//! // Run the interview...
//! // Generate PRD...
//! # }
//! ```

pub mod interview;
pub mod phases;
pub mod prd;

pub use interview::{default_interview_questions, design_questions, DecisionNode, InterviewAction, InterviewEngine, Question};
pub use phases::{PlanPhase, PlanState};
pub use prd::{ArchitectureDecision, FileAction, FileActionType, ImplementationStep, PrdBuilder, PrdDocument, PrdError, PrdGenerator, TestPlan};

/// Run a complete plan interview for the given task ID and title.
///
/// This is the high-level entry point. It:
/// 1. Creates a [`PlanState`] in the InitialUnderstanding phase.
/// 2. Creates an [`InterviewEngine`] with default questions.
/// 3. Runs the one-question-at-a-time interview loop.
/// 4. Advances through phases.
/// 5. Generates and persists the PRD.
/// 6. Returns the final approved plan state.
///
/// Returns an error if the interview cannot produce a valid PRD.
pub async fn run_plan_interview(
    task_id: &str,
    title: &str,
    working_dir: std::path::PathBuf,
    user_message: &str,
) -> Result<PlanState, PrdError> {
    use phases::PlanPhase;

    // 1. Create initial plan state
    let mut state = PlanState::new(task_id.to_string(), title.to_string());
    state.set_meta("user_message", user_message);

    // 2. Create interview engine and seed with default questions
    let mut engine = InterviewEngine::new(working_dir.clone());
    for q in default_interview_questions(&working_dir) {
        engine.add_question(q);
    }

    // 3. Phase: InitialUnderstanding
    while state.phase == PlanPhase::InitialUnderstanding {
        let action = engine.next_action().await;
        match action {
            InterviewAction::AskQuestion { question, .. } => {
                // In a real implementation, this would prompt the user.
                // For automated runs, we accept the recommended answer.
                engine.answer_current(&question.recommended_answer);
                state.question_asked();
            }
            InterviewAction::PhaseComplete { .. } => {
                state.advance_phase();
                engine.advance_phase();
                // Seed design questions
                for q in design_questions() {
                    engine.add_question(q);
                }
            }
            InterviewAction::Complete => {
                state.retreat_to(PlanPhase::Approved);
                break;
            }
        }
    }

    // 4. Phase: Design
    while state.phase == PlanPhase::Design {
        let action = engine.next_action().await;
        match action {
            InterviewAction::AskQuestion { question, .. } => {
                engine.answer_current(&question.recommended_answer);
                state.question_asked();
            }
            InterviewAction::PhaseComplete { .. } => {
                state.advance_phase(); // → Review
            }
            InterviewAction::Complete => break,
        }
    }

    // 5. Phase: Review — in a real implementation this would run scrutiny
    state.advance_phase(); // → FinalPlan

    // 6. Phase: FinalPlan — generate the PRD
    let generator = PrdGenerator::new(working_dir);
    let answers = engine.answer_summary();
    let prd = generator.generate(&answers, task_id, title)?;

    state.draft_prd = Some(prd.clone());
    state.advance_phase(); // → Approved

    // 7. Persist the PRD
    let _prd_path = generator.persist(&prd, task_id)?;

    Ok(state)
}
