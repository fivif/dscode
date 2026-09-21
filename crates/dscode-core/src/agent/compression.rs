//! CompressionPipeline — multi-level context compression.
//!
//! When the conversation outgrows the model's token budget, this pipeline applies
//! escalating levels of compression to keep the agent running without hitting
//! hard context-limit errors:
//!
//! | Level | Threshold                        | Strategy                                       |
//! |-------|----------------------------------|------------------------------------------------|
//! | L0    | > `compress_threshold − 20%`     | Snip stale tool outputs (keep the last 3). **Lossy.** |
//! | L1    | > `compress_threshold` (80%)     | Truncate oldest non-system messages; never split pairs. |
//! | L2    | > `compress_threshold + 5%`      | Tag-compress verbose tool outputs (>2000 → 500 chars). |
//! | L3    | > `compress_threshold + 10%`     | LLM summarise oldest 70% into a system summary. |
//! | L4    | > `compress_threshold + 15%`     | Chunk + continuation hint so the agent carries on. |
//!
//! L1–L4 are anchored to the configured `compress_threshold` (default 0.8,
//! which reproduces the legacy 80/85/90/95 ladder exactly). L0 is anchored to
//! the same value, one step below L1: `compress_threshold − 20%`, i.e. 60 % at
//! the default. It used to be hard-wired to 60 %, which made the *only
//! irreversible level* the one level that ignored the user's setting — at
//! `compress_threshold = 0.9` the conversation was still being snipped from
//! 60 % on.
//!
//! L0 is the only level that runs below the others: it fires on every pass
//! once the window passes its threshold, and it is **irreversible** — the
//! placeholder replaces the tool output, which cannot be recovered from the
//! conversation. A shell/test result can only be reproduced by re-running the
//! command, not by "re-reading" it.
//!
//! # The configured window is an estimate
//!
//! Every threshold above compares an estimate against `window_tokens` — a
//! number the user typed, with nothing tying it to the window of the model
//! actually behind the channel. When it is too large (the 1M default in front
//! of a 128k model) the ladder never fires and the turn dies on the
//! provider's over-window 400. [`CompressionPipeline::force_apply`] is the
//! entry point for that case: the provider has *told us* we are over the
//! limit, which beats any estimate, so it skips the ratio test entirely.
//!
//! # Invariant: never compress away the live instruction
//!
//! No level may remove the last `User` message or anything after it. That
//! message is the turn's instruction; a prompt that reaches the provider with
//! only a `System` message and no user turn makes the model invent a
//! continuation (which the loop then reported as a successful `Complete`), and
//! Anthropic-compatible APIs reject it outright (`messages: []` → 400). Every
//! level therefore checks its split point against `split_keeps_instruction`
//! and returns [`CompressionAction::None`] rather than cutting past it.
//!
//! # One pass is not enough
//!
//! A long tool loop refills the window between iterations — each iteration can
//! append up to 24k chars of tool output. A hard one-pass-per-turn latch let
//! the window climb back over the limit until the provider rejected the
//! request (HTTP 400 is not retryable), killing the turn after dozens of
//! *successful* tool calls. [`CompressionPipeline::apply`] is therefore called
//! again every iteration; after the first pass the caller sets
//! [`cheap_only`](CompressionPipeline::cheap_only) so repeat passes only use
//! the zero-cost levels (L0/L1) and never pay for another LLM summarisation.

use tracing::{info, warn};

use crate::agent::context::{
    compression_prompt, count_message_tokens, count_tokens, count_tool_def_tokens,
};
use crate::config::settings::ContextConfig;
use crate::providers::trait_def::{LlmProvider, Message, MessageContent, Role, ToolDef};
use crate::safety::guard::SafetyGuard;


// ── CompressionAction ──────────────────────────────────────────────────────

/// The outcome of one compression pass.
///
/// L0 ([`Snipped`](CompressionAction::Snipped)) rewrites tool outputs in place
/// rather than removing messages, so it is reported on its own when it is the
/// only level that fired. Callers that must know whether the prompt changed at
/// all should also compare the token count before and after, as the ReAct loop
/// does for [`CompressionPipeline::apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressionAction {
    /// No compression was needed or possible.
    None,
    /// Stale tool outputs were replaced by a re-run placeholder (L0). The only
    /// irreversible level: the original output can no longer be read back from
    /// the conversation.
    Snipped { snipped: usize },
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
    /// Cheap mode: only the zero-cost levels (L0/L1) run. Set by the caller
    /// for every pass after the first one in a turn, so a long tool loop can
    /// keep re-compressing without paying for another LLM summarisation of an
    /// already-summarised conversation.
    pub cheap_only: bool,
    /// Tokens consumed by the tool definitions sent with every request. They
    /// are part of the real prompt but never part of `messages`, so the caller
    /// must hand them in ([`CompressionPipeline::with_tools`]); otherwise a
    /// large MCP tool set is invisible to the threshold maths.
    pub tool_tokens: u64,
}

impl CompressionPipeline {
    /// Create a new pipeline from a [`ContextConfig`].
    pub fn new(context_config: ContextConfig) -> Self {
        Self {
            context_config,
            applied_this_turn: false,
            cheap_only: false,
            tool_tokens: 0,
        }
    }

    /// Account for the tool definitions that accompany every request.
    pub fn with_tools(mut self, tools: &[ToolDef]) -> Self {
        self.tool_tokens = count_tool_def_tokens(tools);
        self
    }

    /// Reset the per-turn state (one-compression guard + cheap mode).
    ///
    /// Call this at the start of each `execute()` / `execute_stream()` call so
    /// compression can fire again for a new user message.
    pub fn reset(&mut self) {
        self.applied_this_turn = false;
        self.cheap_only = false;
    }

    /// Record that a pass actually changed something, and hand the action back.
    ///
    /// Only a real change counts: an earlier version set `applied_this_turn`
    /// on every L2/L3 attempt, including the fallback to L1 that returned
    /// [`CompressionAction::None`], so a turn could end up latched with
    /// nothing compressed at all.
    fn applied(&mut self, action: CompressionAction) -> CompressionAction {
        if !matches!(action, CompressionAction::None) {
            self.applied_this_turn = true;
        }
        action
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
        let window = self.context_config.window_tokens as f64;
        if window <= 0.0 {
            return CompressionAction::None;
        }

        let sys_refs: Vec<&Message> =
            messages.iter().filter(|m| m.role == Role::System).collect();
        let hist_refs: Vec<&Message> =
            messages.iter().filter(|m| m.role != Role::System).collect();

        let sys_tok = count_message_tokens(&sys_refs);
        let hist_tok = count_message_tokens(&hist_refs);
        // Tool definitions are sent on every request and occupy the same
        // window as the messages do.
        let total_tok = sys_tok + hist_tok + self.tool_tokens;
        let ratio = total_tok as f64 / window;

        // Escalating thresholds anchored to the configured compress_threshold.
        // Default 0.8 reproduces the legacy 80/85/90/95 ladder exactly.
        let base = self.context_config.compress_threshold.clamp(0.1, 0.85);
        let l1 = base;
        let l2 = base + 0.05;
        let l3 = base + 0.10;
        let l4 = base + 0.15;

        // ── L0: Zero-cost snip of stale tool results ────────────────────
        // Anchored one step below L1 rather than hard-wired to 0.60: this is
        // the only *irreversible* level, so it has to follow the user's
        // `compress_threshold` like every other level. At the default 0.8 it
        // is still 0.60 — byte-identical to the legacy constant.
        // Lossy and irreversible: see the module docs.
        let l0 = l0_threshold(base);
        let mut l0_action: Option<CompressionAction> = None;
        if ratio > l0 {
            let snipped = Self::snip_stale_tool_results(messages, 3);
            if snipped > 0 {
                info!(snipped, threshold = l0, "Compression L0: snipped stale tool results");
                l0_action = Some(CompressionAction::Snipped { snipped });
            }
        }

        // ── Repeat pass in the same turn: cheap levels only ─────────────
        // The window can refill between iterations (tool output), so a turn
        // must be able to compress more than once. The second and later passes
        // stay on L0/L1: another LLM summarisation is expensive and adds
        // little once a summary is already in the history.
        if self.cheap_only {
            if ratio > l1 {
                let action = Self::truncate_oldest(messages);
                return self.applied(prefer_level(action, l0_action));
            }
            return self.applied(l0_action.unwrap_or(CompressionAction::None));
        }

        // ── L1: Truncate ────────────────────────────────────────────────
        if ratio > l1 && ratio <= l2 {
            let action = Self::truncate_oldest(messages);
            return self.applied(prefer_level(action, l0_action));
        }

        // ── L2: Tag-compress verbose tool outputs ───────────────────────
        if ratio > l2 && ratio <= l3 {
            let action = Self::tag_compress_tool_outputs(messages);
            if matches!(action, CompressionAction::TagCompressed { .. }) {
                return self.applied(action);
            }
            // Fall back to truncation if tag compression was insufficient.
            let action = Self::truncate_oldest(messages);
            return self.applied(prefer_level(action, l0_action));
        }

        // ── L3: LLM summarise ───────────────────────────────────────────
        if ratio > l3 && ratio <= l4 {
            let action = Self::llm_summarize(messages, provider).await;
            if matches!(action, CompressionAction::LlmSummarized { .. }) {
                return self.applied(action);
            }
            let action = Self::truncate_oldest(messages);
            return self.applied(prefer_level(action, l0_action));
        }

        // ── L4: Chunk ───────────────────────────────────────────────────
        if ratio > l4 {
            let action = Self::chunk_and_continue(messages);
            if matches!(action, CompressionAction::None) {
                // Chunking refused (it would have dropped the live
                // instruction, or there is nothing to drop): fall back to the
                // cheapest level rather than leave the turn over budget.
                let action = Self::truncate_oldest(messages);
                return self.applied(prefer_level(action, l0_action));
            }
            return self.applied(action);
        }

        self.applied(l0_action.unwrap_or(CompressionAction::None))
    }

    /// Compress regardless of the configured thresholds — used when the
    /// provider has *told us* we are over the limit, which beats any estimate.
    ///
    /// [`apply`](CompressionPipeline::apply) compares an estimate against
    /// `window_tokens`, a number the user typed; when that number is larger
    /// than the real model's window the ladder never fires and the turn dies
    /// on an over-window 400. Here the provider has already rejected the
    /// request, so the ratio test is meaningless: this applies the deepest
    /// level that can run, least-destructive first —
    ///
    /// * L3 (LLM summary — keeps the conversation's information, costs one
    ///   provider call),
    /// * L4 (chunk: keep the recent 20 % + a continuation hint),
    /// * L2 (tag-compress verbose tool outputs, 500 chars kept),
    /// * L1 (truncate the oldest half),
    /// * L0 last, because it is the only **irreversible** level.
    ///
    /// The first level that changes something wins, and the module invariant
    /// still holds: no level may *remove* the last `User` message or any
    /// message after it (L2/L0 rewrite tool bodies in place, they never drop a
    /// message). Returns [`CompressionAction::None`] when no level could run —
    /// the caller must then fail the turn, because re-sending an unchanged body
    /// would just bill another rejected generation.
    ///
    /// `cheap_only` is deliberately ignored: the alternative to paying for a
    /// summary here is a dead turn, so saving money is not the priority.
    pub async fn force_apply(
        &mut self,
        messages: &mut Vec<Message>,
        provider: &dyn LlmProvider,
    ) -> CompressionAction {
        // L3 first: a summary removes most of the bytes while preserving the
        // information. Its own provider call can be rejected while the history
        // is over the limit — the cascade below falls through to the purely
        // local levels when that happens.
        let action = Self::llm_summarize(messages, provider).await;
        if !matches!(action, CompressionAction::None) {
            info!(action = ?action, "Compression forced (L3)");
            return self.applied(action);
        }

        // L4: keep only the most recent ~20 % plus a continuation hint.
        let action = Self::chunk_and_continue(messages);
        if !matches!(action, CompressionAction::None) {
            info!("Compression forced (L4)");
            return self.applied(action);
        }

        // L2: shrink verbose tool outputs, keeping their first 500 chars.
        let action = Self::tag_compress_tool_outputs(messages);
        if !matches!(action, CompressionAction::None) {
            info!("Compression forced (L2)");
            return self.applied(action);
        }

        // L1: drop the oldest half of the non-system messages.
        let action = Self::truncate_oldest(messages);
        if !matches!(action, CompressionAction::None) {
            info!("Compression forced (L1)");
            return self.applied(action);
        }

        // L0 last: the placeholder cannot be undone, and only re-running the
        // command reproduces the output it replaced.
        let snipped = Self::snip_stale_tool_results(messages, 3);
        if snipped > 0 {
            warn!(
                snipped,
                "Compression forced (L0): irreversible tool-output snip as a last resort"
            );
            return self.applied(CompressionAction::Snipped { snipped });
        }

        CompressionAction::None
    }

    // ── L0 helpers ───────────────────────────────────────────────────────

    /// Replace older Tool role message bodies with a re-run placeholder,
    /// keeping the most recent `keep_recent` tool results intact.
    ///
    /// This is the L0 level: it fires once the window passes
    /// [`l0_threshold`] (one step below L1), it is lossy, and it cannot be
    /// undone from the conversation. The placeholder must not promise a
    /// "re-read" — a shell/test result has to be produced again by re-running
    /// the command.
    fn snip_stale_tool_results(messages: &mut [Message], keep_recent: usize) -> usize {
        const PLACEHOLDER: &str =
            "[tool output snipped by context compression — re-run the command if you \
             need it again; for file reads, read the file again]";
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

        // Invariant: the split point may not pass the last user message (the
        // turn's live instruction). The alignment loop above stops at `User`
        // messages, so this is a belt-and-braces check — but it is the
        // invariant the whole module promises, so it is enforced here rather
        // than left implicit.
        if !split_keeps_instruction(messages, keep_start_idx) {
            warn!(
                split = keep_start_idx,
                "L1: refusing to truncate past the last user message"
            );
            return CompressionAction::None;
        }

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

        // Invariant: the last user message and everything after it must stay.
        // The alignment loop above walks forward past Tool rows and can run
        // off the end of a tail made only of tool output — which used to leave
        // `rest` empty and send the provider a single System message. The
        // model then "continued" the summary and forge reported Complete.
        if compress_count >= non_sys.len() {
            warn!("L3: refusing to summarise the whole conversation (no live instruction left)");
            return CompressionAction::None;
        }
        if !split_keeps_instruction(messages, non_sys[compress_count].0) {
            warn!("L3: refusing to summarise past the last user message");
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
            ..Default::default()
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

        // All System messages, not just the first: L1 keeps every one of them,
        // so L3/L4 dropping the rest here would lose prompt content the caller
        // never asked to compress.
        let sys_content = joined_system_content(messages);

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
    ///
    /// The split point is never allowed past the last user message: when the
    /// whole tail is tool output the alignment loop used to walk to the end,
    /// leaving a System-only prompt with no instruction at all.
    fn chunk_and_continue(messages: &mut Vec<Message>) -> CompressionAction {
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

        // Invariant: pull the split back to the last user message when the
        // alignment walked past it. Keeping everything from that message
        // onward is the whole point of L4 — the instruction and the tool
        // results it produced survive; older turns do not.
        if let Some(last_user) = non_sys.iter().rposition(|m| m.role == Role::User) {
            keep_start = keep_start.min(last_user);
        }

        if keep_start == 0 || keep_start >= non_sys.len() {
            // Nothing would be removed (or nothing could be kept safely).
            return CompressionAction::None;
        }

        let keep: Vec<Message> = non_sys[keep_start..].iter().map(|m| (*m).clone()).collect();
        let chunks = 1;

        let hint = format!(
            "[CONTINUATION] Previous context was chunked (L4). {} messages removed. \
             Pick up from the last checkpoint.",
            keep_start
        );

        let sys_content = joined_system_content(messages);

        let mk_msg = || Message {
            role: Role::System,
            content: MessageContent::Text(String::new()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            created_at: 0,
            ..Default::default()
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

/// Concatenate every `System` message's text.
///
/// L1 keeps all system messages; L3/L4 used to keep only the first one
/// (`find`/`first`), silently dropping any additional system content.
fn joined_system_content(messages: &[Message]) -> String {
    messages
        .iter()
        .filter(|m| m.role == Role::System)
        .filter_map(|m| m.content.as_text())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Whether a split at absolute index `split_idx` keeps the live instruction.
///
/// Everything from the last `User` message onward must survive, because that
/// message is the turn's task. Without a user message at all, the split must
/// at least leave something behind (otherwise the provider receives a
/// System-only prompt).
fn split_keeps_instruction(messages: &[Message], split_idx: usize) -> bool {
    match messages.iter().rposition(|m| m.role == Role::User) {
        Some(last_user) => split_idx <= last_user,
        None => split_idx < messages.len(),
    }
}

/// Ratio above which L0 snips stale tool outputs: one step below the
/// configured base threshold (L1), never below 10 %.
///
/// L0 used to be hard-wired to 0.60 while L1–L4 followed
/// `compress_threshold`. That made the only *irreversible* level the one level
/// the user could not configure: with `compress_threshold = 0.9` the
/// conversation was still being snipped from 60 % on. Anchoring it to `base`
/// keeps the original intent (L0 fires one step before L1) and keeps the
/// default behaviour identical — `0.8 − 0.20 = 0.60`.
fn l0_threshold(base: f64) -> f64 {
    (base - 0.20).max(0.10)
}

/// Report a level's action when it did something, else L0's snip.
///
/// L0 has no split point of its own and mutates the history in place, so a
/// pass where the configured level refused but L0 fired would otherwise be
/// reported as "nothing happened" while the prompt had in fact shrunk.
fn prefer_level(action: CompressionAction, l0: Option<CompressionAction>) -> CompressionAction {
    if matches!(action, CompressionAction::None) {
        l0.unwrap_or(CompressionAction::None)
    } else {
        action
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::trait_def::{ChatResponse, ProviderError, StreamChunk, ToolDef};
    use std::pin::Pin;

    struct StubProvider;

    #[async_trait::async_trait]
    impl LlmProvider for StubProvider {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDef>,
        ) -> Result<ChatResponse, ProviderError> {
            Ok(ChatResponse {
                content: "compressed summary".into(),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }

        async fn chat_stream(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDef>,
        ) -> Result<
            Pin<Box<dyn futures::stream::Stream<Item = Result<StreamChunk, ProviderError>> + Send>>,
            ProviderError,
        > {
            unimplemented!()
        }

        fn clone_box(&self) -> Box<dyn LlmProvider> {
            panic!("clone_box not used in compression tests")
        }
    }

    fn make_msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: MessageContent::Text(text.to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            created_at: 0,
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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

    // ── apply() threshold dispatch ───────────────────────────────────────

    #[tokio::test]
    async fn test_apply_triggers_l1_when_over_threshold() {
        // window=1000 tokens, threshold=0.8 => L1 fires above 800 tokens.
        let cfg = ContextConfig {
            window_tokens: 1000,
            compress_threshold: 0.8,
            max_agent_iterations: 120,
        };
        let mut pipeline = CompressionPipeline::new(cfg);
        // ASCII is ~4 chars/token: 4 × 800 chars = 800 tokens + 1 for "sys"
        // → ratio ≈ 0.801, inside the L1 band (0.8, 0.85].
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, &"a".repeat(800)),
            make_msg(Role::Assistant, &"b".repeat(800)),
            make_msg(Role::User, &"c".repeat(800)),
            make_msg(Role::Assistant, &"d".repeat(800)),
        ];
        let provider = StubProvider;
        let action = pipeline.apply(&mut msgs, &provider, None).await;
        assert!(
            matches!(action, CompressionAction::Truncated { .. }),
            "expected L1 truncation, got {action:?}"
        );
        assert!(msgs.len() < 5, "oldest non-system message should be dropped");
        // The instruction survives.
        assert_eq!(msgs[1].role, Role::User);
        assert_eq!(msgs[1].content.as_text().unwrap(), "c".repeat(800).as_str());
    }

    #[tokio::test]
    async fn test_apply_respects_custom_compress_threshold() {
        // window=1000 tokens, threshold=0.5 => L1 fires above 500 tokens.
        let cfg = ContextConfig {
            window_tokens: 1000,
            compress_threshold: 0.5,
            max_agent_iterations: 120,
        };
        let mut pipeline = CompressionPipeline::new(cfg);
        // 4 × 520 ASCII chars ≈ 520 tokens + 1 → ratio ≈ 0.521, between
        // L1(0.5) and L2(0.55).
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, &"a".repeat(520)),
            make_msg(Role::Assistant, &"b".repeat(520)),
            make_msg(Role::User, &"c".repeat(520)),
            make_msg(Role::Assistant, &"d".repeat(520)),
        ];
        let provider = StubProvider;
        let action = pipeline.apply(&mut msgs, &provider, None).await;
        assert!(
            matches!(action, CompressionAction::Truncated { .. }),
            "custom threshold 0.5 should trigger L1, got {action:?}"
        );
    }

    /// The live instruction (the last user message and everything after it)
    /// must survive every level. A short conversation where the 50 % split
    /// would delete the only user message must refuse instead — this used to
    /// produce a prompt with no user turn, which the model "continued" by
    /// hallucinating and forge reported as a successful Complete.
    #[tokio::test]
    async fn apply_refuses_to_drop_last_user_message() {
        let cfg = ContextConfig {
            window_tokens: 1000,
            compress_threshold: 0.8,
            max_agent_iterations: 120,
        };
        let mut pipeline = CompressionPipeline::new(cfg);
        // ratio ≈ 0.801 → L1 band, but dropping 50 % of [u, a] would drop u.
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, &"a".repeat(1600)),
            make_msg(Role::Assistant, &"b".repeat(1600)),
        ];
        let before = msgs.clone();
        let provider = StubProvider;
        let action = pipeline.apply(&mut msgs, &provider, None).await;
        assert_eq!(action, CompressionAction::None, "must refuse, got {action:?}");
        assert_eq!(msgs.len(), before.len());
        assert!(msgs.iter().any(|m| m.role == Role::User));
    }

    /// L3 used to walk its split point to the end of a tool-only tail, leaving
    /// `rest` empty and sending the provider a single System message (Anthropic
    /// rejects `messages: []` with a 400).
    #[tokio::test]
    async fn l3_never_emits_a_system_only_history() {
        let cfg = ContextConfig {
            window_tokens: 1000,
            compress_threshold: 0.8,
            max_agent_iterations: 120,
        };
        let mut pipeline = CompressionPipeline::new(cfg);
        // ratio ≈ 0.93 → L3 band; the tail after the only user message is an
        // assistant tool_call + its result, so the 70 % alignment walks to the
        // end of the vector.
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, &"q".repeat(300)),
            make_assistant_with_tool_calls("", &["tc1"]),
            make_tool("tc1", &"r".repeat(3400)),
        ];
        let provider = StubProvider;
        let action = pipeline.apply(&mut msgs, &provider, None).await;
        assert!(
            msgs.iter().any(|m| m.role == Role::User),
            "the user message must survive, got {action:?}"
        );
        assert!(
            msgs.iter().any(|m| m.role == Role::Tool),
            "tool output must not be silently discarded by a refused pass"
        );
        assert!(
            !msgs.iter().any(|m| {
                m.content
                    .as_text()
                    .map_or(false, |s| s.contains("compressed summary"))
            }),
            "LLM summary must not replace the whole history"
        );
    }

    /// L4 clamps its split point back to the last user message instead of
    /// running off the end of a tool-only tail.
    #[tokio::test]
    async fn l4_keeps_the_last_user_message() {
        let cfg = ContextConfig {
            window_tokens: 1000,
            compress_threshold: 0.8,
            max_agent_iterations: 120,
        };
        let mut pipeline = CompressionPipeline::new(cfg);
        // ratio ≈ 0.967 → L4 band.
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, "old question"),
            make_assistant_with_tool_calls("", &["t1"]),
            make_tool("t1", "old output"),
            make_msg(Role::User, "current question"),
            make_assistant_with_tool_calls("", &["t2"]),
            make_tool("t2", &"r".repeat(3800)),
        ];
        let provider = StubProvider;
        let action = pipeline.apply(&mut msgs, &provider, None).await;
        assert!(matches!(action, CompressionAction::Chunked { .. }), "got {action:?}");
        assert!(
            msgs.iter().any(|m| m.content.as_text() == Some("current question")),
            "the live user instruction must survive L4"
        );
        assert!(
            !msgs.iter().any(|m| m.content.as_text() == Some("old question")),
            "older turns should be the ones dropped"
        );
        assert!(msgs[0].content.as_text().unwrap().contains("[CONTINUATION]"));
    }

    /// Repeat pass in the same turn: L0/L1 only, never another LLM summary.
    #[tokio::test]
    async fn cheap_pass_skips_llm_summarisation() {
        let cfg = ContextConfig {
            window_tokens: 1000,
            compress_threshold: 0.8,
            max_agent_iterations: 120,
        };
        let mut pipeline = CompressionPipeline::new(cfg);
        pipeline.cheap_only = true;
        // 4 × 900 ASCII chars ≈ 900 tokens + 1 → ratio ≈ 0.901, which is the
        // L3 band for a full pass (and would call the summarisation model).
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, &"a".repeat(900)),
            make_msg(Role::Assistant, &"b".repeat(900)),
            make_msg(Role::User, &"c".repeat(900)),
            make_msg(Role::Assistant, &"d".repeat(900)),
        ];
        let provider = StubProvider;
        let action = pipeline.apply(&mut msgs, &provider, None).await;
        assert!(
            matches!(action, CompressionAction::Truncated { .. }),
            "cheap pass should use L1, got {action:?}"
        );
        assert!(
            !msgs.iter().any(|m| {
                m.content
                    .as_text()
                    .map_or(false, |s| s.contains("compressed summary"))
            }),
            "cheap pass must not call the summarisation model"
        );
    }

    /// Tool definitions are sent on every request but are not part of
    /// `messages`; a large MCP tool set must count towards the window, because
    /// it can be the difference between "under budget" and a provider 400.
    #[tokio::test]
    async fn tool_definitions_count_towards_the_window() {
        let cfg = ContextConfig {
            window_tokens: 1000,
            compress_threshold: 0.8,
            max_agent_iterations: 120,
        };
        let messages = || {
            vec![
                make_msg(Role::System, "sys"),
                make_msg(Role::User, &"a".repeat(800)),
                make_msg(Role::Assistant, &"b".repeat(800)),
                make_msg(Role::User, &"c".repeat(800)),
                make_msg(Role::Assistant, &"d".repeat(800)),
            ]
        };
        // ~3200 chars of schema ≈ 800 tokens on top of the ~801 message tokens.
        let big_tool = ToolDef::new(
            "mcp_big_search",
            &"d".repeat(3200),
            serde_json::json!({ "type": "object" }),
        );

        let provider = StubProvider;
        let mut without = messages();
        let mut pipeline_without = CompressionPipeline::new(cfg.clone());
        let action_without = pipeline_without.apply(&mut without, &provider, None).await;

        let mut with = messages();
        let mut pipeline_with = CompressionPipeline::new(cfg).with_tools(&[big_tool]);
        assert!(pipeline_with.tool_tokens > 700, "tool tokens must be non-trivial");
        let action_with = pipeline_with.apply(&mut with, &provider, None).await;

        // Ignoring the tool defs the history sits just over the L1 threshold;
        // counting them pushes the same history into the L4 band.
        assert!(
            matches!(action_without, CompressionAction::Truncated { .. }),
            "without tools: expected L1, got {action_without:?}"
        );
        assert!(
            matches!(action_with, CompressionAction::Chunked { .. }),
            "with tools: expected L4, got {action_with:?}"
        );
    }

    #[tokio::test]
    async fn test_apply_noop_below_threshold() {
        let cfg = ContextConfig {
            window_tokens: 100_000,
            compress_threshold: 0.8,
            max_agent_iterations: 120,
        };
        let mut pipeline = CompressionPipeline::new(cfg);
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, "hello"),
            make_msg(Role::Assistant, "hi"),
        ];
        let provider = StubProvider;
        let action = pipeline.apply(&mut msgs, &provider, None).await;
        assert_eq!(action, CompressionAction::None);
        assert_eq!(msgs.len(), 3);
    }

    // ── L0 threshold follows the configured base ─────────────────────────

    fn cfg_with(threshold: f64) -> ContextConfig {
        ContextConfig {
            window_tokens: 1000,
            compress_threshold: threshold,
            max_agent_iterations: 120,
        }
    }

    /// L0 is one step below the configured threshold, not a fixed 60 %.
    #[test]
    fn l0_threshold_follows_the_configured_base() {
        // Default base → the legacy constant, so nothing changes by default.
        assert!((l0_threshold(0.8) - 0.60).abs() < 1e-9, "got {}", l0_threshold(0.8));
        assert!((l0_threshold(0.5) - 0.30).abs() < 1e-9, "got {}", l0_threshold(0.5));
        assert!((l0_threshold(0.85) - 0.65).abs() < 1e-9, "got {}", l0_threshold(0.85));
        // Clamped base (0.1) must not push L0 to zero or below.
        assert!((l0_threshold(0.1) - 0.10).abs() < 1e-9, "got {}", l0_threshold(0.1));
    }

    /// A user who raises `compress_threshold` must also move the *irreversible*
    /// level: at base 0.5 this history is sniped, at base 0.8 it is untouched.
    #[tokio::test]
    async fn l0_fires_at_the_configured_threshold() {
        // window = 1000 tokens; "sys"(1) + "ask"(1) + 6 × 300 ASCII chars (75)
        // = 452 tokens → ratio 0.452. Above L0 at base 0.5 (0.30), below L1
        // (0.50); above neither at base 0.8 (L0 = 0.60).
        let history = || {
            let mut msgs = vec![
                make_msg(Role::System, "sys"),
                make_msg(Role::User, "ask"),
            ];
            for i in 0..6 {
                msgs.push(make_tool(&format!("tc{i}"), &"x".repeat(300)));
            }
            msgs
        };
        let provider = StubProvider;

        let mut at_half = history();
        let mut pipeline = CompressionPipeline::new(cfg_with(0.5));
        let action = pipeline.apply(&mut at_half, &provider, None).await;
        assert!(
            matches!(action, CompressionAction::Snipped { snipped: 3 }),
            "base 0.5 must snip the 3 oldest tool results, got {action:?}"
        );
        assert!(
            at_half[2].content.as_text().unwrap().contains("snipped"),
            "the oldest tool output must carry the re-run placeholder"
        );
        assert_eq!(
            at_half[5].content.as_text().unwrap().len(),
            300,
            "the 3 most recent tool results must survive L0"
        );

        let mut at_default = history();
        let mut pipeline = CompressionPipeline::new(cfg_with(0.8));
        let action = pipeline.apply(&mut at_default, &provider, None).await;
        assert_eq!(
            action,
            CompressionAction::None,
            "base 0.8 keeps L0 at 0.60 — this history is below it"
        );
        assert!(
            at_default[2..].iter().all(|m| m.content.as_text().unwrap().len() == 300),
            "no tool output may be sniped below the L0 threshold"
        );
    }

    // ── force_apply ──────────────────────────────────────────────────────

    /// The provider told us we are over the limit: the deepest level runs even
    /// though `cheap_only` is set (a repeat pass in the same turn), because a
    /// dead turn costs more than a summarisation.
    #[tokio::test]
    async fn force_apply_ignores_cheap_only() {
        let mut pipeline = CompressionPipeline::new(cfg_with(0.8));
        pipeline.cheap_only = true;
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, "old question"),
            make_msg(Role::Assistant, "old answer"),
            make_msg(Role::User, "second question"),
            make_msg(Role::Assistant, "second answer"),
            make_msg(Role::User, "current question"),
            make_msg(Role::Assistant, "current answer"),
        ];
        let provider = StubProvider;
        let action = pipeline.force_apply(&mut msgs, &provider).await;
        assert!(
            matches!(action, CompressionAction::LlmSummarized { .. }),
            "cheap_only must not stop a forced deep compression, got {action:?}"
        );
        assert!(
            msgs.iter().any(|m| {
                m.content
                    .as_text()
                    .map_or(false, |s| s.contains("compressed summary"))
            }),
            "the L3 summary must have been written into the history"
        );
        assert!(
            !msgs.iter().any(|m| m.content.as_text() == Some("old question")),
            "the oldest messages are what got summarised"
        );
    }

    /// Forcing compression still may not cut the live instruction: the last
    /// user message and everything after it survive.
    #[tokio::test]
    async fn force_apply_keeps_the_last_user_message_and_its_tail() {
        let mut pipeline = CompressionPipeline::new(cfg_with(0.8));
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, "old question"),
            make_msg(Role::Assistant, "old answer"),
            make_msg(Role::User, "second question"),
            make_msg(Role::Assistant, "second answer"),
            make_msg(Role::User, "current question"),
            make_msg(Role::Assistant, "current answer"),
        ];
        let provider = StubProvider;
        let action = pipeline.force_apply(&mut msgs, &provider).await;
        assert!(matches!(action, CompressionAction::LlmSummarized { .. }), "got {action:?}");
        assert!(
            msgs.iter().any(|m| m.content.as_text() == Some("current question")),
            "the live user instruction must survive a forced compression"
        );
        assert!(
            msgs.iter().any(|m| m.content.as_text() == Some("current answer")),
            "everything after the live instruction must survive too"
        );
    }

    /// When every level refuses — the whole tail after the only user message is
    /// tool output — `force_apply` must return `None` and leave the history
    /// byte-identical rather than send a prompt without its instruction.
    #[tokio::test]
    async fn force_apply_refuses_when_nothing_can_be_cut_safely() {
        let mut pipeline = CompressionPipeline::new(cfg_with(0.8));
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, "run it"),
            make_assistant_with_tool_calls("", &["tc1"]),
            make_tool("tc1", &"r".repeat(1500)),
        ];
        let before = msgs.clone();
        let provider = StubProvider;
        let action = pipeline.force_apply(&mut msgs, &provider).await;
        assert_eq!(
            action,
            CompressionAction::None,
            "no level may cut past the live instruction, got {action:?}"
        );
        assert_eq!(msgs.len(), before.len());
        assert!(
            msgs.iter().any(|m| m.role == Role::User),
            "the instruction must still be in the history"
        );
        assert!(
            msgs.iter().any(|m| m.role == Role::Tool),
            "the tool result must not be silently discarded"
        );
    }

    /// Forced compression on an already-minimal history is a no-op, not an
    /// error: the caller decides what "nothing to cut" means.
    #[tokio::test]
    async fn force_apply_on_a_short_history_is_a_noop() {
        let mut pipeline = CompressionPipeline::new(cfg_with(0.8));
        let mut msgs = vec![
            make_msg(Role::System, "sys"),
            make_msg(Role::User, "hello"),
        ];
        let before = msgs.clone();
        let provider = StubProvider;
        let action = pipeline.force_apply(&mut msgs, &provider).await;
        assert_eq!(action, CompressionAction::None);
        assert_eq!(msgs.len(), before.len());
    }
}
