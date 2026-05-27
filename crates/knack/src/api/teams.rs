//! Team management endpoints (Track H).

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::api::ApiClient;
use crate::errors::CliError;

#[derive(Debug, Clone, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub plan: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Invite {
    pub id: String,
    pub team_id: String,
    pub email: String,
    pub role: String,
    pub status: String,
    pub invite_token: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreateBody<'a> {
    name: &'a str,
    slug: &'a str,
}

#[derive(Debug, Serialize)]
struct InviteBody<'a> {
    email: &'a str,
    role: &'a str,
}

#[derive(Debug, Serialize)]
struct AcceptBody<'a> {
    invite_token: &'a str,
}

#[derive(Debug, Serialize)]
struct RoleBody<'a> {
    role: &'a str,
}

pub async fn list_my(client: &ApiClient) -> Result<Vec<Team>, CliError> {
    client
        .send_json::<Vec<Team>>(|c| c.request(Method::GET, "/teams"))
        .await
}

pub async fn create(client: &ApiClient, name: &str, slug: &str) -> Result<Team, CliError> {
    let body = serde_json::to_value(CreateBody { name, slug })?;
    client
        .send_json::<Team>(|c| Ok(c.request(Method::POST, "/teams")?.json(&body)))
        .await
}

pub async fn get(client: &ApiClient, team_id: &str) -> Result<Team, CliError> {
    let path = format!("/teams/{team_id}");
    client
        .send_json::<Team>(|c| c.request(Method::GET, &path))
        .await
}

pub async fn invite(
    client: &ApiClient,
    team_id: &str,
    email: &str,
    role: &str,
) -> Result<Invite, CliError> {
    let path = format!("/teams/{team_id}/invites");
    let body = serde_json::to_value(InviteBody { email, role })?;
    client
        .send_json::<Invite>(|c| Ok(c.request(Method::POST, &path)?.json(&body)))
        .await
}

pub async fn accept(client: &ApiClient, invite_token: &str) -> Result<Team, CliError> {
    let body = serde_json::to_value(AcceptBody { invite_token })?;
    client
        .send_json::<Team>(|c| {
            Ok(c.request(Method::POST, "/teams/invites/accept")?
                .json(&body))
        })
        .await
}

pub async fn set_role(
    client: &ApiClient,
    team_id: &str,
    user_id: &str,
    role: &str,
) -> Result<Team, CliError> {
    let path = format!("/teams/{team_id}/memberships/{user_id}");
    let body = serde_json::to_value(RoleBody { role })?;
    client
        .send_json::<Team>(|c| Ok(c.request(Method::PATCH, &path)?.json(&body)))
        .await
}
