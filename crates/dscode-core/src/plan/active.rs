//! Active multi-turn plan interview sessions persisted under `~/.dscode/plans/`.
//!
//! Uses **LLM dynamic questioning** (grill-me): one high-leverage question per turn,
//! driven by phase + project snapshot + prior answers — not a fixed question bank.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

use super::llm_interview::{
    next_llm_turn, project_snapshot, LlmInterviewAction, PendingQuestion,
};
use super::phases::{PlanPhase, PlanState};
use super::prd::{PrdDocument, PrdError, PrdGenerator};
use crate::providers::trait_def::LlmProvider;

/// Result of one plan turn (start or answer).
#[derive(Debug, Clone)]
pub enum PlanTurnResult {
    /// Ask the user a question (grill-me style, one at a time).
    Question {
        phase: PlanPhase,
        question: String,
        recommended: String,
        /// Button choices for the desktop UI.
        options: Vec<String>,
        remaining: u32,
        auto_notes: Vec<String>,
    },
    /// Interview finished; PRD is ready.
    PrdReady {
        prd: PrdDocument,
        path: PathBuf,
        markdown: String,
    },
    /// Interview cancelled.
    Cancelled,
}

/// Persisted active plan interview for a chat session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivePlanSession {
    pub session_id: String,
    pub task_id: String,
    pub title: String,
    pub user_goal: String,
    pub plan_state: PlanState,
    pub working_dir: PathBuf,
    /// (question, answer) history across all phases.
    pub qa_history: Vec<(String, String)>,
    /// Question currently awaiting user answer.
    pub current_question: Option<PendingQuestion>,
    /// How many user questions asked in the current phase.
    pub questions_in_phase: u32,
    /// Cached project snapshot (rebuilt if empty on load).
    #[serde(default)]
    pub project_snapshot: String,
}

impl ActivePlanSession {
    fn plans_dir() -> Result<PathBuf, String> {
        let dir = crate::config::settings::Config::data_dir()
            .map_err(|e| e.to_string())?
            .join("plans");
        std::fs::create_dir_all(&dir).map_err(|e| format!("create plans dir: {e}"))?;
        Ok(dir)
    }

    fn path_for(session_id: &str) -> Result<PathBuf, String> {
        Ok(Self::plans_dir()?.join(format!("{session_id}.json")))
    }

    /// Load an active plan for this chat session, if any.
    pub fn load(session_id: &str) -> Option<Self> {
        let path = Self::path_for(session_id).ok()?;
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Persist this plan session.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path_for(&self.session_id)?;
        let data =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize plan: {e}"))?;
        std::fs::write(path, data).map_err(|e| format!("write plan: {e}"))
    }

    /// Remove active plan state for a session.
    pub fn clear(session_id: &str) {
        if let Ok(path) = Self::path_for(session_id) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Whether a plan interview is in progress for this session.
    pub fn is_active(session_id: &str) -> bool {
        Self::load(session_id).is_some()
    }

    /// Start a new LLM-driven multi-turn plan interview.
    pub async fn start_with_llm(
        provider: &dyn LlmProvider,
        session_id: &str,
        user_goal: &str,
        working_dir: PathBuf,
    ) -> Result<(Self, PlanTurnResult), String> {
        Self::clear(session_id);

        let task_id = uuid::Uuid::new_v4().to_string();
        let title: String = user_goal.chars().take(80).collect();
        if title.trim().is_empty() {
            return Err("Usage: /plan <describe what you want to build>".into());
        }

        let snapshot = project_snapshot(&working_dir);
        let mut plan_state = PlanState::new(task_id.clone(), title.clone());
        plan_state.set_meta("user_message", user_goal);
        plan_state.set_meta("session_id", session_id);
        plan_state.set_meta("mode", "llm_dynamic");

        let mut session = Self {
            session_id: session_id.to_string(),
            task_id,
            title,
            user_goal: user_goal.to_string(),
            plan_state,
            working_dir,
            qa_history: Vec::new(),
            current_question: None,
            questions_in_phase: 0,
            project_snapshot: snapshot,
        };

        let result = session.drive_llm(provider).await?;
        session.persist_for_result(&result)?;
        Ok((session, result))
    }

    /// Apply a user answer and ask the LLM for the next step.
    pub async fn answer_with_llm(
        &mut self,
        provider: &dyn LlmProvider,
        answer: &str,
    ) -> Result<PlanTurnResult, String> {
        let answer = answer.trim();
        if answer.is_empty() {
            return Err("Please provide an answer (or type /plan cancel to abort).".into());
        }

        let pending = self
            .current_question
            .clone()
            .ok_or_else(|| "No pending question — restart with /plan <goal>".to_string())?;

        let final_answer = if answer.eq_ignore_ascii_case("y")
            || answer.eq_ignore_ascii_case("yes")
            || answer.eq_ignore_ascii_case("ok")
            || answer == "推荐"
            || answer.eq_ignore_ascii_case("recommended")
        {
            if pending.recommended.is_empty() {
                answer.to_string()
            } else {
                pending.recommended.clone()
            }
        } else {
            answer.to_string()
        };

        self.qa_history
            .push((pending.text.clone(), final_answer));
        self.current_question = None;
        self.plan_state.question_asked();
        self.questions_in_phase = self.questions_in_phase.saturating_add(1);

        if self.project_snapshot.is_empty() {
            self.project_snapshot = project_snapshot(&self.working_dir);
        }

        let result = self.drive_llm(provider).await?;
        self.persist_for_result(&result)?;
        Ok(result)
    }

    fn persist_for_result(&self, result: &PlanTurnResult) -> Result<(), String> {
        match result {
            PlanTurnResult::PrdReady { .. } | PlanTurnResult::Cancelled => {
                Self::clear(&self.session_id);
            }
            PlanTurnResult::Question { .. } => {
                self.save()?;
            }
        }
        Ok(())
    }

    /// Drive LLM until we need a user answer or the PRD is ready.
    async fn drive_llm(&mut self, provider: &dyn LlmProvider) -> Result<PlanTurnResult, String> {
        // Safety: prevent infinite advance loops
        for _ in 0..12 {
            if self.plan_state.phase == PlanPhase::Approved {
                return self.finalize_prd().await;
            }

            let action = next_llm_turn(
                provider,
                &self.user_goal,
                self.plan_state.phase,
                &self.qa_history,
                self.questions_in_phase,
                &self.project_snapshot,
            )
            .await?;

            match action {
                LlmInterviewAction::Ask {
                    question,
                    recommended,
                    options,
                    auto_notes,
                } => {
                    self.current_question = Some(PendingQuestion {
                        text: question.clone(),
                        recommended: recommended.clone(),
                        options: options.clone(),
                        phase: self.plan_state.phase,
                    });
                    let remaining =
                        super::llm_interview::MAX_QUESTIONS_PER_PHASE.saturating_sub(self.questions_in_phase);
                    return Ok(PlanTurnResult::Question {
                        phase: self.plan_state.phase,
                        question,
                        recommended,
                        options,
                        remaining: remaining.max(1),
                        auto_notes,
                    });
                }
                LlmInterviewAction::Advance { reason } => {
                    info!(phase = ?self.plan_state.phase, %reason, "plan phase advance");
                    match self.plan_state.phase {
                        PlanPhase::Quality => {
                            return self.finalize_prd().await;
                        }
                        PlanPhase::Approved => {
                            return self.finalize_prd().await;
                        }
                        _ => {
                            self.plan_state.advance_phase();
                            self.questions_in_phase = 0;
                            // Loop for next phase first question
                        }
                    }
                }
                LlmInterviewAction::Complete { reason } => {
                    info!(%reason, "plan LLM complete");
                    // Jump to finalize even if earlier phases — user/model satisfied
                    self.plan_state.retreat_to(PlanPhase::Quality);
                    return self.finalize_prd().await;
                }
            }
        }
        // Fallback: enough to write PRD
        self.finalize_prd().await
    }

    async fn finalize_prd(&mut self) -> Result<PlanTurnResult, String> {
        let generator = PrdGenerator::new(self.working_dir.clone());
        let mut answers = self.qa_history.clone();
        answers.insert(
            0,
            ("What is the overall goal?".into(), self.user_goal.clone()),
        );

        // Ensure at least one goal-like answer so PrdGenerator::generate succeeds
        if answers.len() == 1 {
            answers.push((
                "Primary deliverable".into(),
                self.user_goal.clone(),
            ));
        }

        let prd = generator
            .generate(&answers, &self.task_id, &self.title)
            .map_err(|e: PrdError| e.to_string())?;

        let path = generator
            .persist(&prd, &self.task_id)
            .map_err(|e: PrdError| e.to_string())?;

        self.plan_state.draft_prd = Some(prd.clone());
        self.plan_state.retreat_to(PlanPhase::Approved);

        let markdown = format_prd_markdown(&prd, &path);
        info!(task = %self.task_id, path = %path.display(), "PRD generated (LLM interview)");

        Ok(PlanTurnResult::PrdReady {
            prd,
            path,
            markdown,
        })
    }
}

/// Render a PRD as markdown for the chat UI.
pub fn format_prd_markdown(prd: &PrdDocument, path: &std::path::Path) -> String {
    let mut out = String::new();
    out.push_str(&format!("# PRD: {}\n\n", prd.title));
    out.push_str(&format!("**ID:** `{}`  \n", prd.id));
    out.push_str(&format!("**Saved:** `{}`  \n", path.display()));
    out.push_str(&format!("**Estimate:** ~{} min\n\n", prd.estimate_minutes));
    out.push_str(&format!("## Description\n\n{}\n\n", prd.description));

    if !prd.goals.is_empty() {
        out.push_str("## Goals\n\n");
        for g in &prd.goals {
            out.push_str(&format!("- {g}\n"));
        }
        out.push('\n');
    }
    if !prd.success_criteria.is_empty() {
        out.push_str("## Success Criteria\n\n");
        for c in &prd.success_criteria {
            out.push_str(&format!("- {c}\n"));
        }
        out.push('\n');
    }
    if !prd.architecture_decisions.is_empty() {
        out.push_str("## Architecture Decisions\n\n");
        for d in &prd.architecture_decisions {
            out.push_str(&format!("- **{}** — {}\n", d.decision, d.rationale));
        }
        out.push('\n');
    }
    if !prd.implementation_steps.is_empty() {
        out.push_str("## Implementation Steps\n\n");
        for s in &prd.implementation_steps {
            out.push_str(&format!(
                "{}. {} (~{} min)\n",
                s.step_number, s.description, s.estimated_minutes
            ));
        }
        out.push('\n');
    }
    out.push_str("---\n\n");
    out.push_str(
        "PRD ready. Run `/auto` with this plan to execute, or refine with another `/plan`.\n",
    );
    out
}

/// Format a plan question for the UI (markdown body; options render as buttons in desktop).
pub fn format_question(result: &PlanTurnResult) -> String {
    match result {
        PlanTurnResult::Question {
            phase,
            question,
            recommended,
            options,
            remaining,
            auto_notes,
        } => {
            let mut out = String::new();
            out.push_str(&format!("## /plan — {}\n\n", phase.label()));
            out.push_str("_Auto interview — one question at a time. Use the buttons below or type a custom answer._\n\n");
            if !auto_notes.is_empty() {
                out.push_str("### Notes from project context\n\n");
                for n in auto_notes {
                    out.push_str(&format!("- {n}\n"));
                }
                out.push('\n');
            }
            out.push_str(&format!(
                "**Question** (~{remaining} left this phase):\n\n{question}\n\n"
            ));
            if !recommended.is_empty() {
                out.push_str(&format!("> **Recommended:** {recommended}\n\n"));
            }
            if !options.is_empty() {
                out.push_str("**Options:**\n\n");
                for (i, o) in options.iter().enumerate() {
                    out.push_str(&format!("{}. {o}\n", i + 1));
                }
                out.push('\n');
            }
            out
        }
        PlanTurnResult::PrdReady { markdown, .. } => markdown.clone(),
        PlanTurnResult::Cancelled => "Plan interview cancelled.".into(),
    }
}

/// Emit structured plan-question payload for button UI (if applicable).
pub fn plan_question_event(result: &PlanTurnResult) -> Option<crate::agent::stream::StreamEvent> {
    match result {
        PlanTurnResult::Question {
            phase,
            question,
            recommended,
            options,
            remaining,
            auto_notes,
        } => Some(crate::agent::stream::StreamEvent::PlanQuestion {
            phase: phase.label().to_string(),
            question: question.clone(),
            recommended: recommended.clone(),
            options: options.clone(),
            remaining: *remaining,
            auto_notes: auto_notes.clone(),
        }),
        _ => None,
    }
}
