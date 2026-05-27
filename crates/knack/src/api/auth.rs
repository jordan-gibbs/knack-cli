//! Auth API — device-authorization flow types and calls.

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::api::ApiClient;
use crate::errors::CliError;

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: i64,
    pub interval: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DevicePollRequest<'a> {
    pub device_code: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DevicePollResponse {
    pub status: PollStatus,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollStatus {
    AuthorizationPending,
    Approved,
    Denied,
    Expired,
    SlowDown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Me {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub plan: String,
    pub auth_method: String,
}

pub async fn device_start(client: &ApiClient) -> Result<DeviceAuthStart, CliError> {
    client
        .send_json::<DeviceAuthStart>(|c| Ok(c.request_unauth(Method::POST, "/auth/device/start")))
        .await
}

pub async fn device_poll(
    client: &ApiClient,
    device_code: &str,
) -> Result<DevicePollResponse, CliError> {
    let body = DevicePollRequest { device_code };
    let body = serde_json::to_value(&body)?;
    client
        .send_json::<DevicePollResponse>(|c| {
            Ok(c.request_unauth(Method::POST, "/auth/device/poll")
                .json(&body))
        })
        .await
}

pub async fn me(client: &ApiClient) -> Result<Me, CliError> {
    client
        .send_json::<Me>(|c| c.request(Method::GET, "/auth/me"))
        .await
}

pub async fn logout(client: &ApiClient, refresh_token: Option<&str>) -> Result<(), CliError> {
    let body = serde_json::json!({ "refresh_token": refresh_token });
    client
        .send_empty(|c| Ok(c.request(Method::POST, "/auth/logout")?.json(&body)))
        .await
}

// ─── Personal Access Tokens ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct CreateCliTokenRequest<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in_days: Option<i64>,
}

/// Response from `POST /me/cli-tokens`. Contains the plaintext token —
/// the only time the server will ever give it back. The CLI must persist
/// it immediately or revoke the orphan row.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCliTokenResponse {
    pub id: String,
    pub name: String,
    pub plaintext: String,
    pub prefix: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Mint a long-lived Personal Access Token bound to the caller's user.
/// Requires the client to be holding a valid bearer (JWT or PAT — typically
/// a freshly-minted JWT from the device flow, since the whole point is
/// avoiding the keyring round-trip).
pub async fn create_cli_token(
    client: &ApiClient,
    name: &str,
    expires_in_days: Option<i64>,
) -> Result<CreateCliTokenResponse, CliError> {
    let body = serde_json::to_value(&CreateCliTokenRequest { name, expires_in_days })?;
    client
        .send_json::<CreateCliTokenResponse>(|c| {
            Ok(c.request(Method::POST, "/me/cli-tokens")?.json(&body))
        })
        .await
}

/// Soft-revoke a PAT server-side. Best-effort: callers swallow errors
/// during `knack auth logout` so a network blip doesn't block the local
/// wipe.
pub async fn revoke_cli_token(client: &ApiClient, token_id: &str) -> Result<(), CliError> {
    let path = format!("/me/cli-tokens/{token_id}");
    client
        .send_empty(|c| c.request(Method::DELETE, &path))
        .await
}
