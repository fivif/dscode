//! CompressionPipeline — multi-level context compression.
//!
//! When the conversation outgrows the model's token budget, this pipeline applies
//! escalating levels of compression to keep the agent running without hitting
//! hard context-limit errors:
//!
//! | Level | Threshold     | Strategy                                                  |
//! |-------|---------------|-----------------------------------------------------------|
//! | L1    | >80% window   | Truncate oldest non-system messages; never split pairs.   |
//! | L2    | >85% window   | Tag-compress verbose tool outputs (>2000 → 500 chars).    |
//! | L3    | >90% window   | LLM summarise oldest 70% into a system summary.            |
//! | L4    | >95% window   | Chunk + continuation hint so the agent carries on.        |
//!
//! Only one compression pass is allowed per turn (`applied_this_turn` guard).

use tracing::{info, warn};

use crate::agent::context::{compression_prompt, count_message_tokens, count_tokens};
use crate::config::settings::ContextConfig;
use crate::providers::trait_def::{LlmProvider, Message, MessageContent, Role};
use crate::safety::guard::SafetyGuard;


// ── CompressionAction ──────────────────────────────────────────────────────

/// The outcome of one compression pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressionAction {
    /// No compression was needed or possible.
    None,
    /// Oldest non-system messages were dropped (L1).
    Truncated { removed: usize },
    /// Verbose tool outputs were summarised to short tags (L2).
    TagCompressed { compressed: usize },
    /// Oldest 70 % of messages were LLM-summarised and optionally fed to the
    /// summary (L3).
    LlmSummarized { summarized: usize, summary_tokens: u64 },
    /// The context was chunked and a continuation hint inserted (L4).
    Chunked { chunks: usize },
}

// ── CompressionPipeline ────────────────────────────────────────────────────

/// Multi-level context compression pipeline.
///
/// Every ReAct iteration calls [`apply`](CompressionPipeline::apply) before the
/// provider call.  The pipeline checks token utilisation against the configured
/// window and escalates through the four levels as needed.
pub struct CompressionPipeline {
    /// Context-window sizing and threshold configuration.
    pub context_config: ContextConfig,
    /// Whether compression was already applied during the current user turn.
    pub applied_this_turn: bool,
}

impl CompressionPipeline {
    /// Create a new pipeline from a [`ContextConfig`].
    pub fn new(context_config: ContextConfig) -> Self {
        Self {
            context_config,
            applied_this_turn: false,
        }
    }

    /// Reset the one-compression-per-turn guard.
    ///
    /// Call this at the start of each `execute()` / `execute_stream()` call so
    /// compression can fire again for a new user message.
    pub fn reset(&mut self) {
        self.applied_this_turn = false;
    }

    /// Apply the appropriate compression level, returning the action taken.
    ///
    /// # Arguments
    ///
    /// * `messages`     — mutable conversation history (modified in-place).
    /// * `provider`     — LLM backend used for L3 summarisation.
    /// * `_safety_guard`— reserved for future path / command validation during
    ///                     compression (e.g. sanitising tool outputs).
    pub async fn apply(
        &mut self,
        messages: &mut Vec<Message>,
        provider: &dyn LlmProvider,
        _safety_guard: Option<&SafetyGuard>,
    ) -> CompressionAction {
        if self.applied_this_turn {
            return CompressionAction::None;
        }

        let window = self.context_config.window_tokens as f64;

        let sys_refs: Vec<&Message> =
            messages.iter().filter(|m| m.role == Role::System).collect();
        let hist_refs: Vec<&Message> =
            messages.iter().filter(|m| m.role != Role::System).collect();

        let sys_tok = count_message_tokens(&sys_refs);
        let hist_tok = count_message_tokens(&hist_refs);
        let total_tok = sys_tok + hist_tok;
        let ratio = total_tok as f64 / window;

        // ── L0: Zero-cost snip of stale tool results (always when >60%) ──
        if ratio > 0.60 {
            let snipped = Self::snip_stale_tool_results(messages, 3);
            if snipped > 0 {
                info!(snipped, "Compression L0: snipped stale tool results");
            }
        }

        // ── L1: Truncate (>80%) ─────────────────────────────────────────
        if ratio > 0.80 && ratio <= 0.85 {
            let action = Self::truncate_oldest(messages);
            let applied = !matches!(action, CompressionAction::None);
            if applied {
                self.applied_this_turn = true;
            }
            return action;
        }

        // ── L2: Tag-compress verbose tool outputs (>85%) ────────────────
        if ratio > 0.85 && ratio <= 0.90 {
            let action = Self::tag_compress_tool_outputs(messages);
            if matches!(action, CompressionAction::TagCompressed { .. }) {
                self.applied_this_turn = true;
                return action;
            }
            // Fall back to truncation if tag compression was insufficient.
            let trunc = Self::truncate_oldest(messages);
            self.applied_this_turn = true;
            return trunc;
        }

        // ── L3: LLM summarise (>90%) ────────────────────────────────────
        if ratio > 0.90 && ratio <= 0.95 {
            let action = Self::llm_summarize(messages, provider).await;
            if matches!(action, CompressionAction::LlmSummarized { .. }) {
                self.applied_this_turn = true;
                return action;
            }
            let trunc = Self::truncate_oldest(messages);
            self.applied_this_turn = true;
            return trunc;
        }

        // ── L4: Chunk (>95%) ────────────────────────────────────────────
        if ratio > 0.95 {
            let action = Self::chunk_and_continue(messages);
            self.applied_this_turn = true;
            return action;
        }

        CompressionAction::None
    }

    // ── L0 helpers ───────────────────────────────────────────────────────

    /// Replace older Tool role message bodies with a re-read placeholder,
    /// keeping the most recent `keep_recent` tool results intact.
    fn snip_stale_tool_results(messages: &mut [Message], keep_recent: usize) -> usize {
        const PLACEHOLDER: &str = "[Content snipped — re-read if needed]";
        let tool_idxs: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == Role::Tool)
            .map(|(i, _)| i)
            .collect();
        if tool_idxs.len() <= keep_recent {
            return 0;
        }
        let snip_end = tool_idxs.len() - keep_recent;
        let mut n = 0;
        for &idx in &tool_idxs[..snip_end] {
            let text = messages[idx].content.as_text().unwrap_or("");
            if text.len() > 120 && text != PLACEHOLDER {
                messages[idx].content = MessageContent::Text(PLACEHOLDER.into());
                n += 1;
            }
        }
        n
    }

    // ── L1 helpers ───────────────────────────────────────────────────────

    /// Truncate the oldest ~50 % of non-system messages.
    ///
    /// **Never** splits tool_call / tool_result pairs: the split point is
    /// advanced forward past orphaned `Tool` messages and past `Assistant`
    /// messages whose `tool_calls` would end up without results.
    fn truncate_oldest(messages: &mut Vec<Message>) -> CompressionAction {
        // Collect indices of all non-system messages.
        let non_sys_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role != Role::System)
            .map(|(i, _)| i)
            .collect();

        if non_sys_indices.len() < 2 {
            return CompressionAction::None;
        }

        // Target: drop the oldest 50 % of non-system messages.
        let mut remove_count = (non_sys_indices.len() as f64 * 0.5) as usize;
        if remove_count == 0 {
            remove_count = 1;
        }

        // Align the split point so we never orphan a tool_call from its result.
        while remove_count < non_sys_indices.len() {
            let msg_idx = non_sys_indices[remove_count];
            let msg = &messages[msg_idx];
            if msg.role == Role::Tool {
                // Tool message must stay with its assistant tool_call; skip it.
                remove_count += 1;
            } else if msg.role == Role::Assistant
                && msg.tool_calls.as_ref().map_or(false, |tc| !tc.is_empty())
            {
                // Assistant with tool_calls — skip it AND all following Tool
                // messages so we keep the complete round-trip.
                remove_count += 1;
                while remove_count < non_sys_indices.len()
                    && messages[non_sys_indices[remove_count]].role == Role::Tool
                {
                    remove_count += 1;
                }
            } else {
                break;
            }
        }

        if remove_count == 0 || remove_count >= non_sys_indices.len() {
            return CompressionAction::None;
        }

        // Rebuild: keep system messages + non-system messages from the aligned
        // split point onward.
        let keep_start_idx = non_sys_indices[remove_count];
        let sys: Vec<Message> = messages
            .iter()
            .filter(|m| m.role == Role::System)
            .cloned()
            .collect();
        let keep: Vec<Message> = messages[keep_start_idx..].to_vec();
        let removed = messages.len() - sys.len() - keep.len();

        *messages = sys;
        messages.extend(keep);

        info!(removed, "L1: truncated oldest non-system messages");
        CompressionAction::Truncated { removed }
    }

    // ── L2 helpers ───────────────────────────────────────────────────────

    /// Replace verbose tool outputs (>2000 chars) with a short summary tag
    /// containing the first 500 characters.
    fn tag_compress_tool_outputs(messages: &mut Vec<Message>) -> CompressionAction {
        let mut compressed = 0usize;

        for msg in messages.iter_mut() {
            if msg.role != Role::Tool {
                continue;
            }
            let text = match &msg.content {
                MessageContent::Text(s) => s.clone(),
                MessageContent::Parts(_) => continue,
            };
            let char_count = text.chars().count();
            if char_count > 2000 {
                let prefix: String = text.chars().take(500).collect();
                let summary = format!(
                    "[tool output truncated: {} → 500 chars] {}…",
                    char_count, prefix
                );
                msg.content = MessageContent::Text(summary);
                compressed += 1;
            }
        }

        if compressed > 0 {
            info!(compressed, "L2: tag-compressed verbose tool outputs");
            CompressionAction::TagCompressed { compressed }
        } else {
            CompressionAction::None
        }
    }

    // ── L3 helpers ───────────────────────────────────────────────────────

    /// Ask the LLM to summarise the oldest 70 % of non-system messages.
    async fn llm_summarize(
        messages: &mut Vec<Message>,
        provider: &dyn LlmProvider,
    ) -> CompressionAction {
        let non_sys: Vec<(usize, &Message)> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role != Role::System)
            .collect();

        if non_sys.is_empty() {
            return CompressionAction::None;
        }

        let mut compress_count = (non_sys.len() as f64 * 0.7) as usize;
        // Align: don't split tool chains.
        while compress_count < non_sys.len() {
            let (_, msg) = non_sys[compress_count];
            if msg.role == Role::Tool {
                compress_count += 1;
            } else if msg.role == Role::Assistant
                && msg.tool_calls.as_ref().map_or(false, |tc| !tc.is_empty())
            {
                compress_count += 1;
                while compress_count < non_sys.len()
                    && non_sys[compress_count].1.role == Role::Tool
                {
                    compress_count += 1;
                }
            } else {
                break;
            }
        }

        if compress_count == 0 {
            return CompressionAction::None;
        }

        let old: Vec<Message> = non_sys[..compress_count]
            .iter()
            .map(|(_, m)| (*m).clone())
            .collect();

        let half_window = 65536u64; // conservative cap for the summarisation prompt
        let prompt = compression_prompt(&old, half_window);

        let mk_default_msg = || Message {
            role: Role::User,
            content: MessageContent::Text(String::new()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            created_at: 0,
        };

        let summary = match provider
            .chat(
                vec![Message {
                    content: MessageContent::Text(prompt),
                    ..mk_default_msg()
                }],
                vec![],
            )
            .await
        {
            Ok(r) => r.content,
            Err(e) => {
                warn!(%e, "L3: LLM summarisation failed, falling back");
                return CompressionAction::None;
            }
        };

        if summary.is_empty() {
            return CompressionAction::None;
        }

        let sys_content = messages
            .iter()
            .find(|m| m.role == Role::System)
            .and_then(|m| m.content.as_text().map(|s| s.to_string()))
            .unwrap_or_default();

        let rest: Vec<Message> = messages
            .iter()
            .filter(|m| m.role != Role::System)
            .skip(compress_count)
            .cloned()
            .collect();

        let summarized = compress_count;
        let summary_tokens = count_tokens(&summary);

        *messages = vec![Message {
            role: Role::System,
            content: MessageContent::Text(format!(
                "{}\n\n## Conversation Summary (L3 Compression)\n{}",
                sys_content, summary
            )),
            ..mk_default_msg()
        }];
        messages.extend(rest);

        info!(
            summarized,
            summary_tokens,
            "L3: LLM-summarised oldest messages"
        );
        CompressionAction::LlmSummarized {
            summarized,
            summary_tokens,
        }
    }

    // ── L4 helpers ───────────────────────────────────────────────────────

    /// Keep only the most recent ~20 % of non-system messages and inject a
    /// continuation hint so the agent knows there is more work to do.
    fn chunk_and_continue(messages: &mut Vec<Message>) -> CompressionAction {
        let sys: Vec<Message> = messages
            .iter()
            .filter(|m| m.role == Role::System)
            .cloned()
            .collect();

        let non_sys: Vec<&Message> = messages
            .iter()
            .filter(|m| m.role != Role::System)
            .collect();

        if non_sys.is_empty() {
            return CompressionAction::None;
        }

        let keep_count = ((non_sys.len() as f64) * 0.2).max(1.0) as usize;
        let mut keep_start = non_sys.len().saturating_sub(keep_count);

        // Align: don't start with orphaned Tool messages.
        while keep_start < non_sys.len() && non_sys[keep_start].role == Role::Tool {
            keep_start += 1;
        }

        let keep: Vec<Message> = non_sys[keep_start..].iter().map(|m| (*m).clone()).collect();
        let chunks = if keep_start > 0 { 1 } else { 0 };

        let hint = format!(
            "[CONTINUATION] Previous context was chunked (L4). {} messages removed. \
             Pick up from the last checkpoint.",
            keep_start
        );

        let sys_content = sys
            .first()
            .and_then(|m| m.content.as_text().map(|s| s.to_string()))
            .unwrap_or_default();

        let mk_msg = || Message {
            role: Role::System,
            content: MessageContent::Text(String::new()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            created_at: 0,
        };

        *messages = vec![Message {
            content: MessageContent::Text(format!("{}\n\n{}", sys_content, hint)),
            ..mk_msg()
        }];
        messages.extend(keep);

        info!(chunks, "L4: chunked context with continuation hint");
        CompressionAction::Chunked { chunks }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

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
            reasoning_content: None,
            created_at: 0,
        }
    }

    fn make_assistant_with_tool_calls(
        content: &str,
        tc_ids: &[&str],
    ) -> Message {
        use crate::providers::trait_def::{FunctionCall, ToolCall};
        Message {
            role: Role::Assistant,
            content: MessageContent::Text(content.to_string()),
            name: None,
            tool_calls: Some(
                tc_ids
                    .iter()
                    .map(|&id| ToolCall {
                        id: id.to_string(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: "test_tool".into(),
                            arguments: "{}".into(),
                        },
                    })
                    .collect(),
            ),
            tool_call_id: None,
            reasoning_content: None,
            created_at: 0,
        }
    }

    fn make_tool(id: &str, text: &str) -> Message {
        Message {
            role: Role::Tool,
            content: MessageContent::Text(text.to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: Some(id.to_string()),
            reasoning_content: None,
            created_at: 0,
        }
    }

    // ── L1 ───────────────────────────────────────────────────────────────

    #[test]
    fn test_truncate_oldest_basic() {
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, "u1"),
            make_msg(Role::Assistant, "a1"),
            make_msg(Role::User, "u2"),
            make_msg(Role::Assistant, "a2"),
            make_msg(Role::User, "u3"),
            make_msg(Role::Assistant, "a3"),
        ];
        let action = CompressionPipeline::truncate_oldest(&mut msgs);
        assert!(matches!(action, CompressionAction::Truncated { .. }));
        assert!(msgs.len() < 7, "messages should have been truncated");
        assert_eq!(msgs[0].role, Role::System); // system always preserved
    }

    #[test]
    fn test_truncate_respects_tool_pairs() {
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, "run a command"),
            make_assistant_with_tool_calls("ok", &["tc1"]),
            make_tool("tc1", "output"),
            make_msg(Role::User, "another question"),
            make_msg(Role::Assistant, "answer"),
        ];
        let action = CompressionPipeline::truncate_oldest(&mut msgs);
        assert!(matches!(action, CompressionAction::Truncated { .. }));
        // After truncation, the split should not leave a tool message orphaned.
        let has_orphan_tool = msgs.iter().any(|m| {
            m.role == Role::Tool
                && !msgs.iter().any(|prev| {
                    prev.role == Role::Assistant
                        && prev
                            .tool_calls
                            .as_ref()
                            .map_or(false, |tc| tc.iter().any(|t| t.id == m.tool_call_id.clone().unwrap_or_default()))
                })
        });
        assert!(!has_orphan_tool, "no orphaned tool messages allowed");
    }

    // ── L2 ───────────────────────────────────────────────────────────────

    #[test]
    fn test_tag_compress_tool_outputs() {
        let long_text = "x".repeat(2500);
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, "go"),
            make_tool("t1", &long_text),
            make_msg(Role::Assistant, "done"),
        ];
        let action = CompressionPipeline::tag_compress_tool_outputs(&mut msgs);
        assert!(
            matches!(action, CompressionAction::TagCompressed { compressed } if compressed == 1)
        );
        if let MessageContent::Text(s) = &msgs[2].content {
            assert!(s.contains("truncated"));
            assert!(s.chars().count() < 2500);
        } else {
            panic!("expected text content");
        }
    }

    #[test]
    fn test_tag_compress_short_output_untouched() {
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_tool("t1", "short output"),
        ];
        let action = CompressionPipeline::tag_compress_tool_outputs(&mut msgs);
        assert_eq!(action, CompressionAction::None);
        assert_eq!(
            msgs[1].content.as_text().unwrap(),
            "short output"
        );
    }
}
