//! LLM-driven dynamic plan interview (grill-me style).
//!
//! Instead of a fixed question bank, each turn asks the model for the **single**
//! next best clarifying question given: goal, phase, project snapshot, and Q&A so far.
//! The model can also advance phases or declare the interview complete.

use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{info, warn};

use super::phases::PlanPhase;
use crate::providers::trait_def::{LlmProvider, Message, MessageContent, Role};

/// Max user-facing questions per phase before forcing advance (token control).
pub const MAX_QUESTIONS_PER_PHASE: u32 = 4;

/// Pending question shown to the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingQuestion {
    pub text: String,
    pub recommended: String,
    /// Clickable answer choices for the UI (2–4 preferred).
    #[serde(default)]
    pub options: Vec<String>,
    pub phase: PlanPhase,
}

/// Structured LLM turn result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmInterviewAction {
    /// Ask the user one question.
    Ask {
        question: String,
        #[serde(default)]
        recommended: String,
        /// Discrete options for button UI (plus custom free-text always available).
        #[serde(default)]
        options: Vec<String>,
        #[serde(default)]
        auto_notes: Vec<String>,
    },
    /// Current phase is done; advance (or finish if Quality).
    Advance {
        #[serde(default)]
        reason: String,
    },
    /// Enough information — generate PRD.
    Complete {
        #[serde(default)]
        reason: String,
    },
}

/// Raw JSON shape the model is asked to return.
#[derive(Debug, Deserialize)]
struct LlmTurnJson {
    action: String,
    #[serde(default)]
    question: String,
    #[serde(default)]
    recommended: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    auto_notes: Vec<String>,
    #[serde(default)]
    reason: String,
}

/// Collect a lightweight project snapshot for interview context (no LLM).
pub fn project_snapshot(working_dir: &Path) -> String {
    let mut parts = Vec::new();
    parts.push(format!("Working directory: {}", working_dir.display()));

    // Top-level entries
    if let Ok(rd) = std::fs::read_dir(working_dir) {
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() {
                    format!("{name}/")
                } else {
                    name
                }
            })
            .filter(|n| !n.starts_with('.') && n != "target/" && n != "node_modules/")
            .take(40)
            .collect();
        names.sort();
        if !names.is_empty() {
            parts.push(format!("Top-level: {}", names.join(", ")));
        }
    }

    // Key manifest files (truncated)
    for rel in [
        "README.md",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "Makefile",
    ] {
        let p = working_dir.join(rel);
        if let Ok(content) = std::fs::read_to_string(&p) {
            let excerpt: String = content.chars().take(1200).collect();
            parts.push(format!("--- {rel} ---\n{excerpt}"));
        }
    }

    parts.join("\n\n")
}

/// Ask the LLM for the next interview action.
pub async fn next_llm_turn(
    provider: &dyn LlmProvider,
    user_goal: &str,
    phase: PlanPhase,
    qa_history: &[(String, String)],
    questions_in_phase: u32,
    project_snapshot: &str,
) -> Result<LlmInterviewAction, String> {
    // Soft force advance when phase budget exhausted
    if questions_in_phase >= MAX_QUESTIONS_PER_PHASE {
        info!(?phase, questions_in_phase, "plan: phase question budget reached");
        return Ok(LlmInterviewAction::Advance {
            reason: format!(
                "Reached {MAX_QUESTIONS_PER_PHASE} questions in phase {}",
                phase.label()
            ),
        });
    }

    let history_block = if qa_history.is_empty() {
        "(no answers yet)".to_string()
    } else {
        qa_history
            .iter()
            .enumerate()
            .map(|(i, (q, a))| format!("{}. Q: {q}\n   A: {a}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let phase_guide = match phase {
        PlanPhase::Scope => {
            "SCOPE: project boundaries, users, success definition, non-goals. Challenge vague goals."
        }
        PlanPhase::Requirements => {
            "REQUIREMENTS: features, constraints, data, integrations, must-haves vs nice-to-haves."
        }
        PlanPhase::Design => {
            "DESIGN: architecture, modules, APIs, data models, tech choices. Prefer concrete decisions."
        }
        PlanPhase::Risks => {
            "RISKS: technical risks, unknowns, security, migration, rollback. Ask about worst cases."
        }
        PlanPhase::Quality => {
            "QUALITY: acceptance criteria, tests, definition of done, observability."
        }
        PlanPhase::Approved => "Interview already complete.",
    };

    let prompt = format!(
        r#"You are a senior product/engineering interviewer running a grill-me style plan interview.
Ask ONE high-leverage clarifying question at a time. Never dump a list of questions.
Prefer answers discoverable from the project snapshot when possible (put findings in auto_notes and still ask only what the user must decide).

## Goal
{user_goal}

## Current phase
{phase} — {phase_guide}

## Project snapshot
{project_snapshot}

## Q&A so far
{history_block}

## Questions already asked this phase
{questions_in_phase} / {MAX_QUESTIONS_PER_PHASE}

## Output
Return ONLY a single JSON object (no markdown fences):
{{
  "action": "ask" | "advance" | "complete",
  "question": "the one question for the user (if action=ask)",
  "recommended": "a concrete recommended default (also included as first option when possible)",
  "options": ["choice A", "choice B", "choice C"],
  "auto_notes": ["optional notes from codebase/snapshot"],
  "reason": "why advance/complete"
}}

Rules:
- action=ask: one clear question + 2–4 short `options` for button UI + recommended.
- options must be mutually exclusive, concrete answers (not "yes/no" alone unless appropriate).
- Include the recommended answer as one of the options when it is a valid choice.
- action=advance: this phase is sufficiently covered; move to next phase.
- action=complete: only in Quality (or if all phases essentially covered) when PRD can be written.
- Do not repeat previous questions.
- Keep question under 280 characters. Each option under 80 characters. Recommended under 200.
- Prefer Chinese if the user goal is mostly Chinese; otherwise English.
"#
    );

    let response = provider
        .chat(
            vec![Message {
                role: Role::User,
                content: MessageContent::Text(prompt),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                created_at: 0,
            }],
            vec![],
        )
        .await
        .map_err(|e| format!("plan LLM error: {e}"))?;

    parse_llm_turn(&response.content)
}

fn parse_llm_turn(raw: &str) -> Result<LlmInterviewAction, String> {
    let json_str = extract_json_object(raw).ok_or_else(|| {
        format!(
            "plan LLM returned non-JSON: {}",
            raw.chars().take(200).collect::<String>()
        )
    })?;

    let parsed: LlmTurnJson = serde_json::from_str(&json_str)
        .map_err(|e| format!("plan LLM JSON parse: {e}; body={json_str}"))?;

    match parsed.action.to_lowercase().as_str() {
        "ask" => {
            if parsed.question.trim().is_empty() {
                return Err("plan LLM ask action missing question".into());
            }
            Ok(LlmInterviewAction::Ask {
                question: parsed.question.trim().to_string(),
                recommended: parsed.recommended.trim().to_string(),
                options: normalize_options(&parsed.options, &parsed.recommended),
                auto_notes: parsed.auto_notes,
            })
        }
        "advance" => Ok(LlmInterviewAction::Advance {
            reason: parsed.reason,
        }),
        "complete" => Ok(LlmInterviewAction::Complete {
            reason: parsed.reason,
        }),
        other => {
            warn!(action = %other, "unknown plan action, treating as ask if question present");
            if !parsed.question.trim().is_empty() {
                Ok(LlmInterviewAction::Ask {
                    question: parsed.question,
                    recommended: parsed.recommended.clone(),
                    options: normalize_options(&parsed.options, &parsed.recommended),
                    auto_notes: parsed.auto_notes,
                })
            } else {
                Ok(LlmInterviewAction::Advance {
                    reason: format!("unknown action '{other}'"),
                })
            }
        }
    }
}

/// Dedupe / cap options; ensure recommended appears when provided.
fn normalize_options(raw: &[String], recommended: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let rec = recommended.trim();
    if !rec.is_empty() {
        out.push(rec.to_string());
    }
    for o in raw {
        let t = o.trim();
        if t.is_empty() {
            continue;
        }
        if out.iter().any(|x| x.eq_ignore_ascii_case(t)) {
            continue;
        }
        out.push(t.to_string());
        if out.len() >= 5 {
            break;
        }
    }
    // If model gave nothing usable, leave empty — UI still has custom input + recommended button.
    out
}

/// Extract first `{...}` JSON object from model output.
fn extract_json_object(text: &str) -> Option<String> {
    let trimmed = text.trim();
    // Strip ```json fences if present
    let body = if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.trim_start_matches("json").trim_start();
        rest.split("```").next().unwrap_or(rest).trim()
    } else {
        trimmed
    };

    let start = body.find('{')?;
    let mut depth = 0i32;
    for (i, ch) in body[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(body[start..=start + i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_object() {
        let raw = "Here you go:\n```json\n{\"action\":\"ask\",\"question\":\"Q?\",\"recommended\":\"A\"}\n```";
        let j = extract_json_object(raw).unwrap();
        assert!(j.contains("\"action\""));
    }

    #[test]
    fn test_parse_ask() {
        let a = parse_llm_turn(
            r#"{"action":"ask","question":"Who is the user?","recommended":"internal ops","options":["internal ops","public users"]}"#,
        )
            .unwrap();
        match a {
            LlmInterviewAction::Ask { question, recommended, options, .. } => {
                assert!(question.contains("user"));
                assert!(!recommended.is_empty());
                assert!(options.len() >= 1);
            }
            _ => panic!("expected ask"),
        }
    }

    #[test]
    fn test_parse_advance() {
        let a = parse_llm_turn(r#"{"action":"advance","reason":"enough"}"#).unwrap();
        assert!(matches!(a, LlmInterviewAction::Advance { .. }));
    }
}
