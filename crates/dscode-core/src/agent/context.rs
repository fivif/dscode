//! Context packet builder — assembles the complete context for each LLM call.
//!
//! Builds a [`ContextPacket`] containing the system prompt, conversation history
//! (trimmed to respect token budget), and tool definitions. The caller
//! (typically [`Forge`](super::forge::Forge)) prepends this context before
//! appending the current user message and starting the ReAct loop.

use crate::providers::trait_def::{Message, MessageContent, Role, ToolDef};

/// A complete context package ready to be sent to the LLM provider.
#[derive(Debug, Clone)]
pub struct ContextPacket {
    /// Ordered messages forming the conversation prefix (system + history).
    pub messages: Vec<Message>,
    /// Tool definitions available to the model.
    pub tools: Vec<ToolDef>,
}

/// Build a [`ContextPacket`] from conversation history, a system prompt, and tool
/// definitions.
///
/// * `history`    — previous conversation turns (user + assistant + tool messages).
/// * `system_prompt` — the base system prompt (e.g. "You are a coding agent…").
/// * `tools`      — tool definitions from the registry (`ToolRegistry::to_openai_tools()`).
/// * `max_history_messages` — maximum number of historical messages to include
///   (newest first), to stay within the model's context window.
pub fn build_context(
    history: &[Message],
    system_prompt: &str,
    tools: &[ToolDef],
    max_history_messages: usize,
) -> ContextPacket {
    let mut messages = Vec::new();

    messages.push(Message {
        role: Role::System,
        content: MessageContent::Text(system_prompt.to_string()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        created_at: 0,
    });

    // Append the most recent N messages from history.
    let history_len = history.len();
    let mut to_skip = if history_len > max_history_messages {
        history_len - max_history_messages
    } else {
        0
    };
    // Align: never start with a Tool message (orphans its tool_call)
    while to_skip < history_len && history[to_skip].role == Role::Tool {
        to_skip += 1;
    }

    for msg in &history[to_skip..] {
        messages.push(msg.clone());
    }

    ContextPacket {
        messages,
        tools: tools.to_vec(),
    }
}

/// Estimate token count from text. ~1 token per 2.5 characters.
/// Uses char count rather than byte length for better CJK approximation
/// (CJK characters are 1-3 bytes each but roughly 1-2 tokens).
pub fn count_tokens(text: &str) -> u64 {
    (text.chars().count() as f64 / 2.5) as u64
}

/// Estimate tokens for a list of messages.
/// Now also includes reasoning_content and tool_call name+arguments.
pub fn count_message_tokens(messages: &[&Message]) -> u64 {
    messages
        .iter()
        .map(|m| {
            let mut tokens = match &m.content {
                MessageContent::Text(s) => count_tokens(s),
                MessageContent::Parts(_) => {
                    count_tokens(&serde_json::to_string(m).unwrap_or_default())
                }
            };
            if let Some(ref rc) = m.reasoning_content {
                tokens += count_tokens(rc);
            }
            if let Some(ref tcs) = m.tool_calls {
                for tc in tcs {
                    tokens += count_tokens(&tc.function.name);
                    tokens += count_tokens(&tc.function.arguments);
                }
            }
            tokens
        })
        .sum()
}

/// Build a prompt asking the model to compress older conversation into a summary.
pub fn compression_prompt(old_messages: &[Message], max_summary_tokens: u64) -> String {
    let mut body = String::from(
        "Summarize the following conversation history concisely for future context. \
         Preserve decisions, file paths, errors, and open TODOs. \
         Aim for compact bullet points.\n\n---\n",
    );
    for m in old_messages {
        let role = format!("{:?}", m.role);
        let text = m.content.as_text().unwrap_or("");
        let excerpt: String = text.chars().take(800).collect();
        body.push_str(&format!("[{role}] {excerpt}\n"));
    }
    body.push_str(&format!(
        "\n---\nKeep the summary under ~{max_summary_tokens} tokens."
    ));
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::trait_def::ToolDef;

    fn make_msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: MessageContent::Text(text.into()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            created_at: 0,
        }
    }

    #[test]
    fn test_build_context_basic() {
        let history = vec![
            make_msg(Role::User, "hello"),
            make_msg(Role::Assistant, "hi"),
        ];
        let tools = vec![];
        let ctx = build_context(&history, "You are helpful.", &tools, 100);

        assert_eq!(ctx.messages.len(), 3); // system + 2 history
        assert_eq!(ctx.messages[0].role, Role::System);
        assert_eq!(
            ctx.messages[0].content.as_text().unwrap(),
            "You are helpful."
        );
    }

    #[test]
    fn test_build_context_trims_history() {
        let history: Vec<Message> = (0..10)
            .map(|i| make_msg(Role::User, &format!("msg {}", i)))
            .collect();
        let tools = vec![];
        let ctx = build_context(&history, "System", &tools, 5);

        assert_eq!(ctx.messages.len(), 6);
        assert_eq!(ctx.messages[1].content.as_text().unwrap(), "msg 5");
    }

    #[test]
    fn test_build_context_includes_tools() {
        let history = vec![];
        let tools = vec![
            ToolDef::new("do_bash", "Run a bash command", serde_json::json!({})),
            ToolDef::new("do_read", "Read a file", serde_json::json!({})),
        ];
        let ctx = build_context(&history, "System", &tools, 100);
        assert_eq!(ctx.tools.len(), 2);
    }

    #[test]
    fn test_count_tokens() {
        assert!(count_tokens("hello world") > 0);
    }
}
