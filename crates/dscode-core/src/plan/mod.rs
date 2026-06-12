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
                // In a real interactive run, this would prompt the user via stdin.
                // For non-interactive / automated runs, use the user's original
                // message to influence the answer — extract relevant keywords
                // and match them against the question text. Falls back to the
                // recommended answer when no clear match is found.
                let answer = auto_answer_for(&question, user_message);
                engine.answer_current(&answer);
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
                let answer = auto_answer_for(&question, user_message);
                engine.answer_current(&answer);
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

/// Produce a best-effort answer for a question when running non-interactively.
///
/// Uses the user's original message to extract keywords and match them against
/// the question text. Falls back to the question's recommended answer when no
/// relevant keywords from the user message are found.
fn auto_answer_for(question: &interview::Question, user_message: &str) -> String {
    let question_lower = question.text.to_lowercase();
    let msg_lower = user_message.to_lowercase();

    // Split the user message into meaningful tokens
    let tokens: Vec<&str> = msg_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .collect();

    // If the user message directly addresses topics in the question,
    // build a tailored answer from the message content.
    let relevant: Vec<&&str> = tokens
        .iter()
        .filter(|t| question_lower.contains(*t))
        .collect();

    if relevant.is_empty() {
        // No overlap — fall back to the recommended answer.
        question.recommended_answer.clone()
    } else {
        // Use the user's message as contextual input, trimmed to a reasonable length.
        let excerpt: String = user_message.chars().take(500).collect();
        format!(
            "Based on user request: \"{}\"\nRecommended: {}",
            excerpt,
            question.recommended_answer
        )
    }
}
