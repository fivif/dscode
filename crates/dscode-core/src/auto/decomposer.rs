//! Task decomposition — breaks a high-level PRD into subtasks with optional DAG deps.

use crate::providers::trait_def::{LlmProvider, Message, MessageContent, Role};
use super::runner::{AutoError, Subtask, SubtaskStatus};

/// Decompose a high-level PRD into subtasks, optionally with dependencies.
///
/// Uses the cheap `runtime_provider` to produce a structured list. Dependencies
/// are supported via `deps: [ids]` lines or `depends_on: N` suffixes.
pub async fn decompose_task(
    provider: &dyn LlmProvider,
    prd: &str,
) -> Result<Vec<Subtask>, AutoError> {
    let prompt = format!(
        "Break down the following task into a numbered list of subtasks.\n\
         Each line: `N. description` optionally followed by `deps: A,B` where A,B are other numbers.\n\
         Example:\n\
         1. Create user model\n\
         2. Add migration deps: 1\n\
         3. Wire API deps: 1,2\n\
         Prefer independent tasks when possible. Max 12 subtasks.\n\
         Output ONLY the numbered list, no commentary.\n\n\
         Task:\n{prd}"
    );

    let messages = vec![
        Message {
            role: Role::System,
            content: MessageContent::Text(
                "You are a task decomposition assistant. Output only a numbered list. \
                 Use deps: when a task must wait on earlier numbers."
                    .into(),
            ),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            created_at: 0,
        },
        Message {
            role: Role::User,
            content: MessageContent::Text(prompt),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            created_at: 0,
        },
    ];

    let response = provider.chat(messages, vec![]).await?;
    parse_subtasks(&response.content)
}

/// Parse numbered lines into Subtask list with deps.
pub fn parse_subtasks(content: &str) -> Result<Vec<Subtask>, AutoError> {
    let lines: Vec<&str> = content.lines().collect();
    let start_idx = lines.iter().position(|line| {
        let trimmed = line.trim();
        let chars: Vec<char> = trimmed.chars().take(3).collect();
        !chars.is_empty()
            && chars[0].is_numeric()
            && chars.get(1).map_or(false, |c| *c == '.' || *c == ')')
    });

    let parse_from = start_idx.unwrap_or(0);
    let mut raw: Vec<(usize, String, Vec<usize>)> = Vec::new();

    for line in &lines[parse_from..] {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let chars: Vec<char> = line.chars().take(3).collect();
        if chars.len() < 2
            || !chars[0].is_numeric()
            || !(chars[1] == '.' || chars[1] == ')')
        {
            // allow multi-digit: "10. foo"
            if let Some((num, rest)) = split_numbered(line) {
                let (desc, deps) = split_deps(rest);
                if !desc.is_empty() {
                    raw.push((num, desc, deps));
                }
            }
            continue;
        }
        if let Some((num, rest)) = split_numbered(line) {
            let (desc, deps) = split_deps(rest);
            if !desc.is_empty() {
                raw.push((num, desc, deps));
            }
        }
    }

    if raw.is_empty() {
        return Err(AutoError::Parse(
            "no numbered subtasks in decomposer output".into(),
        ));
    }

    // Remap to sequential ids 1..n if model used sparse numbers
    let id_map: std::collections::HashMap<usize, usize> = raw
        .iter()
        .enumerate()
        .map(|(i, (n, _, _))| (*n, i + 1))
        .collect();

    let mut subtasks: Vec<Subtask> = raw
        .into_iter()
        .enumerate()
        .map(|(i, (_orig, description, deps))| {
            let dependencies: Vec<usize> = deps
                .into_iter()
                .filter_map(|d| id_map.get(&d).copied())
                .filter(|&mapped| mapped != i + 1)
                .collect();
            Subtask {
                id: i + 1,
                description,
                dependencies,
                status: SubtaskStatus::Pending,
            }
        })
        .collect();

    // Break cycles: drop deps that form cycles (simple: drop any dep >= self id if cycle detected later)
    if has_cycle(&subtasks) {
        tracing::warn!("decomposer produced cyclic deps — clearing all dependencies");
        for s in &mut subtasks {
            s.dependencies.clear();
        }
    }

    Ok(subtasks)
}

fn split_numbered(line: &str) -> Option<(usize, &str)> {
    let line = line.trim();
    let mut num_end = 0;
    for (i, c) in line.char_indices() {
        if c.is_ascii_digit() {
            num_end = i + 1;
        } else {
            break;
        }
    }
    if num_end == 0 {
        return None;
    }
    let num: usize = line[..num_end].parse().ok()?;
    let rest = line[num_end..].trim_start();
    let rest = rest
        .strip_prefix('.')
        .or_else(|| rest.strip_prefix(')'))
        .unwrap_or(rest)
        .trim_start();
    Some((num, rest))
}

fn split_deps(rest: &str) -> (String, Vec<usize>) {
    // "desc deps: 1,2" or "desc depends_on: 1"
    let lower = rest.to_ascii_lowercase();
    if let Some(idx) = lower.find("deps:") {
        let desc = rest[..idx].trim().to_string();
        let dep_str = rest[idx + 5..].trim();
        let deps = parse_dep_list(dep_str);
        return (desc, deps);
    }
    if let Some(idx) = lower.find("depends_on:") {
        let desc = rest[..idx].trim().to_string();
        let dep_str = rest[idx + 11..].trim();
        let deps = parse_dep_list(dep_str);
        return (desc, deps);
    }
    (rest.trim().to_string(), vec![])
}

fn parse_dep_list(s: &str) -> Vec<usize> {
    s.split(|c: char| c == ',' || c == ' ' || c == ';')
        .filter_map(|p| p.trim().parse().ok())
        .collect()
}

fn has_cycle(subtasks: &[Subtask]) -> bool {
    use std::collections::{HashMap, HashSet, VecDeque};
    let mut indeg: HashMap<usize, usize> = HashMap::new();
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    for s in subtasks {
        indeg.entry(s.id).or_insert(0);
        for &d in &s.dependencies {
            adj.entry(d).or_default().push(s.id);
            *indeg.entry(s.id).or_insert(0) += 1;
        }
    }
    let mut q: VecDeque<usize> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&k, _)| k)
        .collect();
    let mut seen = 0;
    while let Some(id) = q.pop_front() {
        seen += 1;
        if let Some(ch) = adj.get(&id) {
            for &c in ch {
                if let Some(d) = indeg.get_mut(&c) {
                    *d -= 1;
                    if *d == 0 {
                        q.push_back(c);
                    }
                }
            }
        }
    }
    seen != subtasks.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_deps() {
        let text = "\
1. Create model
2. Migration deps: 1
3. API deps: 1, 2
";
        let tasks = parse_subtasks(text).unwrap();
        assert_eq!(tasks.len(), 3);
        assert!(tasks[0].dependencies.is_empty());
        assert_eq!(tasks[1].dependencies, vec![1]);
        assert_eq!(tasks[2].dependencies, vec![1, 2]);
    }

    #[test]
    fn cycle_cleared() {
        // 1 deps 2, 2 deps 1 — after remap still cycle
        let text = "\
1. A deps: 2
2. B deps: 1
";
        let tasks = parse_subtasks(text).unwrap();
        assert!(tasks.iter().all(|t| t.dependencies.is_empty()));
    }
}
