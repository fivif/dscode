//! Melchior brain — evaluates execution quality and decides stop/continue.
//!
//! Melchior reads the PRD, Casper's scrutiny report, and Balthasar's execution
//! output, then assigns a quality score (0-100) and decides whether the MAGI
//! spiral should stop or continue with a specific focus.

use tracing::{debug, warn};

use crate::providers::trait_def::{
    LlmProvider, Message, MessageContent, Role,
};

use super::scheduler::{MagiError, Promotion};

/// The system prompt that primes Melchior for quality evaluation.
const MELCHIOR_SYSTEM_PROMPT: &str = r#"You are Melchior, the quality evaluation brain of the MAGI system.
Your job is to review the execution output against the PRD and scrutiny report,
then produce a structured evaluation.

You MUST respond in exactly this format (no other text):

STOP: true|false
REASON: <one-line explanation of your decision>
QUALITY: <integer 0-100>
FOCUS: <what the next round should focus on, or "None" if stopping>

Rules for evaluation:
- QUALITY 90-100: Task is essentially done, minor polish at most. STOP should be true.
- QUALITY 70-89: Good progress but notable gaps remain. STOP should be false.
- QUALITY 50-69: Partial progress, significant work remaining. STOP should be false.
- QUALITY 0-49: Little or no meaningful progress. STOP should be false and FOCUS must be specific.

Be honest and critical. Do not inflate scores. If the execution was poor, say so."#;

/// Run Melchior's quality evaluation.
///
/// # Arguments
/// * `provider` — the runtime (cheaper) LLM provider for evaluation.
/// * `prd` — the Product Requirements Document.
/// * `scrutiny` — Casper's latest scrutiny report.
/// * `execution` — Balthasar's latest execution output.
///
/// # Returns
/// A [`Promotion`] struct with the quality score and stop/continue decision.
pub async fn promote(
    provider: &dyn LlmProvider,
    prd: &str,
    scrutiny: &str,
    execution: &str,
) -> Result<Promotion, MagiError> {
    let mut messages = Vec::new();

    // System prompt
    messages.push(Message {
        role: Role::System,
        content: MessageContent::Text(MELCHIOR_SYSTEM_PROMPT.to_string()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None, created_at: 0, });

    // User prompt with all the data
    let user_prompt = format!(
        "## PRD\n\n{}\n\n\
         ## Casper's Scrutiny Report\n\n{}\n\n\
         ## Balthasar's Execution Output\n\n{}\n\n\
         Evaluate the execution against the PRD and scrutiny report. \
         Respond in the required format.",
        prd, scrutiny, execution
    );

    messages.push(Message {
        role: Role::User,
        content: MessageContent::Text(user_prompt),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None, created_at: 0, });

    debug!(
        prd_len = prd.len(),
        scrutiny_len = scrutiny.len(),
        execution_len = execution.len(),
        "Melchior: sending evaluation request"
    );

    let response = provider.chat(messages, vec![]).await?;

    debug!(
        response_len = response.content.len(),
        "Melchior: received evaluation"
    );

    parse_promotion(&response.content)
}

/// Parse Melchior's structured response into a [`Promotion`].
///
/// Expected format:
/// ```text
/// STOP: true|false
/// REASON: <text>
/// QUALITY: <number>
/// FOCUS: <text>
/// ```
///
/// Returns `Err(MagiError::Parse)` when no usable `QUALITY` can be found, so
/// the caller's retry wrapper treats a refused/garbled evaluation as an
/// evaluation failure instead of silently counting it as a "continue" vote.
fn parse_promotion(raw: &str) -> Result<Promotion, MagiError> {
    let raw = raw.trim();

    let mut stop_opt: Option<bool> = None;
    let mut stop_reason = String::new();
    let mut quality_opt: Option<f64> = None;
    let mut next_round_focus = String::new();

    for line in raw.lines() {
        let line = line.trim();
        // Tolerate markdown list bullets / bold keys: "- **STOP:** true".
        let line = line.trim_start_matches(|c: char| c == '-' || c == '*' || c == ' ' || c == '\t');
        let line = line.strip_prefix("**").unwrap_or(line);
        // Tolerate a numbered-list prefix: "1. STOP: true".
        let line = strip_list_number(line);

        // Split into (key, value) on the first ':' (or first whitespace for
        // the "STOP true" form) and normalise the key of markdown decoration.
        let (key_raw, value) = match line.find(':') {
            Some(i) => (&line[..i], line[i + 1..].trim()),
            None => match line.find(char::is_whitespace) {
                Some(i) => (&line[..i], line[i..].trim()),
                None => (line, ""),
            },
        };
        let key = key_raw
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_uppercase();

        match key.as_str() {
            "STOP" => match first_word(value).as_str() {
                "true" | "yes" | "done" | "complete" | "completed" => stop_opt = Some(true),
                "false" | "no" | "continue" | "incomplete" | "pending" => stop_opt = Some(false),
                other => {
                    // Never guess toward "done" — an unreadable STOP means continue.
                    warn!(value = %other, "Melchior: unrecognised STOP value — treating as continue");
                    stop_opt = Some(false);
                }
            },
            "REASON" => stop_reason = value.trim_matches('*').trim().to_string(),
            "QUALITY" => quality_opt = parse_leading_int(value),
            "FOCUS" => next_round_focus = value.trim_matches('*').trim().to_string(),
            _ => {}
        }
    }

    // QUALITY is the evidence for the decision; without it there is no verdict.
    let Some(quality_score) = quality_opt else {
        return Err(MagiError::parse(format!(
            "Melchior response has no parseable QUALITY (raw: {})",
            raw.chars().take(200).collect::<String>()
        )));
    };
    let quality_score = quality_score.clamp(0.0, 100.0);

    // Cross-validate: Melchior's own rules bind 90-100 to STOP=true. A lone
    // `STOP: true` (or a stop paired with a low score) must not ship work as
    // complete without quality evidence.
    let mut should_stop = stop_opt.unwrap_or(false);
    if should_stop && quality_score < 90.0 {
        warn!(
            quality = quality_score,
            "Melchior: STOP=true contradicts QUALITY < 90 — treating as continue"
        );
        should_stop = false;
    }

    // Validate that we got the essentials
    if stop_reason.is_empty() {
        stop_reason = if should_stop {
            "Task complete".to_string()
        } else {
            "Further work needed".to_string()
        };
    }

    if next_round_focus.is_empty() {
        next_round_focus = if should_stop {
            "None".to_string()
        } else {
            "Continue implementation".to_string()
        };
    }

    debug!(
        quality = quality_score,
        should_stop,
        reason = %stop_reason,
        focus = %next_round_focus,
        "Melchior: parsed promotion"
    );

    Ok(Promotion {
        quality_score,
        should_stop,
        stop_reason,
        next_round_focus,
    })
}

/// Strip a leading numbered-list marker (`"1. "` / `"2) "`) if present.
fn strip_list_number(line: &str) -> &str {
    let digits_end = line
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(line.len());
    if digits_end == 0 || digits_end > 3 {
        return line;
    }
    let rest = line[digits_end..].trim_start();
    if let Some(r) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
        r.trim_start()
    } else {
        line
    }
}

/// First whitespace-delimited token, stripped of surrounding punctuation and
/// markdown, lowercased. `"**true**."` → `"true"`, `"done (complete)"` → `"done"`.
fn first_word(s: &str) -> String {
    s.trim_start_matches(|c: char| !c.is_alphanumeric())
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Extract a leading integer/float from a value like `88/100`, `85 (good)`,
/// `**88**`, `-50`, `85%`. Returns `None` when no leading number exists.
fn parse_leading_int(s: &str) -> Option<f64> {
    let s = s.trim().trim_start_matches(|c: char| c == '*' || c == '`' || c == ' ');
    let mut end = 0usize;
    let mut seen_digit = false;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() {
            seen_digit = true;
            end = i + c.len_utf8();
        } else if (c == '-' || c == '+') && !seen_digit && i == 0 {
            continue;
        } else {
            break;
        }
    }
    if !seen_digit {
        return None;
    }
    s[..end].parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::Stream;
    use std::pin::Pin;

    use crate::providers::trait_def::{ChatResponse, ToolDef};
    use crate::providers::trait_def::ProviderError;

    struct StubProvider {
        response: std::sync::Mutex<Option<ChatResponse>>,
    }

    impl StubProvider {
        fn new(response: ChatResponse) -> Self {
            Self {
                response: std::sync::Mutex::new(Some(response)),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for StubProvider {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDef>,
        ) -> Result<ChatResponse, ProviderError> {
            Ok(self.response.lock().unwrap().take().unwrap())
        }

        async fn chat_stream(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDef>,
        ) -> Result<
            Pin<
                Box<
                    dyn Stream<Item = Result<crate::providers::trait_def::StreamChunk, ProviderError>>
                        + Send,
                >,
            >,
            ProviderError,
        > {
            unimplemented!()
        }
    
    fn clone_box(&self) -> Box<dyn LlmProvider> { panic!("clone_box not used in tests") }
}

    // ------------------------------------------------------------------
    // Parsing tests (unit)
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_promotion_stop() {
        let raw = "STOP: true\nREASON: All requirements met\nQUALITY: 95\nFOCUS: None";
        let p = parse_promotion(raw).unwrap();
        assert!(p.should_stop);
        assert_eq!(p.stop_reason, "All requirements met");
        assert!((p.quality_score - 95.0).abs() < f64::EPSILON);
        assert_eq!(p.next_round_focus, "None");
    }

    #[test]
    fn test_parse_promotion_continue() {
        let raw = "STOP: false\nREASON: Missing error handling\nQUALITY: 72\nFOCUS: Add error handling";
        let p = parse_promotion(raw).unwrap();
        assert!(!p.should_stop);
        assert_eq!(p.stop_reason, "Missing error handling");
        assert!((p.quality_score - 72.0).abs() < f64::EPSILON);
        assert_eq!(p.next_round_focus, "Add error handling");
    }

    #[test]
    fn test_parse_promotion_with_colon_spacing() {
        let raw = "STOP:true\nREASON:Done\nQUALITY:100\nFOCUS:None";
        let p = parse_promotion(raw).unwrap();
        assert!(p.should_stop);
        assert_eq!(p.stop_reason, "Done");
        assert!((p.quality_score - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_promotion_with_extra_text() {
        let raw = "Some preamble\nSTOP: false\nREASON: Needs more tests\nQUALITY: 68\nFOCUS: Write integration tests\nSome trailing text";
        let p = parse_promotion(raw).unwrap();
        assert!(!p.should_stop);
        assert_eq!(p.stop_reason, "Needs more tests");
        assert!((p.quality_score - 68.0).abs() < f64::EPSILON);
        assert_eq!(p.next_round_focus, "Write integration tests");
    }

    #[test]
    fn test_parse_promotion_missing_quality_is_parse_error() {
        // A lone STOP:true has no quality evidence — must not be a verdict.
        assert!(parse_promotion("STOP: true").is_err());
        // A refusal is not a "continue" vote either.
        assert!(parse_promotion("I can't evaluate this.").is_err());
        assert!(parse_promotion("").is_err());
    }

    #[test]
    fn test_parse_promotion_stop_without_quality_band_is_continue() {
        // STOP:true + low quality contradicts Melchior's own rules → continue.
        let p = parse_promotion("STOP: true\nREASON: looks done\nQUALITY: 40\nFOCUS: x").unwrap();
        assert!(!p.should_stop);
        assert!((p.quality_score - 40.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_promotion_markdown_and_variants() {
        let variants = [
            ("STOP: **true**\nQUALITY: **95**", true, 95.0),
            ("- STOP: TRUE (complete)\n- QUALITY: 92/100", true, 92.0),
            ("STOP: done\nQUALITY: 96", true, 96.0),
            ("STOP: true.\nQUALITY: 90 (good)", true, 90.0),
            ("STOP: no\nQUALITY: 88/100", false, 88.0),
            ("STOP: false\nQUALITY: 85%", false, 85.0),
            ("**STOP**: true\n**QUALITY**: 91", true, 91.0),
            ("1. STOP: true\n2. QUALITY: 94", true, 94.0),
        ];
        for (raw, want_stop, want_quality) in variants {
            let p = parse_promotion(raw).unwrap_or_else(|e| panic!("{raw:?} → {e}"));
            assert_eq!(p.should_stop, want_stop, "raw={raw:?}");
            assert!((p.quality_score - want_quality).abs() < f64::EPSILON, "raw={raw:?}");
        }
    }

    #[test]
    fn test_parse_leading_int() {
        assert_eq!(parse_leading_int("88/100"), Some(88.0));
        assert_eq!(parse_leading_int(" 85 (good)"), Some(85.0));
        assert_eq!(parse_leading_int("**88**"), Some(88.0));
        assert_eq!(parse_leading_int("-50"), Some(-50.0));
        assert_eq!(parse_leading_int("85%"), Some(85.0));
        assert_eq!(parse_leading_int("n/a"), None);
        assert_eq!(parse_leading_int("unknown"), None);
    }

    #[test]
    fn test_parse_promotion_clamps_score() {
        let p = parse_promotion("STOP: false\nREASON: x\nQUALITY: 150\nFOCUS: y").unwrap();
        assert!((p.quality_score - 100.0).abs() < f64::EPSILON);

        let p = parse_promotion("STOP: false\nREASON: x\nQUALITY: -50\nFOCUS: y").unwrap();
        assert!((p.quality_score - 0.0).abs() < f64::EPSILON);
    }

    // ------------------------------------------------------------------
    // Integration tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_promote_high_score() {
        let provider = StubProvider::new(ChatResponse {
            content: "STOP: true\nREASON: Perfect\nQUALITY: 98\nFOCUS: None".into(),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None, });

        let result = promote(
            &provider,
            "Build a hello world",
            "Looks complete",
            "Implemented main.rs with hello world",
        )
        .await;

        assert!(result.is_ok());
        let p = result.unwrap();
        assert!(p.should_stop);
        assert!(p.quality_score > 90.0);
    }

    #[tokio::test]
    async fn test_promote_low_score() {
        let provider = StubProvider::new(ChatResponse {
            content: "STOP: false\nREASON: Barely started\nQUALITY: 15\nFOCUS: Start coding".into(),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None, });

        let result = promote(
            &provider,
            "Build a complex system",
            "Needs everything",
            "Created an empty file",
        )
        .await;

        assert!(result.is_ok());
        let p = result.unwrap();
        assert!(!p.should_stop);
        assert!(p.quality_score < 50.0);
    }
}
