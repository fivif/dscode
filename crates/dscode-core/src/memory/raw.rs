//! Raw message tier — verbatim conversation storage for replay and audit.

use serde::{Deserialize, Serialize};

/// A raw stored message snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
}

impl RawMessage {
    pub fn new(session_id: impl Into<String>, role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            role: role.into(),
            content: content.into(),
            created_at: chrono::Utc::now().timestamp(),
        }
    }
}
