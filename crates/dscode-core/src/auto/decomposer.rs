//! Task decomposition — breaks a high-level PRD into subtasks.

// use async_trait::async_trait; — not needed yet
// use serde::{Deserialize, Serialize}; — not needed yet

use crate::providers::trait_def::{LlmProvider, Message, MessageContent, Role};
use super::runner::{AutoError, Subtask, SubtaskStatus};

/// Decompose a high-level PRD description into a flat list of subtasks.
///
/// Uses the cheap `runtime_provider` (e.g. a fast model) to produce a simple
/// numbered list of subtasks, then parses them into [`Subtask`] structs.
pub async fn decompose_task(
    provider: &dyn LlmProvider,
    prd: &str,
) -> Result<Vec<Subtask>, AutoError> {
    let prompt = format!(
        "Break down the following task into a numbered list of subtasks. \
         Each subtask should be a single line starting with a number and a period. \
         Do NOT include dependencies — just list the subtasks in the order they should be executed.\n\n\
         Task:\n{}",
        prd
    );

    let messages = vec![
        Message {
            role: Role::System,
            content: MessageContent::Text(
                "You are a task decomposition assistant. Output only a numbered list of subtasks, \
                 one per line. Do not add commentary.".into(),
            ),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None, created_at: 0,
        },
        Message {
            role: Role::User,
            content: MessageContent::Text(prompt),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None, created_at: 0,
        },
    ];

    let response = provider.chat(messages, vec![]).await?;

    // Parse the numbered list from the response.
    let subtasks: Vec<Subtask> = response
        .content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            // Match lines like "1. Do something" or "1) Do something"
            let description = line
                .trim_start_matches(|c: char| c.is_numeric() || c == '.' || c == ')' || c == ' ')
                .trim()
                .to_string();
            if description.is_empty() {
                None
            } else {
                Some(description)
            }
        })
        .enumerate()
        .map(|(i, description)| Subtask {
            id: i + 1,
            description,
            dependencies: vec![],
            status: SubtaskStatus::Pending,
        })
        .collect();

    if subtasks.is_empty() {
        // Fallback: treat the entire response as one subtask.
        let description = response.content.trim().to_string();
        if description.is_empty() {
            return Ok(vec![]);
        }
        Ok(vec![Subtask {
            id: 1,
            description,
            dependencies: vec![],
            status: SubtaskStatus::Pending,
        }])
    } else {
        Ok(subtasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_numbered_list() {
        // Simulate the parsing logic directly.
        let input = "1. Install dependencies\n2. Build the project\n3. Run tests";
        let subtasks: Vec<String> = input
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                let description = line
                    .trim_start_matches(|c: char| c.is_numeric() || c == '.' || c == ')' || c == ' ')
                    .trim()
                    .to_string();
                if description.is_empty() {
                    None
                } else {
                    Some(description)
                }
            })
            .collect();

        assert_eq!(subtasks.len(), 3);
        assert_eq!(subtasks[0], "Install dependencies");
        assert_eq!(subtasks[1], "Build the project");
        assert_eq!(subtasks[2], "Run tests");
    }
}
