//! Active multi-turn plan interview sessions persisted under `~/.dscode/plans/`.
//!
//! Uses **LLM dynamic questioning** (grill-me): one high-leverage question per turn,
//! driven by phase + project snapshot + prior answers — not a fixed question bank.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

use super::llm_interview::{
    next_llm_turn, project_snapshot, LlmInterviewAction, PendingQuestion,
};
use super::phases::{PlanPhase, PlanState};
use super::prd::{PrdDocument, PrdError, PrdGenerator};
use crate::agent::stream::StreamEvent;
use crate::providers::trait_def::LlmProvider;

/// On-disk format version of [`ActivePlanSession`].
///
/// Bump this whenever the serialized shape changes incompatibly (a field
/// renamed or removed, a `PlanPhase` variant renamed, …). A file written by a
/// newer build is then reported as an error instead of silently collapsing to
/// "no active plan" — which used to make an in-progress interview disappear on
/// upgrade with nothing to show the user.
pub const PLAN_SCHEMA_VERSION: u32 = 1;

fn default_schema_version() -> u32 {
    PLAN_SCHEMA_VERSION
}

/// An interview file untouched for longer than this is treated as abandoned.
///
/// Without a TTL, a user who starts `/plan`, quits, and comes back the next day
/// has their first ordinary message consumed as the answer to a stale question.
const SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);

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
    /// On-disk format version; see [`PLAN_SCHEMA_VERSION`]. Files written
    /// before the field existed default to the current version.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
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
    ///
    /// A file that exists but is unusable (truncated/corrupt JSON, a future
    /// schema version, or a stale session) is **not** silently ignored: it is
    /// logged at error level. Use [`Self::load_strict`] to react to it
    /// programmatically.
    pub fn load(session_id: &str) -> Option<Self> {
        match Self::load_strict(session_id) {
            Ok(session) => session,
            Err(e) => {
                error!(session = %session_id, %e, "plan interview could not be loaded");
                None
            }
        }
    }

    /// Load an active plan, distinguishing "absent" from "unusable".
    ///
    /// * `Ok(None)` — there is no interview to resume.
    /// * `Ok(Some)` — a valid, fresh interview.
    /// * `Err`      — a file exists but cannot be used. Callers that only need
    ///   a yes/no answer should use [`Self::load`], which logs this.
    ///
    /// This is what makes a torn write visible: the old `load()` collapsed
    /// *every* failure mode into `None`, so a partially written file looked
    /// exactly like "no interview in progress".
    pub fn load_strict(session_id: &str) -> Result<Option<Self>, String> {
        let path = Self::path_for(session_id)?;
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        if data.trim().is_empty() {
            return Err(format!(
                "{} is empty — the previous write did not complete",
                path.display()
            ));
        }
        let session: Self = serde_json::from_str(&data)
            .map_err(|e| format!("{} is corrupt: {e}", path.display()))?;
        if session.schema_version > PLAN_SCHEMA_VERSION {
            return Err(format!(
                "{} uses plan schema v{} but this build understands v{} — upgrade dscode to resume it",
                path.display(),
                session.schema_version,
                PLAN_SCHEMA_VERSION
            ));
        }
        if let Some(age) = Self::age_of(&path) {
            if age > SESSION_TTL {
                return Err(format!(
                    "{} was last updated {}h ago (TTL {}h) — treating the interview as abandoned",
                    path.display(),
                    age.as_secs() / 3600,
                    SESSION_TTL.as_secs() / 3600
                ));
            }
        }
        Ok(Some(session))
    }

    /// How long ago the file backing a session was last written.
    fn age_of(path: &Path) -> Option<std::time::Duration> {
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;
        std::time::SystemTime::now().duration_since(modified).ok()
    }

    /// Persist this plan session.
    ///
    /// Writes a temporary file and renames it over the target: `fs::write`
    /// opens with `CREATE_ALWAYS`, so a crash (or power loss) between truncate
    /// and write used to leave a 0-byte file where the interview used to be.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path_for(&self.session_id)?;
        let data =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize plan: {e}"))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &data).map_err(|e| format!("write plan tmp: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("replace plan file: {e}")
        })
    }

    /// Remove active plan state for a session.
    pub fn clear(session_id: &str) {
        if let Ok(path) = Self::path_for(session_id) {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(path.with_extension("json.tmp"));
        }
    }

    /// Cancel an in-progress interview, returning the terminal turn result.
    ///
    /// `/plan cancel` in `agent/forge.rs` currently calls [`Self::clear`]
    /// directly, so [`PlanTurnResult::Cancelled`] is never constructed and the
    /// UI gets a hand-written string instead of a real result object. This is
    /// the drop-in replacement for that call site.
    pub fn cancel(session_id: &str) -> PlanTurnResult {
        Self::clear(session_id);
        PlanTurnResult::Cancelled
    }

    /// Move an existing interview file to `<session>.json.bak` so that
    /// replacing it is recoverable.
    ///
    /// Returns the backup path when there was something to preserve.
    fn snapshot_existing(session_id: &str) -> Option<PathBuf> {
        let path = Self::path_for(session_id).ok()?;
        if !path.exists() {
            return None;
        }
        let backup = path.with_extension("json.bak");
        match std::fs::rename(&path, &backup) {
            Ok(()) => {
                warn!(
                    path = %path.display(),
                    backup = %backup.display(),
                    "starting a new /plan interview over an existing one — previous answers kept as .bak"
                );
                Some(backup)
            }
            Err(e) => {
                error!(
                    %e,
                    path = %path.display(),
                    "could not back up the existing plan interview; it will be overwritten"
                );
                None
            }
        }
    }

    /// Undo [`Self::snapshot_existing`] after a failed restart.
    fn restore_backup(session_id: &str, backup: &Path) {
        match Self::path_for(session_id) {
            Ok(path) => {
                if let Err(e) = std::fs::rename(backup, &path) {
                    error!(
                        %e,
                        backup = %backup.display(),
                        "could not restore the previous plan interview (it is still on disk)"
                    );
                }
            }
            Err(e) => error!(%e, backup = %backup.display(), "could not restore plan interview"),
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
        progress: Option<&tokio::sync::mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<(Self, PlanTurnResult), String> {
        // Validate *before* touching on-disk state: an empty goal must not be
        // able to destroy the interview that is already in progress.
        let title: String = user_goal.chars().take(80).collect();
        if title.trim().is_empty() {
            return Err("Usage: /plan <describe what you want to build>".into());
        }

        // Replacing an active interview is destructive, and this path is
        // reachable with something that is not really a restart: answering
        // "What should the CLI subcommand be?" with `/plan status` lands here
        // (the command check in forge.rs is a `starts_with`). Keep the previous
        // answers as `<session>.json.bak` — one file per session, discoverable —
        // and restore them if the new interview fails to start.
        let backup = Self::snapshot_existing(session_id);

        let task_id = uuid::Uuid::new_v4().to_string();

        if let Some(tx) = progress {
            let _ = tx.send(StreamEvent::Token {
                content: format!(
                    "_启动 /plan 访谈_\n\n**目标：** {}\n\n_扫描项目快照…_\n",
                    title
                ),
            });
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
            schema_version: PLAN_SCHEMA_VERSION,
        };

        if let Some(tx) = progress {
            let _ = tx.send(StreamEvent::Token {
                content: "_快照就绪，进入 Scope 阶段…_\n".into(),
            });
        }

        let result = match session.drive_llm(provider, progress).await {
            Ok(result) => result,
            Err(e) => {
                // Nothing was started — put the previous interview back.
                if let Some(backup) = &backup {
                    Self::restore_backup(session_id, backup);
                }
                return Err(e);
            }
        };
        if let Err(e) = session.persist_for_result(&result) {
            if let Some(backup) = &backup {
                Self::restore_backup(session_id, backup);
            }
            return Err(e);
        }
        // On success the `.bak` is left in place on purpose: it is the only copy
        // of the interview that was just replaced.
        Ok((session, result))
    }

    /// Apply a user answer and ask the LLM for the next step.
    pub async fn answer_with_llm(
        &mut self,
        provider: &dyn LlmProvider,
        answer: &str,
        progress: Option<&tokio::sync::mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<PlanTurnResult, String> {
        let answer = answer.trim();
        if answer.is_empty() {
            return Err("Please provide an answer (or type /plan cancel to abort).".into());
        }

        let pending = self
            .current_question
            .clone()
            .ok_or_else(|| "No pending question — restart with /plan <goal>".to_string())?;

        // `format_question` prints the options as `1. …`, and a bare number is
        // the only affordance the TUI offers — but nothing used to resolve it,
        // so `qa_history` recorded the literal "2" and the model (and the PRD's
        // Context block) never learned which option that was.
        let final_answer = if let Some(option) = resolve_numbered_option(answer, &pending.options) {
            option
        } else if answer.eq_ignore_ascii_case("y")
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

        if let Some(tx) = progress {
            let _ = tx.send(StreamEvent::Token {
                content: format!(
                    "_已记录回答，继续 {}…_\n",
                    self.plan_state.phase.label()
                ),
            });
        }

        self.qa_history
            .push((pending.text.clone(), final_answer));
        self.current_question = None;
        self.plan_state.question_asked();
        self.questions_in_phase = self.questions_in_phase.saturating_add(1);

        if self.project_snapshot.is_empty() {
            self.project_snapshot = project_snapshot(&self.working_dir);
        }

        let result = self.drive_llm(provider, progress).await?;
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
    async fn drive_llm(
        &mut self,
        provider: &dyn LlmProvider,
        progress: Option<&tokio::sync::mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<PlanTurnResult, String> {
        // Safety: prevent infinite advance loops
        for _ in 0..12 {
            if self.plan_state.phase == PlanPhase::Approved {
                return self.finalize_prd(progress).await;
            }

            let action = next_llm_turn(
                provider,
                &self.user_goal,
                self.plan_state.phase,
                &self.qa_history,
                self.questions_in_phase,
                &self.project_snapshot,
                progress,
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
                            return self.finalize_prd(progress).await;
                        }
                        PlanPhase::Approved => {
                            return self.finalize_prd(progress).await;
                        }
                        _ => {
                            let from = self.plan_state.phase.label().to_string();
                            self.plan_state.advance_phase();
                            self.questions_in_phase = 0;
                            if let Some(tx) = progress {
                                let _ = tx.send(StreamEvent::Token {
                                    content: format!(
                                        "_阶段完成（{}）→ 进入 **{}**…_\n",
                                        from,
                                        self.plan_state.phase.label()
                                    ),
                                });
                            }
                            // Loop for next phase first question
                        }
                    }
                }
                LlmInterviewAction::Complete { reason } => {
                    info!(%reason, "plan LLM complete");
                    // Jump to finalize even if earlier phases — user/model satisfied
                    self.plan_state.retreat_to(PlanPhase::Quality);
                    return self.finalize_prd(progress).await;
                }
            }
        }
        // Fallback: enough to write PRD
        self.finalize_prd(progress).await
    }

    async fn finalize_prd(
        &mut self,
        progress: Option<&tokio::sync::mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<PlanTurnResult, String> {
        // A model that answers `complete` on the very first turn (small/fast
        // models do) would otherwise emit a full PRD having asked the user
        // nothing at all. Require at least one recorded answer.
        if self.qa_history.is_empty() {
            warn!(
                phase = ?self.plan_state.phase,
                "plan interview reached PRD generation with no answers — refusing"
            );
            return Err(
                "plan interview recorded no answers — refusing to write a PRD; \
                 restart with /plan <goal>"
                    .into(),
            );
        }
        if let Some(tx) = progress {
            let _ = tx.send(StreamEvent::Token {
                content: "_访谈完成，正在生成 PRD…_\n".into(),
            });
        }
        let generator = PrdGenerator::new(self.working_dir.clone());
        // The guard above guarantees at least one answer, so prefixing the goal
        // always yields >= 2 pairs and `PrdGenerator::generate` always finds a
        // goal question ("What is the overall goal?") to extract.
        let mut answers = self.qa_history.clone();
        answers.insert(
            0,
            ("What is the overall goal?".into(), self.user_goal.clone()),
        );

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

/// Map a bare option number (`"2"`) onto the option text it labelled.
///
/// Returns `None` for anything that is not a 1-based in-range integer, so free
/// text (including a number written as prose) is recorded verbatim.
fn resolve_numbered_option(answer: &str, options: &[String]) -> Option<String> {
    let n: usize = answer.trim().parse().ok()?;
    if n == 0 {
        return None;
    }
    options.get(n - 1).cloned()
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
            out.push_str("_Auto interview — one question at a time. Reply with an option number, use the buttons below, or type a custom answer._\n\n");
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
