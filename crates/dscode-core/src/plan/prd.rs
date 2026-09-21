//! PRD (Plan Requirements Document) generation.
//!
//! After the interview is complete, the plan engine produces a structured PRD
//! that captures the goals, success criteria, architecture decisions, files to
//! modify, and a time estimate. The PRD is persisted to
//! `~/.dscode/tasks/<task_id>/prd.json` for later execution by the Forge or
//! MAGI scheduler.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tracing::warn;

// ---------------------------------------------------------------------------
// PRD Document
// ---------------------------------------------------------------------------

/// A structured Plan Requirements Document.
///
/// This is the output artifact of the plan engine. It captures everything the
/// agent needs to execute the task: what to build, how to build it, which
/// files to touch, and how to validate success.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrdDocument {
    /// Unique identifier matching the plan/task ID.
    pub id: String,

    /// Human-readable title.
    pub title: String,

    /// Expanded description of what needs to be accomplished.
    pub description: String,

    /// Concrete, measurable goals.
    pub goals: Vec<String>,

    /// How to determine if the task is done successfully.
    pub success_criteria: Vec<String>,

    /// Files that need to be created or modified (absolute paths).
    pub files_to_modify: Vec<FileAction>,

    /// Key architecture and design decisions.
    pub architecture_decisions: Vec<ArchitectureDecision>,

    /// Ordered list of implementation steps.
    pub implementation_steps: Vec<ImplementationStep>,

    /// Test plan.
    pub test_plan: TestPlan,

    /// Time estimate in minutes.
    pub estimate_minutes: u32,

    /// Dependencies (crates, external services, etc.) required.
    pub dependencies: Vec<String>,

    /// Constraints or non-functional requirements.
    pub constraints: Vec<String>,

    /// When the PRD was created. Preserved across regeneration for the same
    /// task id (see [`PrdGenerator::persist`]).
    pub created_at: DateTime<Utc>,

    /// When the PRD was last modified. Equal to `created_at` for a freshly
    /// generated PRD; refreshed when an existing PRD is regenerated.
    pub updated_at: DateTime<Utc>,

    /// Version of this PRD: `1` for a new document, incremented each time a PRD
    /// is regenerated for the same task id.
    pub version: u32,
}

/// An action to take on a specific file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAction {
    /// Absolute path to the file.
    pub path: String,

    /// What to do: create, modify, or delete.
    pub action: FileActionType,

    /// Brief description of the change.
    pub description: String,

    /// Estimated lines of code to add/change.
    pub estimated_lines: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileActionType {
    Create,
    Modify,
    Delete,
}

impl std::fmt::Display for FileActionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileActionType::Create => f.write_str("create"),
            FileActionType::Modify => f.write_str("modify"),
            FileActionType::Delete => f.write_str("delete"),
        }
    }
}

/// A key architecture or design decision with rationale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureDecision {
    /// Short label describing the decision.
    pub decision: String,

    /// Why this decision was made.
    pub rationale: String,

    /// Alternatives that were considered.
    pub alternatives: Vec<String>,
}

/// A single step in the ordered implementation plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplementationStep {
    /// Step number (1-based).
    pub step_number: u32,

    /// What to do in this step.
    pub description: String,

    /// Files involved in this step.
    pub files: Vec<String>,

    /// Estimated time for this step in minutes.
    pub estimated_minutes: u32,

    /// Whether this step is completed.
    #[serde(default)]
    pub completed: bool,
}

/// Test plan for the implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestPlan {
    /// Unit tests to write.
    pub unit_tests: Vec<String>,

    /// Integration tests to write.
    pub integration_tests: Vec<String>,

    /// Manual verification steps.
    pub manual_checks: Vec<String>,
}

impl Default for TestPlan {
    fn default() -> Self {
        Self {
            unit_tests: Vec::new(),
            integration_tests: Vec::new(),
            manual_checks: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// PRD Builder
// ---------------------------------------------------------------------------

/// Builder for constructing a [`PrdDocument`] incrementally during the plan
/// interview process.
#[derive(Debug, Clone)]
pub struct PrdBuilder {
    id: String,
    title: String,
    description: String,
    goals: Vec<String>,
    success_criteria: Vec<String>,
    files_to_modify: Vec<FileAction>,
    architecture_decisions: Vec<ArchitectureDecision>,
    implementation_steps: Vec<ImplementationStep>,
    test_plan: TestPlan,
    estimate_minutes: u32,
    dependencies: Vec<String>,
    constraints: Vec<String>,
}

impl PrdBuilder {
    /// Start building a PRD with the given id and title.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: String::new(),
            goals: Vec::new(),
            success_criteria: Vec::new(),
            files_to_modify: Vec::new(),
            architecture_decisions: Vec::new(),
            implementation_steps: Vec::new(),
            test_plan: TestPlan::default(),
            estimate_minutes: 0,
            dependencies: Vec::new(),
            constraints: Vec::new(),
        }
    }

    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    pub fn goal(mut self, goal: impl Into<String>) -> Self {
        self.goals.push(goal.into());
        self
    }

    pub fn goals(mut self, goals: Vec<String>) -> Self {
        self.goals.extend(goals);
        self
    }

    pub fn success_criterion(mut self, criterion: impl Into<String>) -> Self {
        self.success_criteria.push(criterion.into());
        self
    }

    pub fn success_criteria_list(mut self, criteria: Vec<String>) -> Self {
        self.success_criteria.extend(criteria);
        self
    }

    pub fn file_action(mut self, action: FileAction) -> Self {
        self.files_to_modify.push(action);
        self
    }

    pub fn architecture_decision(mut self, decision: ArchitectureDecision) -> Self {
        self.architecture_decisions.push(decision);
        self
    }

    /// Add multiple architecture decisions at once.
    pub fn architecture_decisions(mut self, decisions: Vec<ArchitectureDecision>) -> Self {
        self.architecture_decisions.extend(decisions);
        self
    }

    pub fn implementation_step(mut self, step: ImplementationStep) -> Self {
        self.implementation_steps.push(step);
        let total: u32 = self.implementation_steps.iter().map(|s| s.estimated_minutes).sum();
        self.estimate_minutes = total;
        self
    }

    pub fn test_plan(mut self, plan: TestPlan) -> Self {
        self.test_plan = plan;
        self
    }

    pub fn dependency(mut self, dep: impl Into<String>) -> Self {
        self.dependencies.push(dep.into());
        self
    }

    pub fn constraint(mut self, constraint: impl Into<String>) -> Self {
        self.constraints.push(constraint.into());
        self
    }

    /// Finalize and build the [`PrdDocument`].
    pub fn build(self) -> PrdDocument {
        let now = Utc::now();
        PrdDocument {
            id: self.id,
            title: self.title,
            description: self.description,
            goals: self.goals,
            success_criteria: self.success_criteria,
            files_to_modify: self.files_to_modify,
            architecture_decisions: self.architecture_decisions,
            implementation_steps: self.implementation_steps,
            test_plan: self.test_plan,
            estimate_minutes: self.estimate_minutes,
            dependencies: self.dependencies,
            constraints: self.constraints,
            created_at: now,
            updated_at: now,
            version: 1,
        }
    }
}

// ---------------------------------------------------------------------------
// PRD Generator
// ---------------------------------------------------------------------------

/// Errors that can occur during PRD generation or persistence.
#[derive(Debug, thiserror::Error)]
pub enum PrdError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("PRD generation requires at least one goal")]
    NoGoals,

    #[error("plan interview finished without producing a PRD")]
    NoPrd,
}

/// Generates a [`PrdDocument`] from the answers gathered during the interview.
///
/// This takes the interview answer summary and constructs a structured PRD
/// with inferred file actions, architecture decisions, and implementation steps.
#[derive(Debug)]
pub struct PrdGenerator {
    /// The working directory for resolving relative paths.
    working_dir: PathBuf,
}

impl PrdGenerator {
    /// Create a new PRD generator.
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }

    /// Generate a PRD from interview answers.
    ///
    /// `answers` is a list of (question, answer) pairs from the interview engine.
    /// `task_id` is the task/session identifier for persistence.
    /// `title` is a human-readable summary of the task.
    pub fn generate(
        &self,
        answers: &[(String, String)],
        task_id: &str,
        title: &str,
    ) -> Result<PrdDocument, PrdError> {
        // Extract key information from answers
        let goals = self.extract_goals(answers);
        if goals.is_empty() {
            return Err(PrdError::NoGoals);
        }

        let success_criteria = self.extract_success_criteria(answers);
        let constraints = self.extract_constraints(answers);
        let files = self.infer_files(answers, &goals);
        let arch_decisions = self.infer_architecture(answers);
        let steps = self.infer_steps(&files, answers);
        let test_plan = self.infer_test_plan(&files, answers);
        let dependencies = self.extract_dependencies(answers);
        let description = self.build_description(answers, &goals);

        let mut prd = PrdBuilder::new(task_id, title)
            .description(description)
            .goals(goals)
            .success_criteria_list(success_criteria)
            .architecture_decisions(arch_decisions)
            .test_plan(test_plan);

        // Only carry entries that actually matched. Joining an empty list used
        // to push a single `""` element, so every PRD's machine-readable lists
        // contained a meaningless blank string.
        for constraint in constraints {
            if !constraint.trim().is_empty() {
                prd = prd.constraint(constraint);
            }
        }
        for dependency in dependencies {
            if !dependency.trim().is_empty() {
                prd = prd.dependency(dependency);
            }
        }

        // Add files and steps individually
        for f in files {
            prd.files_to_modify.push(f);
        }
        for s in steps {
            prd.implementation_steps.push(s);
        }
        prd.estimate_minutes = prd.implementation_steps.iter().map(|s| s.estimated_minutes).sum();

        Ok(prd.build())
    }

    /// Persist a PRD to `~/.dscode/tasks/<task_id>/prd.json`.
    ///
    /// Regenerating a PRD for a task id that already has one keeps the original
    /// `created_at` and bumps `version`, so the "last modified"/"version" fields
    /// mean what their docs say instead of resetting to `1`/now on every write.
    pub fn persist(
        &self,
        prd: &PrdDocument,
        task_id: &str,
    ) -> Result<PathBuf, PrdError> {
        let config_dir = crate::config::settings::Config::data_dir()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))?;
        let task_dir = config_dir.join("tasks").join(task_id);
        std::fs::create_dir_all(&task_dir)?;

        let mut prd = prd.clone();
        if Self::exists(task_id) {
            match Self::load(task_id) {
                Ok(previous) => {
                    prd.created_at = previous.created_at;
                    prd.version = previous.version.saturating_add(1);
                    prd.updated_at = Utc::now();
                }
                Err(e) => warn!(
                    %e,
                    task = %task_id,
                    "existing prd.json could not be read — regenerating it from scratch"
                ),
            }
        }

        let prd_path = task_dir.join("prd.json");
        let json = serde_json::to_string_pretty(&prd)?;
        // Temp file + rename, like the interview state: a torn `fs::write` here
        // would leave an unparseable prd.json behind.
        let tmp = prd_path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &prd_path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e
        })?;

        Ok(prd_path)
    }

    /// Load a PRD from `~/.dscode/tasks/<task_id>/prd.json`.
    pub fn load(task_id: &str) -> Result<PrdDocument, PrdError> {
        let config_dir = crate::config::settings::Config::data_dir()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))?;
        let prd_path = config_dir.join("tasks").join(task_id).join("prd.json");
        let json = std::fs::read_to_string(&prd_path)?;
        let prd: PrdDocument = serde_json::from_str(&json)?;
        Ok(prd)
    }

    /// Check if a PRD exists for the given task ID.
    pub fn exists(task_id: &str) -> bool {
        if let Ok(config_dir) = crate::config::settings::Config::data_dir() {
            config_dir.join("tasks").join(task_id).join("prd.json").exists()
        } else {
            false
        }
    }

    // ------------------------------------------------------------------
    // Extraction helpers
    // ------------------------------------------------------------------

    fn extract_goals(&self, answers: &[(String, String)]) -> Vec<String> {
        answers
            .iter()
            .filter(|(q, _)| {
                let ql = q.to_lowercase();
                ql.contains("goal") || ql.contains("purpose") || ql.contains("accomplish")
            })
            .map(|(_, a)| a.clone())
            .collect()
    }

    fn extract_success_criteria(&self, answers: &[(String, String)]) -> Vec<String> {
        answers
            .iter()
            .filter(|(q, _)| {
                let ql = q.to_lowercase();
                ql.contains("acceptance") || ql.contains("success") || ql.contains("done")
            })
            .map(|(_, a)| a.clone())
            .collect()
    }

    fn extract_constraints(&self, answers: &[(String, String)]) -> Vec<String> {
        split_answer_items_for(answers, |ql| {
            ql.contains("constraint") || ql.contains("requirement")
        })
    }

    fn extract_dependencies(&self, answers: &[(String, String)]) -> Vec<String> {
        split_answer_items_for(answers, |ql| {
            ql.contains("dependenc") || ql.contains("crate") || ql.contains("library")
        })
    }

    fn build_description(&self, answers: &[(String, String)], goals: &[String]) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.push("## Goals".into());
        for (i, g) in goals.iter().enumerate() {
            parts.push(format!("{}. {}", i + 1, g));
        }
        parts.push("\n## Context".into());
        for (q, a) in answers.iter().take(5) {
            parts.push(format!("- Q: {}\n  A: {}", q, a));
        }
        parts.join("\n")
    }

    /// Derive the files this task touches from what the interview established.
    ///
    /// This deliberately does **not** walk the working directory. Doing so
    /// turned the *deliverable* of plan mode into a directory listing: every
    /// `.rs`/`.toml`/`.md` file in the repo became a "Modify" step (~200 steps,
    /// `estimate_minutes ≈ 3000` in this workspace) for a request like
    /// `/plan add a --verbose flag`, none of which the interview ever asked for.
    /// Only paths a participant actually named are considered, deduped by path.
    fn infer_files(&self, answers: &[(String, String)], _goals: &[String]) -> Vec<FileAction> {
        let mut found: BTreeMap<String, FileAction> = BTreeMap::new();
        for (_, answer) in answers {
            for raw in answer.split_whitespace() {
                let token = raw.trim_matches(|c: char| {
                    c == '`'
                        || c == '"'
                        || c == '\''
                        || c == ','
                        || c == ';'
                        || c == ':'
                        || c == '('
                        || c == ')'
                        || c == '['
                        || c == ']'
                        || c == '<'
                        || c == '>'
                });
                if !(token.ends_with(".rs") || token.ends_with(".toml") || token.ends_with(".md")) {
                    continue;
                }
                let Some(path) = self.resolve_mentioned_path(token) else {
                    continue;
                };
                let key = path.display().to_string();
                found.entry(key.clone()).or_insert(FileAction {
                    path: key,
                    action: FileActionType::Modify,
                    description: "Mentioned in interview".into(),
                    estimated_lines: 50,
                });
            }
        }
        found.into_values().collect()
    }

    /// Resolve a path token mentioned in an answer against the working dir.
    ///
    /// Rejects tokens that would replace the working directory outright (the old
    /// `working_dir.join("/etc/hosts.rs")` produced exactly that), anything
    /// containing `..`, and URL-ish text. Backtick/quotes around a path are
    /// stripped by the caller, so `` `main.rs` `` resolves instead of being
    /// missed because of a trailing backtick.
    fn resolve_mentioned_path(&self, token: &str) -> Option<PathBuf> {
        if token.contains("://") || token.is_empty() {
            return None;
        }
        let path = std::path::Path::new(token);
        // Checked before the absolute branch, which would otherwise let an
        // in-project `sub/../../etc` style token through.
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return None;
        }
        if path.has_root() {
            // `has_root`, not `is_absolute`: on Windows `is_absolute()` is false
            // for a root-only path such as `/etc/hosts.rs` (it has a root but no
            // drive prefix), so it fell through to `working_dir.join(..)` and was
            // accepted as an in-project file at `C:\etc\hosts.rs`.
            return if path.starts_with(&self.working_dir) {
                Some(path.to_path_buf())
            } else {
                None
            };
        }
        Some(self.working_dir.join(path))
    }

    /// Derive architecture decisions from what the interview actually settled.
    ///
    /// The previous implementation hardcoded *this repository's* conventions —
    /// "Use thiserror for library error types … Consistent with the existing
    /// codebase (forge.rs, config/settings.rs)" and "Use async Rust with Tokio
    /// runtime" — and emitted them as the **user's** architecture decisions for
    /// every project. When the interview never covered architecture, the
    /// honest answer is an empty list (the PRD then simply has no Architecture
    /// Decisions section) rather than an invented one.
    fn infer_architecture(&self, answers: &[(String, String)]) -> Vec<ArchitectureDecision> {
        answers
            .iter()
            .filter(|(q, a)| is_architecture_question(q) && !a.trim().is_empty())
            .map(|(q, a)| ArchitectureDecision {
                decision: q.trim().trim_end_matches(|c: char| c == '?' || c == '？').trim().to_string(),
                rationale: a.trim().to_string(),
                alternatives: Vec::new(),
            })
            .collect()
    }

    /// Turn inferred files into ordered steps, capped so a chatty answer cannot
    /// produce a plan with hundreds of steps (and a meaningless estimate).
    fn infer_steps(&self, files: &[FileAction], _answers: &[(String, String)]) -> Vec<ImplementationStep> {
        const MAX_INFERRED_STEPS: usize = 20;

        let mut steps: Vec<ImplementationStep> = files
            .iter()
            .take(MAX_INFERRED_STEPS)
            .enumerate()
            .map(|(i, f)| ImplementationStep {
                step_number: (i + 1) as u32,
                description: match f.action {
                    FileActionType::Create => format!("Create {}", f.path),
                    FileActionType::Modify => format!("Modify {}", f.path),
                    FileActionType::Delete => format!("Delete {}", f.path),
                },
                files: vec![f.path.clone()],
                estimated_minutes: 15,
                completed: false,
            })
            .collect();

        // A test step only makes sense when the plan touches Rust sources —
        // "cargo test" was previously emitted unconditionally, including for
        // projects that do not use Cargo at all.
        if files.iter().any(|f| f.path.ends_with(".rs")) {
            steps.push(ImplementationStep {
                step_number: (steps.len() + 1) as u32,
                description: "Run all tests and verify they pass".into(),
                files: vec!["cargo test".into()],
                estimated_minutes: 10,
                completed: false,
            });
        }
        steps
    }

    fn infer_test_plan(&self, files: &[FileAction], _answers: &[(String, String)]) -> TestPlan {
        let mut unit_tests = Vec::new();
        let mut integration_tests = Vec::new();

        for f in files {
            if f.path.ends_with(".rs") {
                unit_tests.push(format!(
                    "Add #[cfg(test)] mod tests to {} covering happy path and edge cases",
                    f.path
                ));
            }
        }

        integration_tests.push("Verify end-to-end flow through the public API".into());

        TestPlan {
            unit_tests,
            integration_tests,
            manual_checks: vec!["Code compiles with `cargo build`".into()],
        }
    }
}

// ---------------------------------------------------------------------------
// Answer text helpers
// ---------------------------------------------------------------------------

/// Whether an interview question is about architecture / design.
fn is_architecture_question(question: &str) -> bool {
    const PHRASES: &[&str] = &["error handling", "data flow", "data model", "data structure"];
    let ql = question.to_lowercase();
    if PHRASES.iter().any(|phrase| ql.contains(phrase)) {
        return true;
    }
    ql.split(|c: char| !c.is_alphanumeric()).any(|token| {
        token.starts_with("architect")
            || token.starts_with("design")
            || token == "module"
            || token == "modules"
            || token == "api"
            || token == "apis"
            || token == "schema"
            || token == "persistence"
            || token == "storage"
            || token == "database"
    })
}

/// Collect the answers to questions matching `matches`, split into items.
fn split_answer_items_for(
    answers: &[(String, String)],
    matches: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (q, a) in answers {
        if !matches(&q.to_lowercase()) {
            continue;
        }
        for item in split_answer_items(a) {
            if !out.iter().any(|existing| existing == &item) {
                out.push(item);
            }
        }
    }
    out
}

/// Split one interview answer into discrete items.
///
/// Interview answers are prose ("Use existing workspace dependencies where
/// possible. No new external services."), not lists. Storing the whole
/// paragraph as a single "dependency" produced a bogus crate name; splitting on
/// line/separator boundaries at least yields the individual claims. A
/// comma-separated segment is only treated as a list when every part is short
/// enough to be a name, so prose commas are not chopped into fragments.
fn split_answer_items(answer: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for line in answer.split(|c: char| c == '\n' || c == ';' || c == '；' || c == '•' || c == '·') {
        let line = line
            .trim_start_matches(|c: char| c == '-' || c == '*' || c == '•' || c.is_whitespace())
            .trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(|c: char| c == ',' || c == '，').map(str::trim).collect();
        let looks_like_a_list = parts.len() > 1
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().count() <= 40);
        if looks_like_a_list {
            items.extend(parts.into_iter().map(str::to_string));
        } else {
            items.push(line.to_string());
        }
    }
    items
        .into_iter()
        .map(|s| {
            s.trim()
                .trim_end_matches(|c: char| c == '.' || c == '。')
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prd_builder_basic() {
        let prd = PrdBuilder::new("task-1", "Test PRD")
            .description("A test description")
            .goal("Implement feature X")
            .success_criterion("All tests pass")
            .constraint("Must be thread-safe")
            .dependency("tokio")
            .build();

        assert_eq!(prd.id, "task-1");
        assert_eq!(prd.title, "Test PRD");
        assert_eq!(prd.description, "A test description");
        assert_eq!(prd.goals, vec!["Implement feature X"]);
        assert_eq!(prd.success_criteria, vec!["All tests pass"]);
        assert_eq!(prd.constraints, vec!["Must be thread-safe"]);
        assert_eq!(prd.dependencies, vec!["tokio"]);
        assert_eq!(prd.version, 1);
    }

    #[test]
    fn test_file_action_types() {
        assert_eq!(FileActionType::Create.to_string(), "create");
        assert_eq!(FileActionType::Modify.to_string(), "modify");
        assert_eq!(FileActionType::Delete.to_string(), "delete");
    }

    #[test]
    fn test_prd_builder_estimate() {
        let prd = PrdBuilder::new("task-1", "Test")
            .implementation_step(ImplementationStep {
                step_number: 1,
                description: "Step 1".into(),
                files: vec!["a.rs".into()],
                estimated_minutes: 30,
                completed: false,
            })
            .implementation_step(ImplementationStep {
                step_number: 2,
                description: "Step 2".into(),
                files: vec!["b.rs".into()],
                estimated_minutes: 20,
                completed: false,
            })
            .build();

        assert_eq!(prd.estimate_minutes, 50);
        assert_eq!(prd.implementation_steps.len(), 2);
    }

    #[test]
    fn test_prd_generator_no_goals() {
        let gen = PrdGenerator::new(PathBuf::from("/tmp"));
        let answers: Vec<(String, String)> = vec![
            ("What language?".into(), "Rust".into()),
        ];
        let result = gen.generate(&answers, "task-1", "Test");
        assert!(result.is_err());
        match result.unwrap_err() {
            PrdError::NoGoals => {}
            _ => panic!("Expected NoGoals error"),
        }
    }

    #[test]
    fn test_prd_generator_with_goals() {
        let tmp = tempfile::tempdir().unwrap();
        let gen = PrdGenerator::new(tmp.path().to_path_buf());
        let answers: Vec<(String, String)> = vec![
            ("What is the goal?".into(), "Implement the Plan engine".into()),
            ("Acceptance criteria?".into(), "All tests pass, code compiles".into()),
            ("Constraints?".into(), "Must use thiserror".into()),
            ("Dependencies?".into(), "tokio, serde, chrono".into()),
            (
                "What modules or files need to be created or modified?".into(),
                "Edit `src/plan.rs` and notes.md".into(),
            ),
        ];
        let prd = gen.generate(&answers, "task-1", "Plan Engine").unwrap();

        assert_eq!(prd.title, "Plan Engine");
        assert_eq!(prd.id, "task-1");
        assert_eq!(prd.goals, vec!["Implement the Plan engine"]);
        assert_eq!(prd.success_criteria, vec!["All tests pass, code compiles"]);
        assert_eq!(prd.constraints, vec!["Must use thiserror"]);
        assert_eq!(prd.dependencies, vec!["tokio", "serde", "chrono"]);
        // Only the files the interview named — not the repo's file listing.
        assert_eq!(prd.files_to_modify.len(), 2);
        assert!(prd
            .files_to_modify
            .iter()
            .any(|f| f.path.ends_with("plan.rs")));
        assert!(prd
            .files_to_modify
            .iter()
            .any(|f| f.path.ends_with("notes.md")));
        assert!(!prd.implementation_steps.is_empty());
        // Architecture comes from the interview, not from hardcoded defaults.
        assert_eq!(prd.architecture_decisions.len(), 1);
        assert!(prd.architecture_decisions[0].decision.contains("modules or files"));
    }

    #[test]
    fn test_infer_files_ignores_directory_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        for name in ["a.rs", "b.rs", "c.toml", "d.md"] {
            std::fs::write(src.join(name), "fn main() {}").unwrap();
        }

        let gen = PrdGenerator::new(tmp.path().to_path_buf());
        let answers: Vec<(String, String)> =
            vec![("What is the goal?".into(), "Add a --verbose flag".into())];
        let prd = gen.generate(&answers, "task-x", "Add flag").unwrap();

        // A directory walk would have listed all four files as "Modify" steps.
        assert!(prd.files_to_modify.is_empty());
        assert!(prd.implementation_steps.is_empty());
        assert_eq!(prd.estimate_minutes, 0);
    }

    #[test]
    fn test_mentioned_paths_are_validated_and_deduped() {
        let tmp = tempfile::tempdir().unwrap();
        let gen = PrdGenerator::new(tmp.path().to_path_buf());
        let answers: Vec<(String, String)> = vec![
            ("What is the goal?".into(), "Add a flag".into()),
            (
                "Which files?".into(),
                "Edit /etc/hosts.rs and src/main.rs and ../escape.rs and src/main.rs again".into(),
            ),
        ];
        let prd = gen.generate(&answers, "task-y", "Paths").unwrap();

        // Absolute outside the project and `..` paths are rejected; the repeated
        // mention is stored once.
        assert_eq!(prd.files_to_modify.len(), 1);
        assert!(prd.files_to_modify[0].path.ends_with("main.rs"));
    }

    #[test]
    fn test_architecture_decisions_empty_when_not_discussed() {
        let gen = PrdGenerator::new(PathBuf::from("/tmp"));
        let answers: Vec<(String, String)> = vec![
            ("What is the goal?".into(), "Ship the feature".into()),
            ("What is the timeline?".into(), "Two weeks".into()),
        ];
        let prd = gen.generate(&answers, "task-z", "Feature").unwrap();
        assert!(
            prd.architecture_decisions.is_empty(),
            "no architecture was discussed, so none may be invented"
        );
        assert!(prd.constraints.is_empty(), "unmatched constraints must not become \"\"");
        assert!(prd.dependencies.is_empty());
    }

    #[test]
    fn test_prd_persist_and_load() {
        let prd = PrdBuilder::new("test-task-id", "Test PRD")
            .description("A PRD for testing persistence")
            .goal("Verify save and load")
            .success_criterion("Round-trip is lossless")
            .build();

        // Write to a temp location (we can't use ~/.dscode in tests, so we test
        // serialization round-trip directly)
        let json = serde_json::to_string_pretty(&prd).unwrap();
        let loaded: PrdDocument = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.id, prd.id);
        assert_eq!(loaded.title, prd.title);
        assert_eq!(loaded.goals, prd.goals);
        assert_eq!(loaded.version, prd.version);
    }

    #[test]
    fn test_prd_serialization_roundtrip() {
        let prd = PrdBuilder::new("task-roundtrip", "Serialization Test")
            .description("Round-trip through JSON")
            .goal("Survive serialization")
            .goal("Survive deserialization")
            .success_criterion("Contents match after round-trip")
            .architecture_decision(ArchitectureDecision {
                decision: "Use JSON".into(),
                rationale: "Human-readable".into(),
                alternatives: vec!["TOML".into(), "YAML".into()],
            })
            .build();

        let json = serde_json::to_string(&prd).unwrap();
        let roundtripped: PrdDocument = serde_json::from_str(&json).unwrap();

        assert_eq!(roundtripped.id, prd.id);
        assert_eq!(roundtripped.title, prd.title);
        assert_eq!(roundtripped.goals, prd.goals);
        assert_eq!(roundtripped.architecture_decisions.len(), 1);
    }

    #[test]
    fn test_default_test_plan() {
        let plan = TestPlan::default();
        assert!(plan.unit_tests.is_empty());
        assert!(plan.integration_tests.is_empty());
        assert!(plan.manual_checks.is_empty());
    }
}
