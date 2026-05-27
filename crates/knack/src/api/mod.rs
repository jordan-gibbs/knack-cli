//! HTTP client for the Knack API.
//!
//! Wraps `reqwest::Client` with three responsibilities:
//!  - Inject `Authorization: Bearer <access>` from the [`TokenStore`]
//!  - Translate `{ ok: false, error: { code, message } }` envelopes into [`CliError`]
//!  - Refresh tokens transparently on a single 401, re-attempting once
//!
//! Lower-level resource modules (`auth`, `skills`, `runs`) take a reference to
//! this client.

pub mod auth;
pub mod cloud;
pub mod feedback;
pub mod folders;
pub mod marketplace;
pub mod runs;
pub mod skills;
pub mod teams;
pub mod users;

pub use cloud::CloudBackend;

use std::sync::{Arc, OnceLock};

use reqwest::{Client, Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth_store::{StoredCredential, TokenStore};
use crate::config::Config;
use crate::errors::CliError;

/// One-shot guard so the X-Knack-Notices banner prints at most once per
/// process invocation, no matter how many API calls a single command
/// fires. Re-running the binary clears it naturally.
static NOTICES_BANNER_PRINTED: OnceLock<()> = OnceLock::new();

/// One-time per-process stderr nudge fired when [`ApiClient`] reads a
/// credential from the legacy keyring fallback instead of the new file
/// store. Tells the user to re-run `knack auth login` to migrate.
/// Stderr (so `--json` stdout stays clean), no formatting deps, no-op
/// after the first call.
fn nudge_keyring_fallback_once() {
    if LEGACY_NUDGE.set(()).is_ok() {
        eprintln!(
            "knack: using legacy keyring credential. Run `knack auth login` to upgrade to \
             a portable file-backed token that works across all your shells (including \
             sandboxed agents)."
        );
    }
}

/// Inspect a response for `X-Knack-Notices` and print a one-line stderr
/// banner if the server flagged "feedback". The banner is intentionally
/// quiet: stderr only (so `--json` stdout is untouched), once per
/// process, and silent when the header is missing or empty.
///
/// The header is a comma-separated list of notice tokens so the server
/// can layer in future ones (`feedback,deprecated_cli`, …) without
/// breaking older CLIs.
fn maybe_print_notices_banner(headers: &reqwest::header::HeaderMap) {
    let Some(value) = headers.get("X-Knack-Notices") else {
        return;
    };
    let Ok(text) = value.to_str() else {
        return;
    };
    let tokens: Vec<&str> = text
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if tokens.iter().any(|t| *t == "feedback") && NOTICES_BANNER_PRINTED.set(()).is_ok() {
        // Stderr only — `--json` consumers reading stdout never see it.
        // Format is plain ASCII so it renders sanely in dumb terminals.
        eprintln!(
            "knack: you have unread replies from staff. run `knack feedback list` to see them."
        );
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ApiErrorBody {
    error: ApiErrorObject,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiErrorObject {
    code: String,
    message: String,
    #[serde(default)]
    details: Option<Value>,
}

/// Try every shape we recognize, falling back to raw text.
///
/// Production servers wrap errors in `{"error": {"code", "message"}}`, but
/// FastAPI's built-in Pydantic 422s return `{"detail": [...]}`, and proxies
/// or rate-limiters can return plain text. Whatever we find, surface it to
/// the user instead of swallowing it as "no envelope".
fn map_api_error_bytes(status: StatusCode, bytes: Option<&[u8]>) -> CliError {
    let Some(bytes) = bytes else {
        return map_api_error(status, None);
    };
    if let Ok(env) = serde_json::from_slice::<ApiErrorBody>(bytes) {
        return map_api_error(status, Some(env));
    }
    if let Ok(detail) = serde_json::from_slice::<FastApiValidationBody>(bytes) {
        let msg = detail.format();
        return map_api_error(
            status,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "VALIDATION_ERROR".into(),
                    message: msg,
                    details: None,
                },
            }),
        );
    }
    let text = std::str::from_utf8(bytes).unwrap_or("<binary body>").trim();
    let snippet = if text.len() > 500 {
        format!("{}…", &text[..500])
    } else {
        text.to_string()
    };
    map_api_error(
        status,
        Some(ApiErrorBody {
            error: ApiErrorObject {
                code: "UNKNOWN".into(),
                message: if snippet.is_empty() {
                    format!("server returned {status} with empty body")
                } else {
                    format!("server returned {status}: {snippet}")
                },
                details: None,
            },
        }),
    )
}

/// Render a `details` value into a short user-facing summary. Recognizes:
///
///   - `{"issues": [{"path", "message"}]}` — SKILL_FORMAT_INVALID, VALIDATION_ERROR.
///   - `{"field", "got", "expected", "hint"}` — META_MISMATCH and other
///     identity-cross-check 409s where the actionable info is the diff
///     between what the file said and what the URL said.
///
/// Falls back to no summary for anything else (the headline message has to
/// stand on its own in that case).
fn format_details(details: Option<&Value>) -> Option<String> {
    let details = details?;
    if let Some(issues) = details.get("issues").and_then(|v| v.as_array()) {
        if issues.is_empty() {
            return None;
        }
        let parts: Vec<String> = issues
            .iter()
            .filter_map(|i| {
                let path = i.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let msg = i.get("message").and_then(|v| v.as_str()).unwrap_or("");
                if path.is_empty() && msg.is_empty() {
                    None
                } else if path.is_empty() {
                    Some(msg.to_string())
                } else {
                    Some(format!("{path}: {msg}"))
                }
            })
            .collect();
        if parts.is_empty() {
            return None;
        }
        return Some(format!("issues: {}", parts.join("; ")));
    }

    // META_MISMATCH-style payloads: server told us which field mismatched
    // and what value was expected vs received. Inline both so the agent can
    // see exactly what to fix without re-reading the spec.
    let field = details.get("field").and_then(|v| v.as_str());
    let got = details.get("got").and_then(|v| v.as_str());
    let expected = details.get("expected").and_then(|v| v.as_str());
    let hint = details.get("hint").and_then(|v| v.as_str());
    if field.is_some() || got.is_some() || expected.is_some() || hint.is_some() {
        let mut parts: Vec<String> = Vec::new();
        if let Some(f) = field {
            parts.push(format!("field: {f}"));
        }
        if let Some(g) = got {
            parts.push(format!("got: {g}"));
        }
        if let Some(e) = expected {
            parts.push(format!("expected: {e}"));
        }
        if let Some(h) = hint {
            parts.push(format!("hint: {h}"));
        }
        if !parts.is_empty() {
            return Some(parts.join(" · "));
        }
    }

    None
}

#[derive(Debug, Deserialize)]
struct FastApiValidationBody {
    detail: Vec<FastApiValidationItem>,
}

#[derive(Debug, Deserialize)]
struct FastApiValidationItem {
    #[serde(default)]
    loc: Vec<Value>,
    #[serde(default)]
    msg: String,
    #[serde(default, rename = "type")]
    kind: String,
}

impl FastApiValidationBody {
    fn format(&self) -> String {
        let parts: Vec<String> = self
            .detail
            .iter()
            .map(|i| {
                let loc = i
                    .loc
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        v => v.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                if i.kind.is_empty() {
                    format!("{loc}: {}", i.msg)
                } else {
                    format!("{loc}: {} ({})", i.msg, i.kind)
                }
            })
            .collect();
        if parts.is_empty() {
            "request validation failed".into()
        } else {
            format!("request validation failed. {}", parts.join("; "))
        }
    }
}

/// Translate a status + JSON body into the right [`CliError`] variant.
fn map_api_error(status: StatusCode, body: Option<ApiErrorBody>) -> CliError {
    let (code, message) = match body {
        Some(b) => {
            let mut msg = b.error.message;
            // Fold structured details (e.g. SKILL_FORMAT_INVALID issues) into
            // the message so the user sees the specific fields that failed
            // instead of a generic "skill format validation failed".
            if let Some(extra) = format_details(b.error.details.as_ref()) {
                msg = format!("{msg}. {extra}");
            }
            (b.error.code, msg)
        }
        None => (
            "UNKNOWN".to_string(),
            format!("server returned {status} with no envelope"),
        ),
    };
    match (status.as_u16(), code.as_str()) {
        (401, _) => CliError::AuthFailed(message),
        (403, "PLAN_LIMIT_EXCEEDED") => CliError::PlanLimit {
            message,
            hint: None,
        },
        (403, _) => CliError::User {
            code: "FORBIDDEN".into(),
            message,
            hint: None,
        },
        (404, _) => CliError::NotFound(message),
        (409, _) => CliError::Conflict {
            message,
            hint: None,
        },
        (s, _) if s >= 500 => CliError::Server {
            status: s,
            code,
            message,
        },
        (s, _) => CliError::Server {
            status: s,
            code,
            message,
        },
    }
}

#[derive(Clone)]
pub struct ApiClient {
    pub config: Config,
    pub http: Client,
    /// Primary credential store. 0.5+: file-backed (`FileStore`). Pre-0.5:
    /// keyring. Wired by `build_client` in `commands/mod.rs`.
    pub store: Arc<dyn TokenStore + Send + Sync>,
    /// Optional legacy store consulted only when [`store`] returns `None`.
    /// In production this is a `KeyringStore` so users upgrading from
    /// pre-0.5 don't lose their auth — their next `knack auth login`
    /// migrates them to the file store. Set to `None` in tests that
    /// don't want a real keyring touched.
    pub legacy_store: Option<Arc<dyn TokenStore + Send + Sync>>,
    pub account: String,
    /// Optional override that bypasses the token store entirely (--auth-token).
    pub bearer_override: Option<String>,
}

/// One-process guard so the legacy-keyring deprecation nudge fires at
/// most once per `knack` invocation, no matter how many requests run.
static LEGACY_NUDGE: std::sync::OnceLock<()> = std::sync::OnceLock::new();

impl ApiClient {
    pub fn new(
        config: Config,
        store: Arc<dyn TokenStore + Send + Sync>,
        account: impl Into<String>,
    ) -> Self {
        Self {
            config,
            http: Client::builder()
                .user_agent(concat!("knack-cli/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client should build"),
            store,
            legacy_store: None,
            account: account.into(),
            bearer_override: None,
        }
    }

    pub fn with_bearer_override(mut self, token: Option<String>) -> Self {
        self.bearer_override = token;
        self
    }

    /// Wire a legacy `TokenStore` (typically `KeyringStore`) as a read-only
    /// fallback consulted when the primary store has nothing.
    pub fn with_legacy_store(mut self, legacy: Option<Arc<dyn TokenStore + Send + Sync>>) -> Self {
        self.legacy_store = legacy;
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.api_base, path)
    }

    /// Resolve the bearer for an outgoing request. Order:
    ///
    ///   1. `bearer_override` — explicit `--auth-token` / `KNACK_AUTH_TOKEN`.
    ///   2. Primary `store` (file in production) — the post-0.5 default.
    ///   3. Legacy `legacy_store` (keyring in production) — kept readable so
    ///      pre-0.5 users keep working until they re-run `knack auth login`.
    ///      Prints a one-time stderr nudge when it actually fires.
    fn current_access_token(&self) -> Result<Option<String>, CliError> {
        if let Some(t) = &self.bearer_override {
            return Ok(Some(t.clone()));
        }
        if let Some(cred) = self.store.load(&self.account)? {
            return Ok(Some(cred.token));
        }
        if let Some(legacy) = &self.legacy_store {
            // Discard keyring read errors silently: missing libsecret /
            // missing dbus on a sandbox is normal and shouldn't break the
            // file-backed happy path. Same for "no entry" → just None.
            if let Some(cred) = legacy.load(&self.account).ok().flatten() {
                nudge_keyring_fallback_once();
                return Ok(Some(cred.token));
            }
        }
        Ok(None)
    }

    /// Look at whatever credential is currently in play and return whether
    /// it's a legacy JWT (needs refresh on 401) or a PAT (401 is terminal).
    /// `bearer_override` is treated as PAT-shaped iff it starts with the
    /// PAT prefix; otherwise as JWT for backwards compat with people who
    /// pipe a raw JWT into `KNACK_AUTH_TOKEN`.
    fn current_is_jwt(&self) -> Result<bool, CliError> {
        if let Some(t) = &self.bearer_override {
            return Ok(!t.starts_with("knack_pat_"));
        }
        if let Some(cred) = self.store.load(&self.account)? {
            return Ok(!cred.is_pat());
        }
        if let Some(legacy) = &self.legacy_store {
            if let Some(cred) = legacy.load(&self.account).ok().flatten() {
                return Ok(!cred.is_pat());
            }
        }
        Ok(false)
    }

    /// Build an authenticated request. Returns the builder so callers can add
    /// JSON bodies, query params, etc.
    pub fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, CliError> {
        let mut rb = self.http.request(method, self.url(path));
        if let Some(tok) = self.current_access_token()? {
            rb = rb.bearer_auth(tok);
        }
        Ok(rb)
    }

    /// Build an unauthenticated request — for `/auth/device/start` and `/auth/device/poll`.
    pub fn request_unauth(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, self.url(path))
    }

    /// Send a request and decode JSON, mapping API error envelopes to typed errors.
    /// Retries once after refreshing tokens if the first attempt 401s.
    pub async fn send_json<T: for<'de> Deserialize<'de>>(
        &self,
        build: impl Fn(&Self) -> Result<RequestBuilder, CliError>,
    ) -> Result<T, CliError> {
        let resp = build(self)?.send().await?;
        let status = resp.status();
        if status.is_success() {
            maybe_print_notices_banner(resp.headers());
            return decode_body(resp).await;
        }
        // Refresh once, retry once — only for legacy JWT credentials.
        // PAT 401 means the token was revoked or expired; refresh doesn't
        // help, surface the AUTH_FAILED so the user re-runs `knack auth login`.
        if status == StatusCode::UNAUTHORIZED
            && self.bearer_override.is_none()
            && self.current_is_jwt().unwrap_or(false)
            && self.try_refresh().await.is_ok()
        {
            let resp2 = build(self)?.send().await?;
            let status2 = resp2.status();
            if status2.is_success() {
                maybe_print_notices_banner(resp2.headers());
                return decode_body(resp2).await;
            }
            let bytes2 = resp2.bytes().await.ok();
            return Err(map_api_error_bytes(status2, bytes2.as_deref()));
        }
        let bytes = resp.bytes().await.ok();
        Err(map_api_error_bytes(status, bytes.as_deref()))
    }

    /// Send a request and discard the body (204 / 200 with empty body).
    pub async fn send_empty(
        &self,
        build: impl Fn(&Self) -> Result<RequestBuilder, CliError>,
    ) -> Result<(), CliError> {
        let resp = build(self)?.send().await?;
        let status = resp.status();
        if status.is_success() {
            maybe_print_notices_banner(resp.headers());
            return Ok(());
        }
        // Refresh-retry path is JWT-only; see send_json for rationale.
        if status == StatusCode::UNAUTHORIZED
            && self.bearer_override.is_none()
            && self.current_is_jwt().unwrap_or(false)
            && self.try_refresh().await.is_ok()
        {
            let resp2 = build(self)?.send().await?;
            let status2 = resp2.status();
            if status2.is_success() {
                maybe_print_notices_banner(resp2.headers());
                return Ok(());
            }
            let bytes2 = resp2.bytes().await.ok();
            return Err(map_api_error_bytes(status2, bytes2.as_deref()));
        }
        let bytes = resp.bytes().await.ok();
        Err(map_api_error_bytes(status, bytes.as_deref()))
    }

    /// Force a refresh, persist the new pair, and return the seconds until
    /// the new access token expires. Only meaningful for legacy JWT
    /// credentials; PATs don't refresh — they live until revoked.
    pub async fn refresh_tokens(&self) -> Result<i64, CliError> {
        self.try_refresh().await?;
        let stored = self
            .store
            .load(&self.account)?
            .ok_or(CliError::AuthRequired)?;
        let expires_at = stored
            .expires_at
            .ok_or_else(|| CliError::AuthFailed("refreshed credential has no expiry".into()))?;
        Ok(expires_at - chrono::Utc::now().timestamp())
    }

    async fn try_refresh(&self) -> Result<(), CliError> {
        let Some(stored) = self.store.load(&self.account)? else {
            return Err(CliError::AuthRequired);
        };
        // PATs don't refresh — a 401 means revoked or expired, which is
        // terminal. Caller should surface AUTH_FAILED with a hint to
        // re-run `knack auth login`.
        if stored.is_pat() {
            return Err(CliError::AuthRequired);
        }
        let Some(refresh) = stored.refresh_token.as_deref() else {
            return Err(CliError::AuthRequired);
        };
        let body = serde_json::json!({ "refresh_token": refresh });
        let resp = self
            .http
            .post(self.url("/auth/refresh"))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            // Refresh failed → wipe the stale tokens so the user is prompted.
            let _ = self.store.clear(&self.account);
            return Err(CliError::AuthRequired);
        }

        #[derive(Deserialize)]
        struct RefreshResp {
            access_token: String,
            refresh_token: String,
            expires_in: i64,
        }
        let r: RefreshResp = resp.json().await?;
        let new = StoredCredential {
            token: r.access_token,
            token_id: None,
            prefix: None,
            refresh_token: Some(r.refresh_token),
            expires_at: Some(chrono::Utc::now().timestamp() + r.expires_in),
            label: None,
            user_id: stored.user_id.clone(),
            email: stored.email.clone(),
        };
        self.store.save(&self.account, &new)?;
        Ok(())
    }
}

async fn decode_body<T: for<'de> Deserialize<'de>>(resp: reqwest::Response) -> Result<T, CliError> {
    let bytes = resp.bytes().await?;
    if bytes.is_empty() {
        // Some endpoints return 200 with empty body; let serde_json handle null
        // for option-typed fields by trying "null".
        return serde_json::from_slice::<T>(b"null").map_err(CliError::from);
    }
    serde_json::from_slice::<T>(&bytes).map_err(CliError::from)
}

/// Page wrapper matching `Page[T]` from the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_401_to_auth_failed() {
        let err = map_api_error(
            StatusCode::UNAUTHORIZED,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "AUTH_REQUIRED".into(),
                    message: "no token".into(),
                    details: None,
                },
            }),
        );
        matches!(err, CliError::AuthFailed(_));
    }

    #[test]
    fn maps_403_plan_limit_to_plan_variant() {
        let err = map_api_error(
            StatusCode::FORBIDDEN,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "PLAN_LIMIT_EXCEEDED".into(),
                    message: "too many".into(),
                    details: None,
                },
            }),
        );
        matches!(err, CliError::PlanLimit { .. });
    }

    #[test]
    fn maps_404_to_not_found() {
        let err = map_api_error(
            StatusCode::NOT_FOUND,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "NOT_FOUND".into(),
                    message: "missing".into(),
                    details: None,
                },
            }),
        );
        matches!(err, CliError::NotFound(_));
    }

    #[test]
    fn maps_409_to_conflict() {
        let err = map_api_error(
            StatusCode::CONFLICT,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "IMMUTABLE_VERSION".into(),
                    message: "exists".into(),
                    details: None,
                },
            }),
        );
        matches!(err, CliError::Conflict { .. });
    }

    #[test]
    fn maps_5xx_to_server() {
        let err = map_api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some(ApiErrorBody {
                error: ApiErrorObject {
                    code: "X".into(),
                    message: "boom".into(),
                    details: None,
                },
            }),
        );
        matches!(err, CliError::Server { .. });
    }
}
