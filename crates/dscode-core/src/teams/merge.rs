//! Merge / final report helpers for teams v2.

use super::board::{truncate_excerpt, TaskSpec, TaskStatus};

const PER_TASK_EXCERPT: usize = 4096;
const TOTAL_MERGE_CAP: usize = 48 * 1024;

/// Build results block for Lead merge LLM (capped).
pub fn build_results_block(tasks: &[&TaskSpec]) -> String {
    let mut out = String::new();
    for t in tasks {
        let status = match t.status {
            TaskStatus::Done => "OK",
            TaskStatus::Failed => "FAIL",
            TaskStatus::Cancelled => "CANCEL",
            TaskStatus::Blocked => "BLOCKED",
            TaskStatus::Running => "RUNNING",
            TaskStatus::Pending => "PENDING",
        };
        let excerpt = t
            .result_excerpt
            .as_deref()
            .or(t.result_summary.as_deref())
            .unwrap_or("(no output)");
        let excerpt = truncate_excerpt(excerpt, PER_TASK_EXCERPT);
        out.push_str(&format!(
            "### {} [{status}] role={:?}\n**Title:** {}\n**Task:** {}\n\n{excerpt}\n\n",
            t.id, t.role, t.title, t.prompt
        ));
        if out.len() > TOTAL_MERGE_CAP {
            out.push_str("\n…(truncated)\n");
            break;
        }
    }
    out
}

/// Fallback when merge LLM fails.
pub fn raw_concat_report(user_task: &str, plan: &str, tasks: &[&TaskSpec]) -> String {
    format!(
        "### Main agent summary (fallback)\n\n**Task:** {user_task}\n\n**Plan:** {plan}\n\n{}",
        build_results_block(tasks)
    )
}

pub fn merge_prompt(user_task: &str, plan: &str, results_block: &str) -> String {
    format!(
        "You are the MAIN agent of a multi-agent coding team. Sub-agents have finished.\n\
         Original user task:\n{user_task}\n\n\
         Your plan was:\n{plan}\n\n\
         Sub-agent results:\n{results_block}\n\
         Write a clear final report for the user in markdown:\n\
         1. What was accomplished overall\n\
         2. Per-agent outcomes (brief)\n\
         3. Key files produced / changes\n\
         4. Remaining risks or follow-ups\n\
         Be concrete. Do not invent files that were not mentioned."
    )
}

/// Synthesize prompt after research tasks complete.
pub fn synthesize_prompt(user_task: &str, research_block: &str) -> String {
    format!(
        "You are the MAIN coordinator. Research sub-agents finished.\n\
         User task:\n{user_task}\n\n\
         Research results:\n{research_block}\n\n\
         Produce concrete IMPLEMENT tasks as JSON only:\n\
         {{\n\
           \"version\": 1,\n\
           \"plan\": \"...\",\n\
           \"tasks\": [\n\
             {{\n\
               \"id\": \"impl1\",\n\
               \"title\": \"...\",\n\
               \"prompt\": \"MUST include exact file paths, what to change, and acceptance criteria. NEVER say based on your findings.\",\n\
               \"role\": \"implement\",\n\
               \"dependencies\": [\"research-task-id\"],\n\
               \"owned_paths\": [\"src/...\"],\n\
               \"standalone\": false\n\
             }}\n\
           ]\n\
         }}\n\
         Rules: each implement task must depend on relevant research ids OR standalone=true.\n\
         Prefer 1–4 implement tasks. owned_paths relative, no .."
    )
}

pub fn decompose_prompt(
    user_task: &str,
    context_summary: &str,
    working_dir: &str,
    max_agents: usize,
    multi_wave: bool,
    skip_research: bool,
) -> String {
    if multi_wave && !skip_research {
        format!(
            "You are the MAIN coordinator of a multi-agent coding team.\n\
             Decompose into RESEARCH (explore-only) subtasks first.\n\
             Working dir: {working_dir}\n\
             Context:\n{context_summary}\n\
             Task: {user_task}\n\n\
             Output STRICT JSON only:\n\
             {{\n\
               \"version\": 1,\n\
               \"plan\": \"1-3 sentences\",\n\
               \"skip_research\": false,\n\
               \"tasks\": [\n\
                 {{\"id\": \"r1\", \"title\": \"...\", \"prompt\": \"self-contained research brief\", \"role\": \"explore\", \"dependencies\": [], \"owned_paths\": []}}\n\
               ]\n\
             }}\n\
             Rules: prefer 2–4 explore tasks, max {max_agents}. role MUST be explore. Independent when possible."
        )
    } else {
        format!(
            "You are the MAIN coordinator of a multi-agent coding team.\n\
             Decompose into independent IMPLEMENT subtasks.\n\
             Working dir: {working_dir}\n\
             Context:\n{context_summary}\n\
             Task: {user_task}\n\n\
             Output STRICT JSON only:\n\
             {{\n\
               \"version\": 1,\n\
               \"plan\": \"1-3 sentences\",\n\
               \"skip_research\": true,\n\
               \"tasks\": [\n\
                 {{\"id\": \"t1\", \"title\": \"...\", \"prompt\": \"self-contained implement brief with paths\", \"role\": \"implement\", \"dependencies\": [], \"owned_paths\": [\"optional/rel/path\"]}}\n\
               ]\n\
             }}\n\
             Rules: prefer 2–6 tasks, max {max_agents}. Only parallel-safe splits. role=implement."
        )
    }
}
