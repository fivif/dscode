//! Melchior brain — evaluates execution quality and decides stop/continue.
//!
//! Melchior reads the PRD, Casper's scrutiny report, and Balthasar's execution
//! output, then assigns a quality score (0-100) and decides whether the MAGI
//! spiral should stop or continue with a specific focus.

use tracing::debug;

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
fn parse_promotion(raw: &str) -> Result<Promotion, MagiError> {
    let raw = raw.trim();

    let mut should_stop = false;
    let mut stop_reason = String::new();
    let mut quality_score = 0.0f64;
    let mut next_round_focus = String::new();

    for line in raw.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("STOP:").or_else(|| line.strip_prefix("STOP ")) {
            let value = value.trim().to_lowercase();
            should_stop = value == "true" || value == "yes";
        } else if let Some(value) = line.strip_prefix("REASON:").or_else(|| line.strip_prefix("REASON ")) {
            stop_reason = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("QUALITY:").or_else(|| line.strip_prefix("QUALITY ")) {
            let value = value.trim();
            quality_score = value
                .parse::<f64>()
                .unwrap_or(50.0)
                .clamp(0.0, 100.0);
        } else if let Some(value) = line.strip_prefix("FOCUS:").or_else(|| line.strip_prefix("FOCUS ")) {
            next_round_focus = value.trim().to_string();
        }
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
    fn test_parse_promotion_defaults() {
        // Empty or missing fields get defaults
        let p = parse_promotion("STOP: true").unwrap();
        assert!(p.should_stop);
        assert!(!p.stop_reason.is_empty());
        assert!((p.quality_score - 0.0).abs() < f64::EPSILON);
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
