//! Real-time event aggregation for multi-agent teams.
//!
//! The [`Monitor`] receives [`StreamEvent`]s from multiple sub-agents via a
//! shared channel, aggregates progress info, and exposes a snapshot of the
//! team's state at any moment.

use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::agent::stream::StreamEvent;

/// Current status of a single sub-agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// Not yet started.
    Pending,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Completed,
    /// Failed with an error.
    Failed(String),
}

/// A snapshot of a sub-agent's current state.
#[derive(Debug, Clone)]
pub struct AgentSnapshot {
    /// Sub-agent identifier.
    pub agent_id: String,
    /// Current status.
    pub status: AgentStatus,
    /// Accumulated text output so far.
    pub output: String,
    /// Current iteration count.
    pub iterations: u32,
    /// Current tool being executed (if any).
    pub current_tool: Option<String>,
    /// Unix timestamp of the last event.
    pub last_event_at: i64,
}

/// Aggregated state for all agents in a team.
#[derive(Debug, Clone)]
pub struct TeamSnapshot {
    /// Per-agent snapshots.
    pub agents: Vec<AgentSnapshot>,
    /// Total number of agents.
    pub total: usize,
    /// Number of agents completed.
    pub completed: usize,
    /// Number of agents that failed.
    pub failed: usize,
    /// Aggregate output from all completed agents.
    pub aggregate_output: String,
}

/// Real-time monitor for multi-agent execution.
pub struct Monitor {
    /// Receiver for StreamEvents from sub-agents.
    rx: mpsc::UnboundedReceiver<AgentStreamEvent>,
    /// Accumulated state per agent.
    agents: HashMap<String, AgentSnapshot>,
    /// Number of agents that have finished (completed or failed).
    finished_count: usize,
}

/// An event from a specific sub-agent, tagged with the agent's ID.
#[derive(Debug, Clone)]
pub struct AgentStreamEvent {
    /// The sub-agent's identifier.
    pub agent_id: String,
    /// The stream event emitted by the sub-agent.
    pub event: StreamEvent,
}

impl Monitor {
    /// Create a new monitor with a receiver channel.
    pub fn new(rx: mpsc::UnboundedReceiver<AgentStreamEvent>) -> Self {
        Self {
            rx,
            agents: HashMap::new(),
            finished_count: 0,
        }
    }

    /// Register agents that will be monitored.
    pub fn register_agent(&mut self, agent_id: String) {
        self.agents.insert(
            agent_id.clone(),
            AgentSnapshot {
                agent_id,
                status: AgentStatus::Pending,
                output: String::new(),
                iterations: 0,
                current_tool: None,
                last_event_at: chrono::Utc::now().timestamp(),
            },
        );
    }

    /// Process the next event from the channel (non-blocking).
    ///
    /// Returns `true` if there are more events to process, `false` if all
    /// agents have finished.
    pub fn try_recv_next(&mut self) -> bool {
        match self.rx.try_recv() {
            Ok(agent_event) => {
                self.apply_event(agent_event);
                true
            }
            Err(mpsc::error::TryRecvError::Empty) => {
                // No event available yet — not done.
                !self.is_fully_done()
            }
            Err(mpsc::error::TryRecvError::Disconnected) => {
                false
            }
        }
    }

    /// Block until the next event arrives, then process it.
    /// Returns `true` if more events may come, `false` if the channel closed.
    pub async fn recv_next(&mut self) -> bool {
        match self.rx.recv().await {
            Some(agent_event) => {
                self.apply_event(agent_event);
                true
            }
            None => false,
        }
    }

    /// Apply a single agent event to the accumulated state.
    fn apply_event(&mut self, ae: AgentStreamEvent) {
        let now = chrono::Utc::now().timestamp();
        let entry = self.agents.entry(ae.agent_id.clone()).or_insert_with(|| {
            AgentSnapshot {
                agent_id: ae.agent_id.clone(),
                status: AgentStatus::Pending,
                output: String::new(),
                iterations: 0,
                current_tool: None,
                last_event_at: now,
            }
        });

        entry.last_event_at = now;

        match &ae.event {
            StreamEvent::Token { content } => {
                entry.status = AgentStatus::Running;
                entry.output.push_str(content);
            }
            StreamEvent::Thinking { content: _, step } => {
                entry.status = AgentStatus::Running;
                entry.iterations = entry.iterations.max(*step);
            }
            StreamEvent::ToolStart { id: _, name, .. } => {
                entry.status = AgentStatus::Running;
                entry.current_tool = Some(name.clone());
            }
            StreamEvent::ToolProgress { id: _, chunk: _ } => {
                // Keep current tool.
            }
            StreamEvent::ToolEnd { id: _, status: _, result: _ } => {
                entry.current_tool = None;
            }
            StreamEvent::Complete { usage: _ } => {
                if !matches!(entry.status, AgentStatus::Completed | AgentStatus::Failed(_)) {
                    entry.status = AgentStatus::Completed;
                    self.finished_count += 1;
                }
            }
            StreamEvent::Error { content } => {
                if !matches!(entry.status, AgentStatus::Completed | AgentStatus::Failed(_)) {
                    entry.status = AgentStatus::Failed(content.clone());
                    self.finished_count += 1;
                }
            }
            StreamEvent::Fact { .. } => {
                // Facts are informational; don't change status.
            }
            StreamEvent::TeamAgentStart { .. }
            | StreamEvent::TeamAgentOutput { .. }
            | StreamEvent::TeamAgentEnd { .. }
            | StreamEvent::TeamComplete { .. }
            | StreamEvent::PlanQuestion { .. } => {
                // Team / plan UI events are handled at a higher level.
            }
        }
    }

    /// Check if all registered agents have finished.
    pub fn is_fully_done(&self) -> bool {
        !self.agents.is_empty() && self.finished_count >= self.agents.len()
    }

    /// Take a snapshot of the current state of all agents.
    pub fn snapshot(&self) -> TeamSnapshot {
        let mut agents: Vec<AgentSnapshot> = self.agents.values().cloned().collect();
        agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));

        let total = agents.len();
        let completed = agents.iter().filter(|a| a.status == AgentStatus::Completed).count();
        let failed = agents
            .iter()
            .filter(|a| matches!(a.status, AgentStatus::Failed(_)))
            .count();
        let aggregate_output = agents
            .iter()
            .filter(|a| a.status == AgentStatus::Completed)
            .map(|a| format!("## {}\n\n{}", a.agent_id, a.output))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        TeamSnapshot {
            agents,
            total,
            completed,
            failed,
            aggregate_output,
        }
    }

    /// Consume all pending events and return the final snapshot.
    pub async fn collect_all(mut self) -> TeamSnapshot {
        while self.recv_next().await {}
        self.snapshot()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_monitor_single_agent() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut monitor = Monitor::new(rx);
        monitor.register_agent("agent-1".into());

        tx.send(AgentStreamEvent {
            agent_id: "agent-1".into(),
            event: StreamEvent::Token {
                content: "Hello".into(),
            },
        })
        .unwrap();
        tx.send(AgentStreamEvent {
            agent_id: "agent-1".into(),
            event: StreamEvent::Complete { usage: None },
        })
        .unwrap();
        drop(tx);

        let snapshot = monitor.collect_all().await;
        assert_eq!(snapshot.total, 1);
        assert_eq!(snapshot.completed, 1);
        assert_eq!(snapshot.failed, 0);
        assert!(snapshot.aggregate_output.contains("Hello"));
    }

    #[tokio::test]
    async fn test_monitor_multiple_agents() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut monitor = Monitor::new(rx);
        monitor.register_agent("agent-1".into());
        monitor.register_agent("agent-2".into());

        tx.send(AgentStreamEvent {
            agent_id: "agent-1".into(),
            event: StreamEvent::Token {
                content: "Result A".into(),
            },
        })
        .unwrap();
        tx.send(AgentStreamEvent {
            agent_id: "agent-1".into(),
            event: StreamEvent::Complete { usage: None },
        })
        .unwrap();
        tx.send(AgentStreamEvent {
            agent_id: "agent-2".into(),
            event: StreamEvent::Error {
                content: "Something broke".into(),
            },
        })
        .unwrap();
        drop(tx);

        let snapshot = monitor.collect_all().await;
        assert_eq!(snapshot.total, 2);
        assert_eq!(snapshot.completed, 1);
        assert_eq!(snapshot.failed, 1);
    }

    #[tokio::test]
    async fn test_monitor_tool_tracking() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut monitor = Monitor::new(rx);
        monitor.register_agent("agent-1".into());

        tx.send(AgentStreamEvent {
            agent_id: "agent-1".into(),
            event: StreamEvent::ToolStart {
                id: "call_1".into(),
                name: "do_bash".into(),
                description: String::new(),
                arguments: String::new(),
            },
        })
        .unwrap();
        drop(tx);

        let snapshot = monitor.collect_all().await;
        assert_eq!(snapshot.agents[0].current_tool, Some("do_bash".into()));
    }
}
