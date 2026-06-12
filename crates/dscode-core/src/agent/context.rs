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

/// Build a [`ContextPacket`] from conversation history, a system prompt, tool
/// definitions, and optional wiki knowledge nodes.
///
/// * `history`    — previous conversation turns (user + assistant + tool messages).
/// * `system_prompt` — the base system prompt (e.g. "You are a coding agent…").
/// * `tools`      — tool definitions from the registry (`ToolRegistry::to_openai_tools()`).
/// * `wiki_nodes` — knowledge snippets retrieved from the two-layer wiki.
/// * `max_history_messages` — maximum number of historical messages to include
///   (newest first), to stay within the model's context window.
pub fn build_context(
    history: &[Message],
    system_prompt: &str,
    tools: &[ToolDef],
    wiki_nodes: &[String],
    max_history_messages: usize,
) -> ContextPacket {
    let mut messages = Vec::new();

    // 1. Build the system prompt, appending wiki knowledge if present.
    let system_content = if wiki_nodes.is_empty() {
        system_prompt.to_string()
    } else {
        let mut enriched = system_prompt.to_string();
        enriched.push_str("\n\n## Relevant Knowledge\n");
        for (i, node) in wiki_nodes.iter().enumerate() {
            enriched.push_str(&format!("\n### Knowledge {}\n{}\n", i + 1, node));
        }
        enriched
    };

    messages.push(Message {
        role: Role::System,
        content: MessageContent::Text(system_content),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None, created_at: 0,
    });

    // 2. Append the most recent N messages from history.
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

/// Estimate token count from text. Simple heuristic: ~1 token per 3.5 bytes.
/// Accurate enough for threshold decisions (±15%).
pub fn count_tokens(text: &str) -> u64 {
    (text.len() as f64 / 3.5) as u64
}

/// Estimate tokens for a list of messages.
pub fn count_message_tokens(messages: &[Message]) -> u64 {
    messages
        .iter()
        .map(|m| match &m.content {
            MessageContent::Text(s) => count_tokens(s),
            MessageContent::Parts(_) => count_tokens(&serde_json::to_string(m).unwrap_or_default()),
        })
        .sum()
}

/// Build a compression summary prompt for a chunk of messages.
pub fn compression_prompt(old_messages: &[Message]) -> String {
    let convo = old_messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| format!(
            "[{}]: {}",
            match m.role { Role::User => "User", Role::Assistant => "Agent", Role::Tool => "Tool", _ => "System" },
            m.content.as_text().unwrap_or("")
        ))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "Condense the following conversation excerpt into a structured summary. \
         Preserve all critical information:\n\
         - Files edited and what changed\n\
         - Decisions made and reasoning\n\
         - Facts learned about the codebase\n\
         - Errors encountered and fixes\n\
         - Tool calls and their brief results\n\
         Keep the summary under 500 words.\n\n\
         === CONVERSATION ===\n{}\n=== END ===",
        convo
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: MessageContent::Text(text.to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None, created_at: 0,
        }
    }

    #[test]
    fn test_build_context_basic() {
        let history = vec![
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi there!"),
        ];
        let tools = vec![];
        let wiki: Vec<String> = vec![];
        let ctx = build_context(&history, "You are helpful.", &tools, &wiki, 100);

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
        let wiki: Vec<String> = vec![];
        let ctx = build_context(&history, "System", &tools, &wiki, 5);

        // system + 5 most recent history messages
        assert_eq!(ctx.messages.len(), 6);
        assert_eq!(
            ctx.messages[1].content.as_text().unwrap(),
            "msg 5"
        );
    }

    #[test]
    fn test_build_context_with_wiki() {
        let history = vec![];
        let tools = vec![];
        let wiki = vec!["Node A".to_string(), "Node B".to_string()];
        let ctx = build_context(&history, "You are helpful.", &tools, &wiki, 100);

        let system_text = ctx.messages[0].content.as_text().unwrap();
        assert!(system_text.contains("## Relevant Knowledge"));
        assert!(system_text.contains("Node A"));
        assert!(system_text.contains("Node B"));
    }

    #[test]
    fn test_build_context_includes_tools() {
        let history = vec![];
        let tools = vec![
            ToolDef::new("do_bash", "Run a bash command", serde_json::json!({})),
            ToolDef::new("do_read", "Read a file", serde_json::json!({})),
        ];
        let wiki: Vec<String> = vec![];
        let ctx = build_context(&history, "System", &tools, &wiki, 100);

        assert_eq!(ctx.tools.len(), 2);
        assert_eq!(ctx.tools[0].function.name, "do_bash");
    }
}
