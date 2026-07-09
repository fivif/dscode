//! Per-agent cancel tokens and nudge buffers for teams sessions.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

/// Handle for one running sub-agent.
#[derive(Debug)]
pub struct AgentHandle {
    pub agent_id: String,
    pub task_id: String,
    pub cancel: CancellationToken,
    pub nudge: Arc<Mutex<Vec<String>>>,
    pub notify: Arc<Notify>,
}

/// Session-scoped control plane.
#[derive(Debug, Default)]
pub struct TeamControlPlane {
    agents: Mutex<HashMap<String, AgentHandle>>,
    session_cancel: Mutex<Option<CancellationToken>>,
}

impl TeamControlPlane {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn begin_session(&self, parent: CancellationToken) {
        let mut g = self.session_cancel.lock().await;
        *g = Some(parent);
    }

    pub async fn end_session(&self) {
        let mut agents = self.agents.lock().await;
        for h in agents.values() {
            h.cancel.cancel();
        }
        agents.clear();
        *self.session_cancel.lock().await = None;
    }

    pub async fn register(
        &self,
        agent_id: String,
        task_id: String,
    ) -> (CancellationToken, Arc<Mutex<Vec<String>>>, Arc<Notify>) {
        let parent = self
            .session_cancel
            .lock()
            .await
            .clone()
            .unwrap_or_else(CancellationToken::new);
        let cancel = parent.child_token();
        let nudge = Arc::new(Mutex::new(Vec::new()));
        let notify = Arc::new(Notify::new());
        let handle = AgentHandle {
            agent_id: agent_id.clone(),
            task_id,
            cancel: cancel.clone(),
            nudge: nudge.clone(),
            notify: notify.clone(),
        };
        self.agents.lock().await.insert(agent_id, handle);
        (cancel, nudge, notify)
    }

    pub async fn unregister(&self, agent_id: &str) {
        self.agents.lock().await.remove(agent_id);
    }

    pub async fn stop_agent(&self, agent_id: &str) -> bool {
        if let Some(h) = self.agents.lock().await.get(agent_id) {
            h.cancel.cancel();
            true
        } else {
            false
        }
    }

    pub async fn stop_all(&self) {
        let agents = self.agents.lock().await;
        for h in agents.values() {
            h.cancel.cancel();
        }
    }

    pub async fn nudge_agent(&self, agent_id: &str, message: String) -> bool {
        if let Some(h) = self.agents.lock().await.get(agent_id) {
            h.nudge.lock().await.push(message);
            h.notify.notify_one();
            true
        } else {
            false
        }
    }

    pub async fn list_agent_ids(&self) -> Vec<String> {
        self.agents.lock().await.keys().cloned().collect()
    }
}

/// Global registry of active team control planes by session.
#[derive(Debug, Default)]
pub struct ControlPlaneRegistry {
    inner: Mutex<HashMap<String, Arc<TeamControlPlane>>>,
}

impl ControlPlaneRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_or_create(&self, session_id: &str) -> Arc<TeamControlPlane> {
        let mut g = self.inner.lock().await;
        g.entry(session_id.to_string())
            .or_insert_with(|| Arc::new(TeamControlPlane::new()))
            .clone()
    }

    pub async fn get(&self, session_id: &str) -> Option<Arc<TeamControlPlane>> {
        self.inner.lock().await.get(session_id).cloned()
    }

    pub async fn remove(&self, session_id: &str) {
        if let Some(cp) = self.inner.lock().await.remove(session_id) {
            cp.end_session().await;
        }
    }
}

/// Process-wide registry for desktop IPC stop/nudge.
static GLOBAL_CP: std::sync::OnceLock<ControlPlaneRegistry> = std::sync::OnceLock::new();

pub fn global_control_planes() -> &'static ControlPlaneRegistry {
    GLOBAL_CP.get_or_init(ControlPlaneRegistry::new)
}
