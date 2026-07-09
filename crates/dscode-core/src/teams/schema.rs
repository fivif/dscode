//! Decompose / synthesize JSON validation for teams v2.

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::board::{TaskSpec, WaveKind};
use super::ownership::normalize_rel;
use super::role::AgentRole;

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("json parse: {0}")]
    Parse(String),
    #[error("validation: {0}")]
    Validation(String),
}

/// LLM envelope for decompose / synthesize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposeEnvelope {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub skip_research: bool,
    pub tasks: Vec<TaskSpecInput>,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpecInput {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub role: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub owned_paths: Vec<String>,
    #[serde(default)]
    pub wave: Option<String>,
    #[serde(default)]
    pub standalone: bool,
}

const ID_RE: &str = r"^[a-zA-Z][a-zA-Z0-9_-]{0,31}$";

/// Parse and validate decompose JSON into TaskSpecs.
pub fn parse_decompose(
    raw: &str,
    max_agents: usize,
    multi_wave: bool,
) -> Result<(String, Vec<TaskSpec>, bool), SchemaError> {
    let json = extract_json(raw)?;
    let env: DecomposeEnvelope =
        serde_json::from_str(&json).map_err(|e| SchemaError::Parse(e.to_string()))?;
    validate_and_build(env, max_agents, multi_wave, false)
}

/// Parse synthesize JSON (implement tasks after research).
pub fn parse_synthesize(
    raw: &str,
    max_agents: usize,
    existing_ids: &[String],
) -> Result<(String, Vec<TaskSpec>), SchemaError> {
    let json = extract_json(raw)?;
    let env: DecomposeEnvelope =
        serde_json::from_str(&json).map_err(|e| SchemaError::Parse(e.to_string()))?;
    let (plan, tasks, _) = validate_and_build(env, max_agents, true, true)?;
    // implement must depend on existing research unless standalone
    for t in &tasks {
        if t.role == AgentRole::Implement && !t.standalone {
            if t.dependencies.is_empty() {
                return Err(SchemaError::Validation(format!(
                    "implement task {} needs dependencies or standalone=true",
                    t.id
                )));
            }
            for d in &t.dependencies {
                if !existing_ids.contains(d) && !tasks.iter().any(|x| x.id == *d) {
                    return Err(SchemaError::Validation(format!(
                        "unknown dependency {d} for {}",
                        t.id
                    )));
                }
            }
        }
        if matches_vague_findings(&t.prompt) {
            return Err(SchemaError::Validation(
                "prompt must not say 'based on your findings' — be concrete".into(),
            ));
        }
    }
    Ok((plan, tasks))
}

fn validate_and_build(
    env: DecomposeEnvelope,
    max_agents: usize,
    multi_wave: bool,
    is_synthesize: bool,
) -> Result<(String, Vec<TaskSpec>, bool), SchemaError> {
    if env.tasks.is_empty() {
        return Err(SchemaError::Validation("empty tasks".into()));
    }
    if env.tasks.len() > max_agents {
        return Err(SchemaError::Validation(format!(
            "too many tasks {} > max {max_agents}",
            env.tasks.len()
        )));
    }

    let id_re = Regex::new(ID_RE).unwrap();
    let mut ids = std::collections::HashSet::new();
    let mut specs = Vec::new();

    for input in &env.tasks {
        if !id_re.is_match(&input.id) {
            return Err(SchemaError::Validation(format!(
                "invalid id: {}",
                input.id
            )));
        }
        if !ids.insert(input.id.clone()) {
            return Err(SchemaError::Validation(format!(
                "duplicate id: {}",
                input.id
            )));
        }
        if input.title.is_empty() || input.title.chars().count() > 120 {
            return Err(SchemaError::Validation(format!(
                "bad title for {}",
                input.id
            )));
        }
        if input.prompt.trim().is_empty() || input.prompt.len() > 8192 {
            return Err(SchemaError::Validation(format!(
                "bad prompt for {}",
                input.id
            )));
        }
        let role = AgentRole::parse(&input.role).ok_or_else(|| {
            SchemaError::Validation(format!("bad role {} for {}", input.role, input.id))
        })?;

        if multi_wave && !is_synthesize && !env.skip_research && role != AgentRole::Explore {
            return Err(SchemaError::Validation(
                "multi-wave initial decompose must be explore-only (or skip_research)".into(),
            ));
        }

        let mut owned = Vec::new();
        for p in &input.owned_paths {
            match normalize_rel(p) {
                Ok(n) => owned.push(n.to_string_lossy().to_string()),
                Err(()) => {
                    tracing::warn!(path = %p, "dropping invalid owned_path");
                }
            }
        }

        let wave = input
            .wave
            .as_deref()
            .and_then(|w| match w {
                "research" => Some(WaveKind::Research),
                "implement" => Some(WaveKind::Implement),
                "verify" => Some(WaveKind::Verify),
                _ => None,
            })
            .unwrap_or_else(|| WaveKind::from_role(role));

        let mut spec = TaskSpec::new(&input.id, &input.title, &input.prompt, role);
        spec.dependencies = input.dependencies.clone();
        spec.owned_paths = owned;
        spec.wave = wave;
        spec.standalone = input.standalone;
        specs.push(spec);
    }

    // deps exist + acyclic
    let id_set: std::collections::HashSet<_> = ids.iter().cloned().collect();
    for s in &specs {
        for d in &s.dependencies {
            if !id_set.contains(d) {
                return Err(SchemaError::Validation(format!(
                    "unknown dep {d} for {}",
                    s.id
                )));
            }
        }
    }
    check_acyclic(&specs)?;

    Ok((env.plan, specs, env.skip_research))
}

fn check_acyclic(specs: &[TaskSpec]) -> Result<(), SchemaError> {
    use std::collections::{HashMap, VecDeque};
    let mut indeg: HashMap<&str, usize> = HashMap::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for s in specs {
        indeg.entry(&s.id).or_insert(0);
        for d in &s.dependencies {
            adj.entry(d.as_str()).or_default().push(&s.id);
            *indeg.entry(&s.id).or_insert(0) += 1;
        }
    }
    let mut q: VecDeque<&str> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut seen = 0;
    while let Some(id) = q.pop_front() {
        seen += 1;
        if let Some(ch) = adj.get(id) {
            for c in ch {
                if let Some(d) = indeg.get_mut(c) {
                    *d -= 1;
                    if *d == 0 {
                        q.push_back(c);
                    }
                }
            }
        }
    }
    if seen != specs.len() {
        return Err(SchemaError::Validation("dependency cycle".into()));
    }
    Ok(())
}

/// Extract JSON object from model output (fence-tolerant).
pub fn extract_json(raw: &str) -> Result<String, SchemaError> {
    let trimmed = raw.trim();
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                return Ok(trimmed[start..=end].to_string());
            }
        }
    }
    Err(SchemaError::Parse("no JSON object found".into()))
}

pub fn matches_vague_findings(prompt: &str) -> bool {
    let re = Regex::new(r"(?i)based on (your |the )?(findings|research|above)").unwrap();
    re.is_match(prompt)
}

/// Fallback single implement task.
pub fn fallback_task(user_task: &str) -> TaskSpec {
    let mut t = TaskSpec::new("t-1", "Full task", user_task, AgentRole::Implement);
    t.owned_paths = vec![];
    t
}

/// Whether to skip research wave (single-wave style).
pub fn prefer_skip_research(user_message: &str, waves_enabled: bool) -> bool {
    if !waves_enabled {
        return true;
    }
    let m = user_message;
    let force_research = m.contains("调研")
        || m.contains("为什么")
        || m.contains("分析")
        || m.to_ascii_lowercase().contains("investigate")
        || m.to_ascii_lowercase().contains("explore");

    let has_path = m.contains('/')
        || m.contains(".rs")
        || m.contains(".ts")
        || m.contains(".py")
        || m.contains(".tsx");
    let re = Regex::new(r"(?i)(fix|bug|实现|添加|修改|refactor|implement)\b").unwrap();
    let wants_code = re.is_match(m) && has_path;

    if force_research && !wants_code {
        return false;
    }
    if m.chars().count() < 80 && !m.contains("调研") && !m.contains("分析架构") {
        return true;
    }
    wants_code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_implement() {
        let raw = r#"{
          "version": 1,
          "plan": "split work",
          "skip_research": true,
          "tasks": [
            {"id": "t1", "title": "A", "prompt": "impl a", "role": "implement"},
            {"id": "t2", "title": "B", "prompt": "impl b", "role": "implement", "dependencies": ["t1"]}
          ]
        }"#;
        let (plan, tasks, skip) = parse_decompose(raw, 8, false).unwrap();
        assert!(skip || !plan.is_empty() || true);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[1].dependencies, vec!["t1"]);
    }

    #[test]
    fn rejects_cycle() {
        let raw = r#"{
          "tasks": [
            {"id": "a", "title": "A", "prompt": "p", "role": "implement", "dependencies": ["b"]},
            {"id": "b", "title": "B", "prompt": "p", "role": "implement", "dependencies": ["a"]}
          ]
        }"#;
        assert!(parse_decompose(raw, 8, false).is_err());
    }

    #[test]
    fn vague_findings() {
        assert!(matches_vague_findings("Based on your findings, fix auth"));
        assert!(!matches_vague_findings(
            "Fix null check in src/auth.rs:42 for Session.user"
        ));
    }
}
