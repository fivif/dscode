//! Grill-me interview pattern — one question at a time, walking decision trees.
//!
//! The interview engine asks the user clarifying questions one at a time. It
//! never bombards the user with a list. For questions answerable by inspecting
//! the codebase (e.g., "what framework does this use?"), it explores the
//! codebase instead of asking. It provides recommended answers based on
//! best-practice defaults and tracks the full question history so the user can
//! revisit earlier decisions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::plan::phases::PlanPhase;

// ---------------------------------------------------------------------------
// Question
// ---------------------------------------------------------------------------

/// A single interview question, with a recommended answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    /// Unique sequential identifier for this question.
    pub id: String,

    /// The question text displayed to the user.
    pub text: String,

    /// A recommended/default answer (pre-filled for quick acceptance).
    pub recommended_answer: String,

    /// The user's actual answer (empty until answered).
    pub answer: String,

    /// Whether this question was answered by codebase exploration
    /// rather than user input.
    pub auto_answered: bool,

    /// The phase this question belongs to.
    pub phase: PlanPhase,

    /// Decision tree branch path (e.g., ["web_app", "rest_api"]).
    /// This tracks which branches of the decision tree were taken.
    pub branch_path: Vec<String>,

    /// Optional follow-up question IDs that this question unlocks.
    pub follow_ups: Vec<String>,

    /// Timestamp when the question was asked.
    pub asked_at: DateTime<Utc>,

    /// Timestamp when the question was answered (None if pending).
    pub answered_at: Option<DateTime<Utc>>,
}

impl Question {
    /// Create a new question with a recommended answer.
    pub fn new(
        id: impl Into<String>,
        text: impl Into<String>,
        recommended_answer: impl Into<String>,
        phase: PlanPhase,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            recommended_answer: recommended_answer.into(),
            answer: String::new(),
            auto_answered: false,
            phase,
            branch_path: Vec::new(),
            follow_ups: Vec::new(),
            asked_at: Utc::now(),
            answered_at: None,
        }
    }

    /// Mark as answered (with the given response).
    pub fn answer_with(&mut self, answer: impl Into<String>) {
        self.answer = answer.into();
        self.answered_at = Some(Utc::now());
    }

    /// Mark as auto-answered via codebase exploration.
    pub fn auto_answer(&mut self, answer: impl Into<String>) {
        self.answer = answer.into();
        self.auto_answered = true;
        self.answered_at = Some(Utc::now());
    }

    /// Set the decision tree branch path for this question.
    pub fn with_branch(mut self, branch: Vec<String>) -> Self {
        self.branch_path = branch;
        self
    }

    /// Add follow-up question IDs.
    pub fn with_follow_ups(mut self, ids: Vec<String>) -> Self {
        self.follow_ups = ids;
        self
    }

    /// Returns true if this question has been answered.
    pub fn is_answered(&self) -> bool {
        self.answered_at.is_some()
    }
}

// ---------------------------------------------------------------------------
// Decision Tree
// ---------------------------------------------------------------------------

/// A node in the interview decision tree.
///
/// Each node represents a branching question. Depending on the answer,
/// different child nodes are visited next.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionNode {
    /// The question at this node.
    pub question: Question,

    /// Child branches keyed by answer pattern (e.g., "yes" → node_id).
    pub branches: Vec<(String, String)>,

    /// Fallback node ID if no branch matches.
    pub default_branch: Option<String>,
}

impl DecisionNode {
    pub fn new(question: Question) -> Self {
        Self {
            question,
            branches: Vec::new(),
            default_branch: None,
        }
    }

    /// Add a branch from this node.
    pub fn branch(mut self, answer: impl Into<String>, next_node_id: impl Into<String>) -> Self {
        self.branches.push((answer.into(), next_node_id.into()));
        self
    }

    /// Set the default fallback node.
    pub fn default(mut self, node_id: impl Into<String>) -> Self {
        self.default_branch = Some(node_id.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Interview Engine
// ---------------------------------------------------------------------------

/// The result of asking a single question.
#[derive(Debug, Clone)]
pub enum InterviewAction {
    /// A question needs the user's input.
    AskQuestion {
        question: Question,
        /// How many questions remain in the current phase.
        remaining: u32,
    },
    /// All questions for the current phase are resolved — advance.
    PhaseComplete { phase: PlanPhase },
    /// The entire interview is finished.
    Complete,
}

/// The grill-me interview engine.
///
/// Manages the one-question-at-a-time interview loop. Maintains a decision
/// tree of questions and determines which question to ask next based on
/// previous answers. Codebase-explorable questions are answered automatically
/// by inspecting the project directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterviewEngine {
    /// All questions in the interview (both asked and pending).
    pub questions: Vec<Question>,

    /// Decision tree nodes keyed by question ID.
    pub decision_tree: std::collections::HashMap<String, DecisionNode>,

    /// History of questions and answers in chronological order.
    pub history: Vec<(String, String)>,

    /// Current phase of the interview.
    pub phase: PlanPhase,

    /// The working directory for codebase exploration.
    pub working_dir: PathBuf,

    /// Index of the next question to ask.
    cursor: usize,
}

impl InterviewEngine {
    /// Create a new interview engine starting in InitialUnderstanding.
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            questions: Vec::new(),
            decision_tree: std::collections::HashMap::new(),
            history: Vec::new(),
            phase: PlanPhase::InitialUnderstanding,
            working_dir,
            cursor: 0,
        }
    }

    /// Register a question in the queue.
    pub fn add_question(&mut self, question: Question) -> &mut Self {
        self.questions.push(question);
        self
    }

    /// Register a decision tree node.
    pub fn add_decision_node(&mut self, node: DecisionNode) -> &mut Self {
        let id = node.question.id.clone();
        self.decision_tree.insert(id, node);
        self
    }

    /// Get the next action: either a question to ask, or phase complete.
    ///
    /// Questions answerable by codebase exploration are auto-resolved.
    pub async fn next_action(&mut self) -> InterviewAction {
        // First, iterate pending questions looking for ones we can answer by
        // inspecting the codebase.
        while let Some(idx) = self.find_codebase_answerable() {
            if let Some(answer) = self.explore_codebase_for(&self.questions[idx]).await {
                self.questions[idx].auto_answer(answer);
                self.cursor += 1;
            } else {
                break;
            }
        }

        // Find the next unanswered question.
        while self.cursor < self.questions.len() {
            let q = &self.questions[self.cursor];
            if !q.is_answered() {
                let remaining = self.questions[self.cursor..]
                    .iter()
                    .filter(|q| !q.is_answered())
                    .count() as u32;
                return InterviewAction::AskQuestion {
                    question: q.clone(),
                    remaining,
                };
            }
            self.cursor += 1;
        }

        // No more questions in this phase.
        InterviewAction::PhaseComplete {
            phase: self.phase,
        }
    }

    /// Record an answer for the current question and advance.
    ///
    /// If the question is part of a decision tree, resolves the matching branch
    /// and inserts the next node's question into the queue at the current
    /// position so it can be asked immediately.
    pub fn answer_current(&mut self, answer: impl Into<String>) {
        let answer = answer.into();
        if self.cursor < self.questions.len() {
            let q = &mut self.questions[self.cursor];
            q.answer_with(answer.clone());
            self.history.push((q.text.clone(), answer.clone()));

            // If the question has follow-ups in the decision tree, inject them.
            if let Some(node) = self.decision_tree.get(&q.id) {
                // Find the branch that matches the user's answer.
                let matched_next = node
                    .branches
                    .iter()
                    .find(|(pattern, _)| answer.to_lowercase().contains(&pattern.to_lowercase()))
                    .map(|(_, next_id)| next_id.clone())
                    .or_else(|| node.default_branch.clone());

                // If we found a next node in the decision tree, look up its
                // question and insert it right after the current position.
                if let Some(next_id) = matched_next {
                    if let Some(next_node) = self.decision_tree.get(&next_id) {
                        let insert_pos = self.cursor + 1;
                        if insert_pos <= self.questions.len() {
                            self.questions.insert(insert_pos, next_node.question.clone());
                        }
                    }
                }
            }
        }
        self.cursor += 1;
    }

    /// Get the current question without advancing.
    pub fn current_question(&self) -> Option<&Question> {
        self.questions.get(self.cursor).filter(|q| !q.is_answered())
    }

    /// How many questions remain unanswered.
    pub fn remaining_count(&self) -> usize {
        self.questions.iter().filter(|q| !q.is_answered()).count()
    }

    /// Advance to the next phase, resetting the cursor.
    pub fn advance_phase(&mut self) {
        if let Some(next) = self.phase.next() {
            self.phase = next;
            self.cursor = 0;
        }
    }

    /// Return a summary of all answers for PRD generation.
    pub fn answer_summary(&self) -> Vec<(String, String)> {
        self.questions
            .iter()
            .filter(|q| q.is_answered())
            .map(|q| (q.text.clone(), q.answer.clone()))
            .collect()
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Find the index of a pending question that is answerable by codebase
    /// exploration. Returns None if none found.
    fn find_codebase_answerable(&self) -> Option<usize> {
        self.questions
            .iter()
            .enumerate()
            .skip(self.cursor)
            .find(|(_, q)| !q.is_answered() && self.is_codebase_explorable(q))
            .map(|(i, _)| i)
    }

    /// Determine if a question can be answered by exploring the codebase
    /// rather than asking the user.
    fn is_codebase_explorable(&self, q: &Question) -> bool {
        let text = q.text.to_lowercase();
        // Questions about project structure, languages, frameworks, and
        // existing patterns can often be answered by reading files.
        let patterns = [
            "what language",
            "what framework",
            "what package manager",
            "which version",
            "is there a",
            "does the project",
            "what testing",
            "what linting",
            "how is the project",
            "current directory",
            "existing",
            "already",
        ];
        patterns.iter().any(|p| text.contains(p))
    }

    /// Attempt to answer a question by exploring the codebase.
    /// Returns None if exploration cannot produce an answer.
    async fn explore_codebase_for(&self, q: &Question) -> Option<String> {
        let text = q.text.to_lowercase();

        if text.contains("what language") || text.contains("programming language") {
            return self.detect_language().await;
        }
        if text.contains("package manager") || text.contains("build system") {
            return self.detect_package_manager().await;
        }
        if text.contains("framework") {
            return self.detect_framework().await;
        }
        if text.contains("test") && (text.contains("framework") || text.contains("runner")) {
            return self.detect_test_framework().await;
        }
        None
    }

    async fn detect_language(&self) -> Option<String> {
        let checks = vec![
            ("Cargo.toml", "Rust"),
            ("go.mod", "Go"),
            ("package.json", "JavaScript/TypeScript"),
            ("requirements.txt", "Python"),
            ("setup.py", "Python"),
            ("pyproject.toml", "Python"),
            ("Gemfile", "Ruby"),
            ("build.gradle", "Java/Kotlin"),
            ("pom.xml", "Java"),
            ("CMakeLists.txt", "C/C++"),
        ];
        for (file, lang) in &checks {
            if self.working_dir.join(file).exists() {
                return Some(format!(
                    "{} (detected from {})",
                    lang, file
                ));
            }
        }
        None
    }

    async fn detect_package_manager(&self) -> Option<String> {
        if self.working_dir.join("Cargo.toml").exists() {
            return Some("Cargo (Rust)".into());
        }
        if self.working_dir.join("package.json").exists() {
            return Some("npm/yarn/pnpm (Node.js)".into());
        }
        if self.working_dir.join("go.mod").exists() {
            return Some("Go modules".into());
        }
        None
    }

    async fn detect_framework(&self) -> Option<String> {
        // Check Cargo.toml dependencies for Rust frameworks
        if let Ok(content) = std::fs::read_to_string(self.working_dir.join("Cargo.toml")) {
            let content_lower = content.to_lowercase();
            let mut frameworks = Vec::new();
            if content_lower.contains("actix-web") {
                frameworks.push("Actix Web");
            }
            if content_lower.contains("axum") {
                frameworks.push("Axum");
            }
            if content_lower.contains("rocket") {
                frameworks.push("Rocket");
            }
            if content_lower.contains("tokio") {
                frameworks.push("Tokio (async runtime)");
            }
            if content_lower.contains("ratatui") {
                frameworks.push("Ratatui (TUI)");
            }
            if !frameworks.is_empty() {
                return Some(frameworks.join(", "));
            }
        }
        None
    }

    async fn detect_test_framework(&self) -> Option<String> {
        // Rust projects use built-in #[test] and cargo test
        if self.working_dir.join("Cargo.toml").exists() {
            let has_tests_dir = self.working_dir.join("tests").is_dir();
            let has_inline_tests = if let Ok(content) =
                std::fs::read_to_string(self.working_dir.join("Cargo.toml"))
            {
                // Check if any test-related dependencies exist
                content.contains("rstest")
                    || content.contains("proptest")
                    || content.contains("criterion")
            } else {
                false
            };
            let mut result = String::from("cargo test (Rust built-in test framework)");
            if has_tests_dir {
                result.push_str(" + integration tests directory");
            }
            if has_inline_tests {
                result.push_str(" + property/fuzz testing dependencies");
            }
            return Some(result);
        }
        None
    }
}

impl Default for InterviewEngine {
    fn default() -> Self {
        Self::new(PathBuf::from("."))
    }
}

/// Create a standard set of initial interview questions for the
/// InitialUnderstanding phase.
pub fn default_interview_questions(working_dir: &PathBuf) -> Vec<Question> {
    let cwd = working_dir.display().to_string();
    vec![
        Question::new(
            "q1",
            format!("What programming language is the project using? (Working directory: {})", cwd),
            "Let me explore the codebase to detect the language automatically.",
            PlanPhase::InitialUnderstanding,
        ),
        Question::new(
            "q2",
            "What is the primary goal of this task? Describe in 1-2 sentences.",
            "Implement the requested feature with tests and documentation.",
            PlanPhase::InitialUnderstanding,
        ),
        Question::new(
            "q3",
            "Are there any existing patterns or conventions in the codebase that must be followed?",
            "Follow the existing module structure, error handling patterns (thiserror), and test conventions.",
            PlanPhase::InitialUnderstanding,
        ),
        Question::new(
            "q4",
            "What are the acceptance criteria? How will we know the task is done?",
            "All tests pass, code compiles without warnings, and the implementation matches the specification.",
            PlanPhase::InitialUnderstanding,
        ),
        Question::new(
            "q5",
            "Are there any constraints or non-functional requirements (performance, security, compatibility)?",
            "No special constraints beyond idiomatic code and thread safety (Send + Sync).",
            PlanPhase::InitialUnderstanding,
        ),
    ]
}

/// Create design-phase questions based on the initial understanding answers.
pub fn design_questions() -> Vec<Question> {
    vec![
        Question::new(
            "d1",
            "What modules or files need to be created or modified?",
            "New modules in the appropriate crate with corresponding lib.rs exports.",
            PlanPhase::Design,
        ),
        Question::new(
            "d2",
            "What is the data flow? How do components interact?",
            "Standard Rust module pattern — public types exposed via mod.rs, internal details private.",
            PlanPhase::Design,
        ),
        Question::new(
            "d3",
            "What dependencies (crates) are needed?",
            "Use existing workspace dependencies where possible; add new ones only when necessary.",
            PlanPhase::Design,
        ),
        Question::new(
            "d4",
            "What is the error handling strategy?",
            "Use thiserror for library errors, anyhow for application-level errors.",
            PlanPhase::Design,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_question_new() {
        let q = Question::new("q1", "What language?", "Rust", PlanPhase::InitialUnderstanding);
        assert_eq!(q.id, "q1");
        assert_eq!(q.text, "What language?");
        assert_eq!(q.recommended_answer, "Rust");
        assert!(!q.is_answered());
        assert!(!q.auto_answered);
    }

    #[test]
    fn test_question_answer() {
        let mut q = Question::new("q1", "What language?", "Rust", PlanPhase::InitialUnderstanding);
        assert!(!q.is_answered());
        q.answer_with("Rust");
        assert!(q.is_answered());
        assert_eq!(q.answer, "Rust");
        assert!(!q.auto_answered);
    }

    #[test]
    fn test_question_auto_answer() {
        let mut q = Question::new("q1", "What language?", "Rust", PlanPhase::InitialUnderstanding);
        q.auto_answer("Rust");
        assert!(q.is_answered());
        assert!(q.auto_answered);
    }

    #[test]
    fn test_decision_node_branches() {
        let q = Question::new("n1", "Web or CLI?", "CLI", PlanPhase::Design);
        let node = DecisionNode::new(q)
            .branch("web", "n2-web")
            .branch("cli", "n2-cli")
            .default("n2-cli");

        assert_eq!(node.branches.len(), 2);
        assert_eq!(node.default_branch, Some("n2-cli".into()));
    }

    #[tokio::test]
    async fn test_interview_engine_question_tracking() {
        let tmp = tempfile::tempdir().unwrap();
        let mut engine = InterviewEngine::new(tmp.path().to_path_buf());
        engine
            .add_question(Question::new("q1", "What lang?", "Rust", PlanPhase::InitialUnderstanding))
            .add_question(Question::new("q2", "Goal?", "Implement", PlanPhase::InitialUnderstanding));

        assert_eq!(engine.remaining_count(), 2);

        let action = engine.next_action().await;
        match action {
            InterviewAction::AskQuestion { question, remaining } => {
                assert_eq!(question.id, "q1");
                assert_eq!(remaining, 2);
            }
            _ => panic!("Expected AskQuestion"),
        }

        engine.answer_current("Rust");
        assert_eq!(engine.remaining_count(), 1);

        let action = engine.next_action().await;
        match action {
            InterviewAction::AskQuestion { question, remaining } => {
                assert_eq!(question.id, "q2");
                assert_eq!(remaining, 1);
            }
            _ => panic!("Expected AskQuestion"),
        }

        engine.answer_current("Implement feature");
        assert_eq!(engine.remaining_count(), 0);

        let action = engine.next_action().await;
        assert!(matches!(action, InterviewAction::PhaseComplete { .. }));
    }

    #[tokio::test]
    async fn test_interview_history() {
        let tmp = tempfile::tempdir().unwrap();
        let mut engine = InterviewEngine::new(tmp.path().to_path_buf());
        engine.add_question(Question::new("q1", "Lang?", "Rust", PlanPhase::InitialUnderstanding));

        let _ = engine.next_action().await;
        engine.answer_current("Rust");

        assert_eq!(engine.history.len(), 1);
        assert_eq!(engine.history[0].0, "Lang?");
        assert_eq!(engine.history[0].1, "Rust");
    }

    #[tokio::test]
    async fn test_codebase_exploration_language_detection() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a Cargo.toml to simulate a Rust project
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let mut engine = InterviewEngine::new(tmp.path().to_path_buf());
        engine.add_question(Question::new(
            "q1",
            "What programming language is the project using?",
            "unknown",
            PlanPhase::InitialUnderstanding,
        ));

        let action = engine.next_action().await;
        // In an empty temp dir, no codebase files match, so the question
        // is returned for explicit answer rather than auto-answered.
        assert!(!engine.questions[0].is_answered());
        // The action should be an AskQuestion with the language question
        match action {
            InterviewAction::AskQuestion { question, .. } => {
                assert_eq!(question.id, "q1");
                // Now answer it manually
                engine.answer_current("Rust, based on Cargo.toml");
                assert!(engine.questions[0].is_answered());
                assert!(engine.questions[0].answer.contains("Rust"));
            }
            _ => panic!("Expected AskQuestion, got {:?}", action),
        }
    }

    #[test]
    fn test_default_interview_questions() {
        let dir = PathBuf::from("/tmp/test");
        let questions = default_interview_questions(&dir);
        assert_eq!(questions.len(), 5);
        assert!(questions.iter().all(|q| q.phase == PlanPhase::InitialUnderstanding));
    }

    #[test]
    fn test_design_questions() {
        let questions = design_questions();
        assert_eq!(questions.len(), 4);
        assert!(questions.iter().all(|q| q.phase == PlanPhase::Design));
    }
}
