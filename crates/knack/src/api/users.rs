//! Username + user-self-service endpoints.

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::api::ApiClient;
use crate::errors::CliError;

#[derive(Debug, Clone, Deserialize)]
pub struct UsernameRead {
    pub username: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsernameAvailability {
    pub candidate: String,
    pub available: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct UsernamePut<'a> {
    username: &'a str,
}

pub async fn get_my_username(client: &ApiClient) -> Result<UsernameRead, CliError> {
    client
        .send_json::<UsernameRead>(|c| c.request(Method::GET, "/me/username"))
        .await
}

pub async fn put_my_username(client: &ApiClient, username: &str) -> Result<UsernameRead, CliError> {
    let body = serde_json::to_value(UsernamePut { username })?;
    client
        .send_json::<UsernameRead>(|c| Ok(c.request(Method::PUT, "/me/username")?.json(&body)))
        .await
}

pub async fn check_username(
    client: &ApiClient,
    candidate: &str,
) -> Result<UsernameAvailability, CliError> {
    let candidate = candidate.to_string();
    client
        .send_json::<UsernameAvailability>(|c| {
            Ok(c.request(Method::GET, "/users/check-username")?
                .query(&[("candidate", candidate.as_str())]))
        })
        .await
}
