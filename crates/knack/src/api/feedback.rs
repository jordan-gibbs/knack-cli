//! Feedback (agent ↔ maintainer) endpoints.
//!
//! A thread is a two-sided conversation between the user's account
//! (web + agents) and Knack staff. The CLI only talks to the
//! caller-facing side (`/feedback/*`); the staff side lives at
//! `/admin/feedback/*` and is served by the web inbox.
//!
//! The `Thread` and `Message` types mirror the server's `ThreadRead` /
//! `MessageRead` schemas. New optional fields can be added without
//! breaking older CLIs because every field is `#[serde(default)]` and
//! unknown fields are ignored.
//!
//! Server-side, `from_side` is always derived from the route — the CLI
//! cannot post an "admin" message even if it tried.

use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::ApiClient;
use crate::errors::CliError;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub id: String,
    /// "user" or "admin". Set server-side; CLI never trusts the body it
    /// posted — it always reads back what the server stored.
    pub from_side: String,
    #[serde(default)]
    pub author_user_id: Option<String>,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Thread {
    pub id: String,
    pub subject: String,
    /// "open" or "closed".
    pub status: String,
    pub opened_by_user_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub skill_id: Option<String>,
    #[serde(default)]
    pub cli_context: Option<Value>,
    #[serde(default)]
    pub has_unread_admin_replies: bool,
    #[serde(default)]
    pub has_unread_user_messages: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ThreadListItem {
    pub id: String,
    pub subject: String,
    pub status: String,
    pub opened_by_user_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub skill_id: Option<String>,
    #[serde(default)]
    pub has_unread_admin_replies: bool,
    #[serde(default)]
    pub has_unread_user_messages: bool,
    #[serde(default)]
    pub message_count: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct OpenBody<'a> {
    subject: &'a str,
    body: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skill_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cli_context: Option<&'a Value>,
}

#[derive(Debug, Serialize)]
struct MessageBody<'a> {
    body: &'a str,
}

pub async fn open(
    client: &ApiClient,
    subject: &str,
    body: &str,
    run_id: Option<&str>,
    skill_id: Option<&str>,
    cli_context: Option<&Value>,
) -> Result<Thread, CliError> {
    let payload = serde_json::to_value(OpenBody {
        subject,
        body,
        run_id,
        skill_id,
        cli_context,
    })?;
    client
        .send_json::<Thread>(|c| Ok(c.request(Method::POST, "/feedback/threads")?.json(&payload)))
        .await
}

pub async fn list(
    client: &ApiClient,
    status: Option<&str>,
) -> Result<Vec<ThreadListItem>, CliError> {
    let status = status.map(str::to_string);
    client
        .send_json::<Vec<ThreadListItem>>(|c| {
            let mut rb = c.request(Method::GET, "/feedback/threads")?;
            if let Some(s) = &status {
                rb = rb.query(&[("status", s)]);
            }
            Ok(rb)
        })
        .await
}

pub async fn show(client: &ApiClient, thread_id: &str) -> Result<Thread, CliError> {
    let path = format!("/feedback/threads/{thread_id}");
    client
        .send_json::<Thread>(|c| c.request(Method::GET, &path))
        .await
}

pub async fn reply(client: &ApiClient, thread_id: &str, body: &str) -> Result<Thread, CliError> {
    let path = format!("/feedback/threads/{thread_id}/messages");
    let payload = serde_json::to_value(MessageBody { body })?;
    client
        .send_json::<Thread>(|c| Ok(c.request(Method::POST, &path)?.json(&payload)))
        .await
}
