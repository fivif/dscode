//! Casper brain — scrutinizes the PRD and previous round results.
//!
//! Casper reads the Product Requirements Document and all prior MAGI rounds,
//! identifies gaps, regressions, and areas needing attention, and produces a
//! focused scrutiny report that guides Balthasar's next execution round.

use tracing::debug;

use crate::providers::trait_def::{
    LlmProvider, Message, MessageContent, Role,
};

use super::scheduler::{MagiError, MagiRound};

/// The system prompt that primes Casper for the scrutiny role.
const CASPER_SYSTEM_PROMPT: &str = r#"You are Casper, the scrutiny brain of the MAGI system.
Your job is to review the PRD (Product Requirements Document) and previous execution rounds,
then identify:

1. What requirements have been met so far.
2. What requirements are still missing or incomplete.
3. Any regressions or bugs introduced.
4. Edge cases that have not been addressed.
5. Specific, actionable focus areas for the next execution round.

Be thorough and critical. Point out every gap and potential issue.
Your output will guide the execution brain (Balthasar) to fix remaining problems.

Respond with a detailed scrutiny report. Do NOT execute any code — just review and critique."#;

/// Run Casper's scrutiny phase.
///
/// # Arguments
/// * `provider` — the primary LLM provider.
/// * `prd` — the Product Requirements Document describing the task.
/// * `previous_rounds` — all completed MAGI rounds so far (empty on first round).
/// * `previous_scrutiny` — the scrutiny report from the last round (empty on first round).
///
/// # Returns
/// A scrutiny report string describing gaps, regressions, and focus areas.
pub async fn scrutinize(
    provider: &dyn LlmProvider,
    prd: &str,
    previous_rounds: &[MagiRound],
    previous_scrutiny: &str,
) -> Result<String, MagiError> {
    let mut messages = Vec::new();

    // System prompt
    messages.push(Message {
        role: Role::System,
        content: MessageContent::Text(CASPER_SYSTEM_PROMPT.to_string()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None, created_at: 0, });

    // Build the user prompt
    let user_prompt = build_scrutiny_prompt(prd, previous_rounds, previous_scrutiny);

    messages.push(Message {
        role: Role::User,
        content: MessageContent::Text(user_prompt),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None, created_at: 0, });

    debug!(
        round_count = previous_rounds.len(),
        prompt_len = messages.last().map(|m| m.content.as_text().map(|s| s.len()).unwrap_or(0)).unwrap_or(0),
        "Casper: sending scrutiny request"
    );

    let response = provider.chat(messages, vec![]).await?;

    debug!(
        response_len = response.content.len(),
        "Casper: received scrutiny report"
    );

    Ok(response.content)
}

/// Build the user prompt for Casper.
fn build_scrutiny_prompt(
    prd: &str,
    previous_rounds: &[MagiRound],
    previous_scrutiny: &str,
) -> String {
    let mut prompt = String::new();

    prompt.push_str("## PRD (Product Requirements Document)\n\n");
    prompt.push_str(prd);
    prompt.push_str("\n\n");

    if previous_rounds.is_empty() {
        prompt.push_str(
            "## First Round\n\n\
             This is the first execution round. Please review the PRD and identify:\n\
             - All required components\n\
             - Potential challenges and edge cases\n\
             - Specific focus areas for the first execution pass\n",
        );
    } else {
        prompt.push_str("## Previous Rounds Summary\n\n");

        for round in previous_rounds {
            prompt.push_str(&format!(
                "### Round {}\n\
                 **Scrutiny Focus:** {}\n\
                 **Execution Result:** {}\n\
                 **Quality Score:** {:.1}/100\n\
                 **Melchior's Note:** {}\n\n",
                round.round_number,
                round.scrutiny,
                round.execution,
                round.promotion.quality_score,
                round.promotion.stop_reason,
            ));
        }

        if !previous_scrutiny.is_empty() {
            prompt.push_str("## Previous Scrutiny Focus\n\n");
            prompt.push_str(previous_scrutiny);
            prompt.push_str("\n\n");
        }

        prompt.push_str(
            "## Your Task\n\n\
             Review the progress above and identify what still needs to be done. \
             Be specific about remaining gaps, regressions, and edge cases. \
             Provide actionable focus areas for the next execution round.\n",
        );
    }

    prompt
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

    #[tokio::test]
    async fn test_scrutinize_first_round() {
        let provider = StubProvider::new(ChatResponse {
            content: "All looks clear. Focus on the auth module.".into(),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None, });

        let result = scrutinize(
            &provider,
            "Build a login page",
            &[],
            "",
        )
        .await;

        assert!(result.is_ok());
        let report = result.unwrap();
        assert!(report.contains("auth"));
    }

    #[tokio::test]
    async fn test_scrutinize_with_previous_rounds() {
        use crate::magi::scheduler::Promotion;

        let provider = StubProvider::new(ChatResponse {
            content: "Round 1 left off with incomplete tests. Focus on testing.".into(),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None, });

        let previous = vec![MagiRound {
            round_number: 1,
            scrutiny: "Focus on core implementation".into(),
            execution: "Implemented basic structure".into(),
            promotion: Promotion {
                quality_score: 60.0,
                should_stop: false,
                stop_reason: "Missing tests".into(),
                next_round_focus: "Add tests".into(),
            },
        }];

        let result = scrutinize(
            &provider,
            "Build a CLI tool with tests",
            &previous,
            "",
        )
        .await;

        assert!(result.is_ok());
        let report = result.unwrap();
        assert!(report.contains("test"));
    }

    #[test]
    fn test_build_scrutiny_prompt_first_round() {
        let prompt = build_scrutiny_prompt("Build a web server", &[], "");
        assert!(prompt.contains("Build a web server"));
        assert!(prompt.contains("First Round"));
    }

    #[test]
    fn test_build_scrutiny_prompt_with_rounds() {
        use crate::magi::scheduler::Promotion;

        let rounds = vec![MagiRound {
            round_number: 1,
            scrutiny: "Check auth".into(),
            execution: "Added login".into(),
            promotion: Promotion {
                quality_score: 70.0,
                should_stop: false,
                stop_reason: "Need more".into(),
                next_round_focus: "Add dashboard".into(),
            },
        }];

        let prompt = build_scrutiny_prompt("Build app", &rounds, "last scrutiny");
        assert!(prompt.contains("Build app"));
        assert!(prompt.contains("Round 1"));
        assert!(prompt.contains("Check auth"));
        assert!(prompt.contains("last scrutiny"));
    }
}
