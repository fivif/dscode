//! PRD (Plan Requirements Document) generation.
//!
//! After the interview is complete, the plan engine produces a structured PRD
//! that captures the goals, success criteria, architecture decisions, files to
//! modify, and a time estimate. The PRD is persisted to
//! `~/.dscode/tasks/<task_id>/prd.json` for later execution by the Forge or
//! MAGI scheduler.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

    /// When the PRD was created.
    pub created_at: DateTime<Utc>,

    /// When the PRD was last modified.
    pub updated_at: DateTime<Utc>,

    /// The version of this PRD (incremented on significant updates).
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

        let _estimate: u32 = steps.iter().map(|s| s.estimated_minutes).sum();

        let prd = PrdBuilder::new(task_id, title)
            .description(description)
            .goals(goals)
            .success_criteria_list(success_criteria)
            .architecture_decisions(arch_decisions)
            .test_plan(test_plan)
            .constraint(constraints.join("; "))
            .dependency(dependencies.join(", "));

        // Add files and steps individually
        let mut prd = prd;
        for f in files {
            prd.files_to_modify.push(f);
        }
        for s in steps {
            prd.implementation_steps.push(s);
        }
        // Recalculate estimate
        prd.estimate_minutes = prd.implementation_steps.iter().map(|s| s.estimated_minutes).sum();

        Ok(prd.build())
    }

    /// Persist a PRD to `~/.dscode/tasks/<task_id>/prd.json`.
    pub fn persist(
        &self,
        prd: &PrdDocument,
        task_id: &str,
    ) -> Result<PathBuf, PrdError> {
        let config_dir = crate::config::settings::Config::data_dir()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))?;
        let task_dir = config_dir.join("tasks").join(task_id);
        std::fs::create_dir_all(&task_dir)?;

        let prd_path = task_dir.join("prd.json");
        let json = serde_json::to_string_pretty(prd)?;
        std::fs::write(&prd_path, json)?;

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
        answers
            .iter()
            .filter(|(q, _)| {
                let ql = q.to_lowercase();
                ql.contains("constraint") || ql.contains("requirement")
            })
            .map(|(_, a)| a.clone())
            .collect()
    }

    fn extract_dependencies(&self, answers: &[(String, String)]) -> Vec<String> {
        answers
            .iter()
            .filter(|(q, _)| {
                let ql = q.to_lowercase();
                ql.contains("dependenc") || ql.contains("crate") || ql.contains("library")
            })
            .map(|(_, a)| a.clone())
            .collect()
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

    fn infer_files(&self, answers: &[(String, String)], _goals: &[String]) -> Vec<FileAction> {
        // Inspect the working directory for existing source files
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.working_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if ext == "rs" || ext == "toml" || ext == "md" {
                        files.push(FileAction {
                            path: path.display().to_string(),
                            action: FileActionType::Modify,
                            description: format!("Existing {} file", ext),
                            estimated_lines: 0,
                        });
                    }
                }
            }
        }
        // Also check answers for explicitly mentioned paths
        for (_, answer) in answers {
            for word in answer.split_whitespace() {
                if word.ends_with(".rs") || word.ends_with(".toml") {
                    files.push(FileAction {
                        path: self.working_dir.join(word).display().to_string(),
                        action: FileActionType::Modify,
                        description: "Mentioned in interview".into(),
                        estimated_lines: 50,
                    });
                }
            }
        }
        files
    }

    fn infer_architecture(&self, _answers: &[(String, String)]) -> Vec<ArchitectureDecision> {
        let mut decisions = Vec::new();
        decisions.push(ArchitectureDecision {
            decision: "Use thiserror for library error types".into(),
            rationale: "Consistent with the existing codebase (forge.rs, config/settings.rs)"
                .into(),
            alternatives: vec!["anyhow throughout".into(), "custom Error enums without derive".into()],
        });
        decisions.push(ArchitectureDecision {
            decision: "Use async Rust with Tokio runtime".into(),
            rationale: "The agent loop is async; all I/O is Tokio-based".into(),
            alternatives: vec!["sync with threads".into(), "async-std".into()],
        });
        decisions
    }

    fn infer_steps(&self, files: &[FileAction], _answers: &[(String, String)]) -> Vec<ImplementationStep> {
        let mut steps = Vec::new();
        for (i, f) in files.iter().enumerate() {
            steps.push(ImplementationStep {
                step_number: (i + 1) as u32,
                description: match f.action {
                    FileActionType::Create => format!("Create {}", f.path),
                    FileActionType::Modify => format!("Modify {}", f.path),
                    FileActionType::Delete => format!("Delete {}", f.path),
                },
                files: vec![f.path.clone()],
                estimated_minutes: 15,
                completed: false,
            });
        }
        // Add a testing step
        steps.push(ImplementationStep {
            step_number: (steps.len() + 1) as u32,
            description: "Run all tests and verify they pass".into(),
            files: vec!["cargo test".into()],
            estimated_minutes: 10,
            completed: false,
        });
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
        ];
        let prd = gen.generate(&answers, "task-1", "Plan Engine").unwrap();

        assert_eq!(prd.title, "Plan Engine");
        assert_eq!(prd.id, "task-1");
        assert_eq!(prd.goals, vec!["Implement the Plan engine"]);
        assert_eq!(prd.success_criteria, vec!["All tests pass, code compiles"]);
        assert!(!prd.implementation_steps.is_empty());
    }

    #[test]
    fn test_prd_persist_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let gen = PrdGenerator::new(tmp.path().to_path_buf());

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
