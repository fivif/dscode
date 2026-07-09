//! Shared TaskBoard — DAG is the sole scheduler authority.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::role::AgentRole;

/// Task lifecycle (no long-lived Ready/Claimed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Done,
    Failed,
    Blocked,
    Cancelled,
}

/// Wave label only — not a scheduling key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaveKind {
    Research,
    Implement,
    Verify,
}

impl WaveKind {
    pub fn from_role(role: AgentRole) -> Self {
        match role {
            AgentRole::Explore => WaveKind::Research,
            AgentRole::Implement => WaveKind::Implement,
            AgentRole::Verify => WaveKind::Verify,
        }
    }
}

/// One task on the board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub role: AgentRole,
    pub dependencies: Vec<String>,
    pub owned_paths: Vec<String>,
    pub status: TaskStatus,
    pub owner_agent_id: Option<String>,
    pub result_summary: Option<String>,
    /// ≤ 4 KiB chars
    pub result_excerpt: Option<String>,
    pub wave: WaveKind,
    #[serde(default)]
    pub standalone: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl TaskSpec {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        prompt: impl Into<String>,
        role: AgentRole,
    ) -> Self {
        let now = chrono::Utc::now().timestamp();
        let role = role;
        Self {
            id: id.into(),
            title: title.into(),
            prompt: prompt.into(),
            role,
            dependencies: vec![],
            owned_paths: vec![],
            status: TaskStatus::Pending,
            owner_agent_id: None,
            result_summary: None,
            result_excerpt: None,
            wave: WaveKind::from_role(role),
            standalone: false,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BoardError {
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("task not schedulable: {0}")]
    NotReady(String),
    #[error("task already claimed: {0}")]
    AlreadyTaken(String),
    #[error("dependency cycle detected")]
    Cycle,
    #[error("invalid transition for task {id}: {from:?} → {to:?}")]
    InvalidTransition {
        id: String,
        from: TaskStatus,
        to: TaskStatus,
    },
    #[error("duplicate task id: {0}")]
    DuplicateId(String),
}

/// Shared task board for a teams session.
#[derive(Debug, Clone)]
pub struct TaskBoard {
    pub session_id: String,
    tasks: HashMap<String, TaskSpec>,
    pub persist_path: Option<PathBuf>,
}

impl TaskBoard {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            tasks: HashMap::new(),
            persist_path: None,
        }
    }

    pub fn with_persist_path(mut self, path: PathBuf) -> Self {
        self.persist_path = Some(path);
        self
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&TaskSpec> {
        self.tasks.get(id)
    }

    pub fn tasks(&self) -> impl Iterator<Item = &TaskSpec> {
        self.tasks.values()
    }

    pub fn upsert(&mut self, mut task: TaskSpec) -> Result<(), BoardError> {
        let now = chrono::Utc::now().timestamp();
        if let Some(existing) = self.tasks.get(&task.id) {
            task.created_at = existing.created_at;
            task.updated_at = now;
        } else {
            task.created_at = now;
            task.updated_at = now;
        }
        self.tasks.insert(task.id.clone(), task);
        self.refresh_blocked();
        Ok(())
    }

    /// Insert many; reject duplicate ids within the batch.
    pub fn upsert_many(&mut self, tasks: Vec<TaskSpec>) -> Result<(), BoardError> {
        let mut seen = HashSet::new();
        for t in &tasks {
            if !seen.insert(t.id.clone()) {
                return Err(BoardError::DuplicateId(t.id.clone()));
            }
        }
        for t in tasks {
            self.upsert(t)?;
        }
        Ok(())
    }

    pub fn is_schedulable(&self, id: &str) -> bool {
        let Some(t) = self.tasks.get(id) else {
            return false;
        };
        if t.status != TaskStatus::Pending {
            return false;
        }
        t.dependencies.iter().all(|d| {
            self.tasks
                .get(d)
                .map(|dep| dep.status == TaskStatus::Done)
                .unwrap_or(false)
        })
    }

    pub fn schedulable_tasks(&self) -> Vec<&TaskSpec> {
        let mut out: Vec<&TaskSpec> = self
            .tasks
            .values()
            .filter(|t| self.is_schedulable(&t.id))
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Atomic claim: Pending + deps Done → Running.
    pub fn claim(&mut self, id: &str, agent_id: &str) -> Result<&TaskSpec, BoardError> {
        if !self.is_schedulable(id) {
            if self.tasks.get(id).map(|t| t.status == TaskStatus::Running) == Some(true) {
                return Err(BoardError::AlreadyTaken(id.to_string()));
            }
            return Err(BoardError::NotReady(id.to_string()));
        }
        let now = chrono::Utc::now().timestamp();
        let t = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| BoardError::NotFound(id.to_string()))?;
        t.status = TaskStatus::Running;
        t.owner_agent_id = Some(agent_id.to_string());
        t.updated_at = now;
        Ok(self.tasks.get(id).unwrap())
    }

    pub fn mark_done(
        &mut self,
        id: &str,
        summary: String,
        excerpt: String,
    ) -> Result<(), BoardError> {
        let now = chrono::Utc::now().timestamp();
        let t = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| BoardError::NotFound(id.to_string()))?;
        if t.status != TaskStatus::Running {
            return Err(BoardError::InvalidTransition {
                id: id.to_string(),
                from: t.status,
                to: TaskStatus::Done,
            });
        }
        t.status = TaskStatus::Done;
        t.result_summary = Some(summary);
        t.result_excerpt = Some(truncate_excerpt(&excerpt, 4096));
        t.updated_at = now;
        self.refresh_blocked();
        Ok(())
    }

    pub fn mark_failed(&mut self, id: &str, err: String) -> Result<(), BoardError> {
        let now = chrono::Utc::now().timestamp();
        let t = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| BoardError::NotFound(id.to_string()))?;
        if !matches!(t.status, TaskStatus::Running | TaskStatus::Pending) {
            return Err(BoardError::InvalidTransition {
                id: id.to_string(),
                from: t.status,
                to: TaskStatus::Failed,
            });
        }
        t.status = TaskStatus::Failed;
        t.result_summary = Some(err);
        t.updated_at = now;
        self.refresh_blocked();
        Ok(())
    }

    pub fn mark_cancelled(&mut self, id: &str) -> Result<(), BoardError> {
        let now = chrono::Utc::now().timestamp();
        let t = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| BoardError::NotFound(id.to_string()))?;
        // Allow cancel from Running or Pending (and re-cancel is ok)
        if matches!(
            t.status,
            TaskStatus::Done | TaskStatus::Failed | TaskStatus::Blocked
        ) {
            // Force cancel only if not already terminal success/fail; still allow override
            if t.status == TaskStatus::Done {
                return Ok(());
            }
        }
        t.status = TaskStatus::Cancelled;
        t.updated_at = now;
        self.refresh_blocked();
        Ok(())
    }

    /// deps Failed/Cancelled → Blocked for still-Pending tasks.
    pub fn refresh_blocked(&mut self) {
        let now = chrono::Utc::now().timestamp();
        let terminal_fail: HashSet<String> = self
            .tasks
            .iter()
            .filter(|(_, t)| {
                matches!(t.status, TaskStatus::Failed | TaskStatus::Cancelled)
            })
            .map(|(id, _)| id.clone())
            .collect();

        for t in self.tasks.values_mut() {
            if t.status != TaskStatus::Pending {
                continue;
            }
            if t.dependencies.iter().any(|d| terminal_fail.contains(d)) {
                t.status = TaskStatus::Blocked;
                t.updated_at = now;
            }
        }
    }

    /// Kahn layering for parallel groups. Cycle → Err.
    pub fn parallel_layers(&self) -> Result<Vec<Vec<String>>, BoardError> {
        let mut indeg: HashMap<String, usize> = HashMap::new();
        let mut adj: HashMap<String, Vec<String>> = HashMap::new();
        for t in self.tasks.values() {
            indeg.entry(t.id.clone()).or_insert(0);
            for d in &t.dependencies {
                if self.tasks.contains_key(d) {
                    adj.entry(d.clone()).or_default().push(t.id.clone());
                    *indeg.entry(t.id.clone()).or_insert(0) += 1;
                }
            }
        }
        let mut q: VecDeque<String> = indeg
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(id, _)| id.clone())
            .collect();
        let mut layers = Vec::new();
        let mut seen = 0usize;
        while !q.is_empty() {
            let mut layer = Vec::new();
            let n = q.len();
            for _ in 0..n {
                let id = q.pop_front().unwrap();
                layer.push(id.clone());
                seen += 1;
                if let Some(children) = adj.get(&id) {
                    for c in children {
                        if let Some(deg) = indeg.get_mut(c) {
                            *deg -= 1;
                            if *deg == 0 {
                                q.push_back(c.clone());
                            }
                        }
                    }
                }
            }
            layer.sort();
            layers.push(layer);
        }
        if seen != self.tasks.len() {
            return Err(BoardError::Cycle);
        }
        Ok(layers)
    }

    pub fn counts(&self) -> BoardCounts {
        let mut c = BoardCounts::default();
        for t in self.tasks.values() {
            match t.status {
                TaskStatus::Pending => c.pending += 1,
                TaskStatus::Running => c.running += 1,
                TaskStatus::Done => c.done += 1,
                TaskStatus::Failed => c.failed += 1,
                TaskStatus::Blocked => c.blocked += 1,
                TaskStatus::Cancelled => c.cancelled += 1,
            }
        }
        c.total = self.tasks.len();
        c
    }

    pub fn all_terminal(&self) -> bool {
        self.tasks.values().all(|t| {
            matches!(
                t.status,
                TaskStatus::Done
                    | TaskStatus::Failed
                    | TaskStatus::Blocked
                    | TaskStatus::Cancelled
            )
        })
    }

    pub fn any_running(&self) -> bool {
        self.tasks
            .values()
            .any(|t| t.status == TaskStatus::Running)
    }

    pub fn persist(&self) -> std::io::Result<()> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let list: Vec<&TaskSpec> = self.tasks.values().collect();
        let json = serde_json::to_string_pretty(&list)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardCounts {
    pub total: usize,
    pub pending: usize,
    pub running: usize,
    pub done: usize,
    pub failed: usize,
    pub blocked: usize,
    pub cancelled: usize,
}

pub fn truncate_excerpt(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_requires_deps_done() {
        let mut b = TaskBoard::new("s1");
        let a = TaskSpec::new("a", "A", "do a", AgentRole::Implement);
        let mut c = TaskSpec::new("c", "C", "do c", AgentRole::Implement);
        c.dependencies = vec!["a".into()];
        b.upsert(a).unwrap();
        b.upsert(c).unwrap();

        assert!(b.is_schedulable("a"));
        assert!(!b.is_schedulable("c"));
        b.claim("a", "agent-a").unwrap();
        b.mark_done("a", "ok".into(), "excerpt".into()).unwrap();
        assert!(b.is_schedulable("c"));
        b.claim("c", "agent-c").unwrap();
    }

    #[test]
    fn parallel_layers_and_cycle() {
        let mut b = TaskBoard::new("s");
        let mut t1 = TaskSpec::new("1", "1", "p", AgentRole::Implement);
        let mut t2 = TaskSpec::new("2", "2", "p", AgentRole::Implement);
        t2.dependencies = vec!["1".into()];
        b.upsert(t1).unwrap();
        b.upsert(t2).unwrap();
        let layers = b.parallel_layers().unwrap();
        assert_eq!(
            layers,
            vec![vec!["1".to_string()], vec!["2".to_string()]]
        );

        // cycle
        let mut t1 = TaskSpec::new("1", "1", "p", AgentRole::Implement);
        t1.dependencies = vec!["2".into()];
        let mut t2 = TaskSpec::new("2", "2", "p", AgentRole::Implement);
        t2.dependencies = vec!["1".into()];
        let mut b2 = TaskBoard::new("s2");
        b2.upsert(t1).unwrap();
        b2.upsert(t2).unwrap();
        assert_eq!(b2.parallel_layers(), Err(BoardError::Cycle));
    }

    #[test]
    fn failed_dep_blocks() {
        let mut b = TaskBoard::new("s");
        let a = TaskSpec::new("a", "A", "p", AgentRole::Implement);
        let mut c = TaskSpec::new("c", "C", "p", AgentRole::Implement);
        c.dependencies = vec!["a".into()];
        b.upsert(a).unwrap();
        b.upsert(c).unwrap();
        b.claim("a", "x").unwrap();
        b.mark_failed("a", "boom".into()).unwrap();
        assert_eq!(b.get("c").unwrap().status, TaskStatus::Blocked);
    }
}
