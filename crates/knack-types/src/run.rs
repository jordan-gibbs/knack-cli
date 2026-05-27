use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunLog {
    pub run_id: Uuid,
    pub skill: String,
    pub status: RunStatus,
    pub duration_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub agent: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Succeeded,
    Failed,
    Aborted,
}
