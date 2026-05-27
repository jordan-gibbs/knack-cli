//! Folder management endpoints.
//!
//! Folders organize personal and team skills per-owner. Public skills
//! are never foldered (server enforces this with a CHECK constraint).
//! The CLI's ``folder`` command group calls into here; the in-process
//! ``find_or_create_by_name`` helper underpins ``knack create --folder``
//! and ``knack folder mv`` so users don't have to spell IDs.

use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::api::ApiClient;
use crate::errors::CliError;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Folder {
    pub id: String,
    pub name: String,
    /// "personal" or "team" — synthesized server-side from the XOR
    /// owner columns.
    pub scope: String,
    #[serde(default)]
    pub owner_user_id: Option<String>,
    #[serde(default)]
    pub owner_team_id: Option<String>,
    /// Self-FK for nested folders. ``None`` means this is a root folder
    /// for its owner. Clients can walk parent_folder_id back to the
    /// root to render a breadcrumb without a second round-trip.
    #[serde(default)]
    pub parent_folder_id: Option<String>,
    #[serde(default)]
    pub skill_count: u32,
    /// Live count of direct sub-folders (not recursive). ``None`` when
    /// the server hasn't been upgraded to migration 0022 yet —
    /// ``#[serde(default)]`` keeps the CLI forward-compatible.
    #[serde(default)]
    pub subfolder_count: Option<u32>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct CreateBody<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_team_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_folder_id: Option<&'a str>,
}

/// PATCH body for ``rename``/``reparent``. Both fields are optional;
/// ``parent_folder_id`` is wrapped in ``Option<Option<&str>>`` so callers
/// can distinguish "field omitted" (don't reparent) from "explicit
/// null" (promote to root). When ``Some(None)`` is serialized as
/// ``"parent_folder_id": null`` the server reads it as "move to root".
#[derive(Debug, Serialize, Default)]
struct UpdateBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_folder_id: Option<Option<&'a str>>,
}

pub async fn list(
    client: &ApiClient,
    scope: Option<&str>,
    team_id: Option<&str>,
) -> Result<Vec<Folder>, CliError> {
    let scope = scope.map(str::to_string);
    let team_id = team_id.map(str::to_string);
    client
        .send_json::<Vec<Folder>>(|c| {
            let mut rb = c.request(Method::GET, "/folders")?;
            if let Some(s) = &scope {
                rb = rb.query(&[("scope", s)]);
            }
            if let Some(t) = &team_id {
                rb = rb.query(&[("team_id", t)]);
            }
            Ok(rb)
        })
        .await
}

pub async fn create(
    client: &ApiClient,
    name: &str,
    owner_team_id: Option<&str>,
    parent_folder_id: Option<&str>,
) -> Result<Folder, CliError> {
    let body = serde_json::to_value(CreateBody {
        name,
        owner_team_id,
        parent_folder_id,
    })?;
    client
        .send_json::<Folder>(|c| Ok(c.request(Method::POST, "/folders")?.json(&body)))
        .await
}

pub async fn rename(client: &ApiClient, folder_id: &str, name: &str) -> Result<Folder, CliError> {
    let path = format!("/folders/{folder_id}");
    let body = serde_json::to_value(UpdateBody {
        name: Some(name),
        ..Default::default()
    })?;
    client
        .send_json::<Folder>(|c| Ok(c.request(Method::PATCH, &path)?.json(&body)))
        .await
}

/// Move a folder under a new parent (or to the root when ``new_parent_id``
/// is ``None``). Server-side validation:
///   * new parent must exist
///   * same owner as the folder being moved
///   * no cycles (the new parent must not be a descendant of the folder)
/// Any of those fail → 422 FOLDER_INVARIANT.
pub async fn reparent(
    client: &ApiClient,
    folder_id: &str,
    new_parent_id: Option<&str>,
) -> Result<Folder, CliError> {
    let path = format!("/folders/{folder_id}");
    let body = serde_json::to_value(UpdateBody {
        parent_folder_id: Some(new_parent_id),
        ..Default::default()
    })?;
    client
        .send_json::<Folder>(|c| Ok(c.request(Method::PATCH, &path)?.json(&body)))
        .await
}

pub async fn delete(client: &ApiClient, folder_id: &str) -> Result<(), CliError> {
    let path = format!("/folders/{folder_id}");
    client
        .send_empty(|c| c.request(Method::DELETE, &path))
        .await
}

/// Convenience: case-insensitive lookup by name inside a scope. Returns
/// ``None`` when the folder doesn't exist — callers compose this with
/// ``create`` to implement "find or create" (used by
/// ``knack folder mv`` and the planned ``knack create --folder=X``).
pub async fn find_by_name(
    client: &ApiClient,
    name: &str,
    scope: Option<&str>,
    team_id: Option<&str>,
) -> Result<Option<Folder>, CliError> {
    let needle = name.trim().to_lowercase();
    let all = list(client, scope, team_id).await?;
    Ok(all.into_iter().find(|f| f.name.to_lowercase() == needle))
}

/// Look up a folder by id first, then fall back to name lookup so users
/// can type either at the command line.
pub async fn resolve(
    client: &ApiClient,
    id_or_name: &str,
    scope: Option<&str>,
    team_id: Option<&str>,
) -> Result<Folder, CliError> {
    // UUIDs are 36 chars with four hyphens; this isn't airtight but
    // it's enough to avoid an extra round trip for the common case
    // (folder names are typically short, human-readable words).
    let looks_like_uuid = id_or_name.len() == 36 && id_or_name.matches('-').count() == 4;
    if looks_like_uuid {
        let path = format!("/folders?scope={}", scope.unwrap_or(""));
        let _ = path;
        // The /folders endpoint doesn't expose a by-id getter, but the
        // list endpoint with a stable id filter is overkill for the CLI:
        // we just list and filter.
        let all = list(client, scope, team_id).await?;
        if let Some(f) = all.into_iter().find(|f| f.id == id_or_name) {
            return Ok(f);
        }
    }
    if let Some(f) = find_by_name(client, id_or_name, scope, team_id).await? {
        return Ok(f);
    }
    Err(CliError::NotFound(format!(
        "folder `{}` not found",
        id_or_name
    )))
}
