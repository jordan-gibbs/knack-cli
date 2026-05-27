//! Runs API — start, finish, mark.

use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::ApiClient;
use crate::errors::CliError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub skill_version_id: String,
    pub agent_id: Option<String>,
    pub runtime: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: String,
    pub inputs_summary: Option<Value>,
    pub outputs_summary: Option<Value>,
    pub files_touched: Option<Vec<String>>,
    #[serde(default)]
    pub marks: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunCreate {
    pub skill_version_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs_summary: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunFinish {
    pub status: String, // "succeeded" | "failed" | "unknown"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs_summary: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_touched: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunMarkBody {
    pub status: String, // "succeeded" | "failed"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub async fn start(client: &ApiClient, body: &RunCreate) -> Result<Run, CliError> {
    let body = serde_json::to_value(body)?;
    client
        .send_json::<Run>(|c| Ok(c.request(Method::POST, "/runs")?.json(&body)))
        .await
}

pub async fn finish(client: &ApiClient, run_id: &str, body: &RunFinish) -> Result<Run, CliError> {
    let path = format!("/runs/{run_id}/finish");
    let body = serde_json::to_value(body)?;
    client
        .send_json::<Run>(|c| Ok(c.request(Method::POST, &path)?.json(&body)))
        .await
}

pub async fn mark(client: &ApiClient, run_id: &str, body: &RunMarkBody) -> Result<Run, CliError> {
    let path = format!("/runs/{run_id}/mark");
    let body = serde_json::to_value(body)?;
    client
        .send_json::<Run>(|c| Ok(c.request(Method::POST, &path)?.json(&body)))
        .await
}

pub async fn get(client: &ApiClient, run_id: &str) -> Result<Run, CliError> {
    let path = format!("/runs/{run_id}");
    client
        .send_json::<Run>(|c| c.request(Method::GET, &path))
        .await
}
