//! `knack auth` — login (device flow), logout, status.

use std::time::Duration;

use clap::Subcommand;
use serde_json::json;
use tokio::time::sleep;

use crate::api::auth as api_auth;
use crate::api::ApiClient;
use crate::auth_store::StoredCredential;
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{chatter, emit_err, emit_ok, OutputMode};

#[derive(Debug, Subcommand)]
pub enum AuthCmd {
    /// Sign in via the browser (OAuth 2.0 device flow). Use --start / --poll
    /// for sandboxed agents that can't keep a long-running process alive.
    Login(LoginArgs),
    /// Revoke the current session and forget the local tokens
    Logout,
    /// Print the current authenticated user + token expiry
    Status,
    /// Proactively refresh the access token (long-running agents). Idempotent.
    Refresh,
}

#[derive(Debug, clap::Args)]
pub struct LoginArgs {
    /// Don't try to open the browser; just print the URL.
    #[arg(long)]
    pub no_browser: bool,

    /// Stateless mode: print the device_code + verification_uri as JSON and
    /// exit immediately. Pair with `--poll <device_code>` to check status.
    /// Use this only when you genuinely can't keep a tool call alive for the
    /// device-code TTL (~5 min). For most agents the default blocking flow is
    /// simpler — it opens the browser, polls in-process, and returns when the
    /// user approves. Reach for `--start` for headless CI or for sandboxes
    /// that hard-cap subprocess lifetime below the device-code TTL.
    #[arg(long, conflicts_with = "poll")]
    pub start: bool,

    /// Stateless mode: run a single poll against the device flow. On
    /// `approved`, mints a Personal Access Token and saves it to
    /// `~/.knack/auth.json`. Re-run the same command repeatedly (e.g.
    /// between agent turns) until the response reports `approved` or
    /// `expired`.
    #[arg(long, value_name = "DEVICE_CODE")]
    pub poll: Option<String>,

    /// Override the auto-generated PAT label. Defaults to
    /// `knack-cli@<hostname>`. Visible in
    /// `getknack.ai/app/settings#cli-tokens`.
    #[arg(long)]
    pub label: Option<String>,

    /// Expire the PAT after N days. Default: never expires. Pass a
    /// positive integer to opt into rotation; the CLI will surface an
    /// AuthRequired one day before expiry.
    #[arg(long, value_name = "DAYS")]
    pub expires_in_days: Option<i64>,
}

pub async fn run(cmd: AuthCmd, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    // In github self-host mode the cloud auth surface is meaningless. Route
    // status to a backend-aware printer and turn login/refresh into clean
    // no-ops so an agent following the playbook doesn't get stuck on
    // AUTH_REQUIRED. Logout still runs through the cloud path so a stale
    // pre-migration token gets cleared.
    if let BackendMode::Github {
        owner,
        repo,
        local_path,
    } = &client.config.backend
    {
        match cmd {
            AuthCmd::Login(_) => return github_login_noop(mode),
            AuthCmd::Status => {
                return github_status(owner, repo, local_path, mode);
            }
            AuthCmd::Refresh => return github_refresh_noop(mode),
            AuthCmd::Logout => { /* fall through, clear any cloud token */ }
        }
    }
    match cmd {
        AuthCmd::Login(a) => {
            if a.start {
                login_start(client, mode).await
            } else if let Some(code) = a.poll.clone() {
                login_poll(a, client, mode, &code).await
            } else {
                login(a, client, mode).await
            }
        }
        AuthCmd::Logout => logout(client, mode).await,
        AuthCmd::Status => status(client, mode).await,
        AuthCmd::Refresh => refresh(client, mode).await,
    }
}

fn github_login_noop(mode: OutputMode) -> CliResult<()> {
    emit_ok(
        mode,
        json!({
            "backend": "github",
            "needs_signin": false,
            "message": "self-host mode does not require sign-in",
        }),
        || {
            println!("self-host mode does not require sign-in.");
            println!();
            println!("github auth is handled by `gh auth login` (already set up if `knack init --self-host` succeeded).");
            println!(
                "to switch to Knack Cloud, run `knack init --cloud` and then `knack auth login`."
            );
        },
    );
    Ok(())
}

fn github_refresh_noop(mode: OutputMode) -> CliResult<()> {
    emit_ok(
        mode,
        json!({
            "backend": "github",
            "refreshed": false,
            "message": "nothing to refresh in self-host mode",
        }),
        || {
            println!("self-host mode does not maintain a refreshable session.");
        },
    );
    Ok(())
}

fn github_status(
    owner: &str,
    repo: &str,
    local_path: &std::path::Path,
    mode: OutputMode,
) -> CliResult<()> {
    let gh_user = crate::commands::init::resolve_gh_user_opt();
    let clone_present = local_path.join(".git").exists();
    emit_ok(
        mode,
        json!({
            "backend": "github",
            "owner": owner,
            "repo": repo,
            "local_path": local_path.display().to_string(),
            "local_clone_present": clone_present,
            "gh_user": gh_user,
            "needs_signin": false,
        }),
        || {
            println!("backend:     github (self-host)");
            println!("repo:        {}/{}", owner, repo);
            println!(
                "local clone: {}{}",
                local_path.display(),
                if clone_present { "" } else { "  (missing)" }
            );
            match &gh_user {
                Some(u) => println!("gh user:     {}", u),
                None => println!("gh user:     (not signed in via gh; run `gh auth login`)"),
            }
        },
    );
    Ok(())
}

/// Stateless step 1: kick off the device flow and return the user-visible
/// code + URL. Exit immediately so the caller (e.g. a sandboxed agent) can
/// hand the URL to the human, wait for them to click approve, then call
/// `--poll <device_code>` repeatedly until the status changes to
/// `approved`. Lives outside any long-lived process.
async fn login_start(client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let start = match api_auth::device_start(&client).await {
        Ok(s) => s,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };
    // Best-effort browser open — same as the blocking flow. Stateless mode
    // is typically used by agents in sandboxes where xdg-open / `open` /
    // `start` may be unavailable; if that's the case the call fails
    // silently and the printed URL is the fallback the human follows.
    let _ = webbrowser::open(&start.verification_uri);
    emit_ok(
        mode,
        json!({
            "device_code": start.device_code,
            "user_code": start.user_code,
            "verification_uri": start.verification_uri,
            "expires_in": start.expires_in,
            "interval": start.interval,
        }),
        || {
            println!("Open {} and approve the code:", start.verification_uri);
            println!("  {}", start.user_code);
            println!(
                "Then run: knack auth login --poll {} (repeat until approved)",
                start.device_code,
            );
        },
    );
    Ok(())
}

/// Hostname for the default PAT label (`knack-cli@<hostname>`). Falls
/// back through Windows `COMPUTERNAME`, Unix `HOSTNAME`/`HOST` env, then
/// "unknown" — no new dep, no fallible system call.
fn host_label() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "unknown".into())
}

/// Convert a freshly-received device-flow JWT into a long-lived PAT and
/// persist it to the primary credential store. Shared by both the
/// blocking `login()` and stateless `login_poll()` flows so the
/// persistence semantics never diverge.
///
/// Safety net: if `POST /me/cli-tokens` returns a token but the local
/// file write fails, best-effort revoke the orphan server-side so the
/// user doesn't end up with an inaccessible row in their token list.
async fn finalize_login_to_pat(
    client: &ApiClient,
    access_jwt: &str,
    label: Option<&str>,
    expires_in_days: Option<i64>,
) -> Result<(api_auth::CreateCliTokenResponse, api_auth::Me), CliError> {
    // Use the JWT as a one-shot bearer to mint the PAT. The JWT itself
    // is never persisted; we discard it as soon as the PAT lands.
    let jwt_client = client.clone().with_bearer_override(Some(access_jwt.into()));

    let owned_label = label
        .map(str::to_owned)
        .unwrap_or_else(|| format!("knack-cli@{}", host_label()));
    let pat = api_auth::create_cli_token(&jwt_client, &owned_label, expires_in_days).await?;

    // Identify the user via /auth/me with the same JWT so we can cache
    // user_id + email alongside the PAT. Best-effort — if /me fails the
    // PAT still works, we just don't have offline identity for `status`.
    let me = api_auth::me(&jwt_client).await.map_err(|e| {
        // /me failure is rare and probably indicates a bigger problem;
        // surface it but only after we've revoked the orphan PAT.
        let pat_id = pat.id.clone();
        let revoke_client = jwt_client.clone();
        tokio::spawn(async move {
            let _ = api_auth::revoke_cli_token(&revoke_client, &pat_id).await;
        });
        e
    })?;

    let cred = StoredCredential {
        token: pat.plaintext.clone(),
        token_id: Some(pat.id.clone()),
        prefix: Some(pat.prefix.clone()),
        refresh_token: None,
        expires_at: pat.expires_at.map(|dt| dt.timestamp()),
        label: Some(pat.name.clone()),
        user_id: Some(me.id.clone()),
        email: Some(me.email.clone()),
    };

    if let Err(e) = client.store.save(&client.account, &cred) {
        // Local persist failed — revoke the server-side row so the user
        // doesn't have a ghost token they can't see.
        let _ = api_auth::revoke_cli_token(&jwt_client, &pat.id).await;
        return Err(e);
    }

    // Best-effort: clear the legacy keyring entry now that the file
    // store has the canonical credential. Means the legacy fallback
    // path stops firing for this account.
    if let Some(legacy) = &client.legacy_store {
        let _ = legacy.clear(&client.account);
    }

    Ok((pat, me))
}

/// Stateless step 2: ask the server once whether the device flow has been
/// approved. If yes, mint a PAT and persist it to `~/.knack/auth.json`.
/// Either way, emit a JSON envelope the caller can branch on
/// (`status` field).
///
/// Exits 0 in all non-network cases — including `authorization_pending`,
/// `slow_down`, `denied`, and `expired` — so the caller doesn't have to
/// distinguish "the CLI broke" from "the user hasn't clicked yet". Bad
/// network or 5xx still propagates as a normal error.
async fn login_poll(
    args: LoginArgs,
    client: ApiClient,
    mode: OutputMode,
    device_code: &str,
) -> CliResult<()> {
    let resp = match api_auth::device_poll(&client, device_code).await {
        Ok(r) => r,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };
    use api_auth::PollStatus;
    let status_str = match resp.status {
        PollStatus::AuthorizationPending => "authorization_pending",
        PollStatus::SlowDown => "slow_down",
        PollStatus::Denied => "denied",
        PollStatus::Expired => "expired",
        PollStatus::Approved => "approved",
    };

    let mut approved_email: Option<String> = None;
    let mut approved_prefix: Option<String> = None;

    if matches!(resp.status, PollStatus::Approved) {
        let access = resp.access_token.clone().unwrap_or_default();
        if access.is_empty() {
            let err = CliError::AuthFailed("server omitted access token".into());
            emit_err(mode, &err);
            return Err(err);
        }
        match finalize_login_to_pat(
            &client,
            &access,
            args.label.as_deref(),
            args.expires_in_days,
        )
        .await
        {
            Ok((pat, me)) => {
                approved_email = Some(me.email);
                approved_prefix = Some(pat.prefix);
            }
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        }
    }

    emit_ok(
        mode,
        json!({
            "status": status_str,
            "approved": matches!(resp.status, PollStatus::Approved),
            "email": approved_email,
            "token_prefix": approved_prefix,
        }),
        || match resp.status {
            PollStatus::Approved => println!(
                "✓ approved — PAT saved to ~/.knack/auth.json. Persists across all future shells."
            ),
            PollStatus::AuthorizationPending => {
                println!("waiting for approval — re-run --poll in a few seconds")
            }
            PollStatus::SlowDown => {
                println!("slow down — poll interval too tight, wait longer")
            }
            PollStatus::Denied => println!("approval denied"),
            PollStatus::Expired => println!("device code expired — run --start again"),
        },
    );
    Ok(())
}

async fn login(args: LoginArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let start = match api_auth::device_start(&client).await {
        Ok(s) => s,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    chatter(mode, format!("Opening {} ...", start.verification_uri));
    chatter(
        mode,
        format!(
            "If a browser doesn't open, visit it manually. Code: {}",
            start.user_code
        ),
    );

    if !args.no_browser {
        // webbrowser::open is synchronous and best-effort; failure isn't fatal.
        let _ = webbrowser::open(&start.verification_uri);
    } else {
        // In --no-browser mode the URL needs to be visible even if we're in
        // --quiet, so write it to stderr unconditionally.
        eprintln!("{}", start.verification_uri);
    }

    chatter(mode, "Waiting for browser approval...");

    let interval = Duration::from_secs(start.interval.max(1));
    let deadline =
        std::time::Instant::now() + Duration::from_secs(start.expires_in.clamp(60, 3600) as u64);

    loop {
        sleep(interval).await;
        if std::time::Instant::now() >= deadline {
            let err = CliError::AuthFailed("device code expired".into());
            emit_err(mode, &err);
            return Err(err);
        }

        let resp = match api_auth::device_poll(&client, &start.device_code).await {
            Ok(r) => r,
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        };

        use api_auth::PollStatus;
        match resp.status {
            PollStatus::AuthorizationPending => continue,
            PollStatus::SlowDown => {
                sleep(interval).await;
                continue;
            }
            PollStatus::Denied => {
                let err = CliError::AuthFailed("approval denied".into());
                emit_err(mode, &err);
                return Err(err);
            }
            PollStatus::Expired => {
                let err = CliError::AuthFailed("device code expired".into());
                emit_err(mode, &err);
                return Err(err);
            }
            PollStatus::Approved => {
                let access = resp.access_token.unwrap_or_default();
                if access.is_empty() {
                    let err = CliError::AuthFailed("server omitted access token".into());
                    emit_err(mode, &err);
                    return Err(err);
                }
                let (pat, me) = match finalize_login_to_pat(
                    &client,
                    &access,
                    args.label.as_deref(),
                    args.expires_in_days,
                )
                .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        emit_err(mode, &e);
                        return Err(e);
                    }
                };
                emit_ok(
                    mode,
                    json!({
                        "user_id": me.id,
                        "email": me.email,
                        "name": me.name,
                        "plan": me.plan,
                        "account": client.account,
                        "token_id": pat.id,
                        "token_prefix": pat.prefix,
                        "expires_at": pat.expires_at,
                    }),
                    || {
                        println!(
                            "✓ logged in as {} ({}). Token saved to ~/.knack/auth.json.",
                            me.email, me.plan
                        );
                        println!(
                            "  Persists across all shells and sandboxed agents on this machine."
                        );
                    },
                );
                return Ok(());
            }
        }
    }
}

async fn logout(client: ApiClient, mode: OutputMode) -> CliResult<()> {
    // Best-effort server-side revoke, then wipe both stores locally
    // regardless of network outcome. Order matters: try the revoke
    // before clearing the local credential so the bearer we send is
    // the one we're about to invalidate (cheap correctness — server
    // doesn't actually need it, but `_from_pat` looks neat in logs).
    let stored = client.store.load(&client.account)?;
    if let Some(cred) = &stored {
        if let Some(token_id) = &cred.token_id {
            // PAT path: hit DELETE /me/cli-tokens/{id}.
            let _ = api_auth::revoke_cli_token(&client, token_id).await;
        } else if let Some(refresh) = cred.refresh_token.as_deref() {
            // Legacy JWT path: hit POST /auth/logout with the refresh.
            let _ = api_auth::logout(&client, Some(refresh)).await;
        }
    }
    client.store.clear(&client.account)?;
    // Clear the legacy keyring too — covers users who started on
    // pre-0.5 and have a stale entry sitting there.
    if let Some(legacy) = &client.legacy_store {
        let _ = legacy.clear(&client.account);
    }

    emit_ok(
        mode,
        json!({ "account": client.account, "logged_out": true }),
        || {
            println!("logged out.");
        },
    );
    Ok(())
}

async fn status(client: ApiClient, mode: OutputMode) -> CliResult<()> {
    // Five possible bearer sources, resolved in the same order
    // ApiClient::current_access_token uses. We only surface info that's
    // relevant to whichever wins; the others are hidden so the output
    // doesn't lie about what's authenticating the request.
    let (source, prefix, expires_in_secs, token_id) = match resolve_status_source(&client)? {
        Some(info) => info,
        None => {
            let err = CliError::AuthRequired;
            emit_err(mode, &err);
            return Err(err);
        }
    };

    match api_auth::me(&client).await {
        Ok(me) => {
            emit_ok(
                mode,
                json!({
                    "user_id": me.id,
                    "email": me.email,
                    "plan": me.plan,
                    "account": client.account,
                    "auth_method": me.auth_method,
                    "bearer_source": source.as_str(),
                    "token_prefix": prefix,
                    "token_id": token_id,
                    "token_expires_in_seconds": expires_in_secs,
                }),
                || {
                    let suffix = match source {
                        BearerSource::PatFile => ", token via ~/.knack/auth.json (manage at \
                             getknack.ai/app/settings#cli-tokens)"
                            .to_string(),
                        BearerSource::PatEnv => {
                            ", via KNACK_AUTH_TOKEN env (personal access token)".to_string()
                        }
                        BearerSource::JwtEnv => ", via KNACK_AUTH_TOKEN env (JWT)".to_string(),
                        BearerSource::JwtFile => match expires_in_secs {
                            Some(s) if s > 0 => {
                                format!(", JWT valid for {}", human_duration(s))
                            }
                            Some(_) => ", JWT expired (refresh on next call)".into(),
                            None => ", JWT (no expiry recorded)".into(),
                        },
                        BearerSource::KeyringJwt => match expires_in_secs {
                            Some(s) if s > 0 => format!(
                                ", legacy keyring JWT valid for {} (re-run \
                                 `knack auth login` to upgrade)",
                                human_duration(s)
                            ),
                            _ => {
                                ", legacy keyring JWT (re-run `knack auth login` to upgrade)".into()
                            }
                        },
                    };
                    println!("{} ({}){}", me.email, me.plan, suffix);
                    // Refresh nudge only meaningful for refreshable
                    // credentials nearing expiry.
                    if matches!(source, BearerSource::JwtFile | BearerSource::KeyringJwt) {
                        if let Some(s) = expires_in_secs {
                            if s > 0 && s < 86_400 {
                                println!("    proactively refresh with `knack auth refresh`");
                            }
                        }
                    }
                },
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum BearerSource {
    /// Primary credential at `~/.knack/auth.json`, PAT-shaped.
    PatFile,
    /// Primary credential at `~/.knack/auth.json`, JWT-shaped. Only
    /// happens if a user hand-edited the file. Surfaced for completeness.
    JwtFile,
    /// Env override (`KNACK_AUTH_TOKEN` / `--auth-token`), PAT-shaped.
    PatEnv,
    /// Env override, JWT-shaped (pasted into CI by hand).
    JwtEnv,
    /// Legacy keyring read. Fires the deprecation nudge on the way in.
    KeyringJwt,
}

impl BearerSource {
    fn as_str(self) -> &'static str {
        match self {
            BearerSource::PatFile => "pat_file",
            BearerSource::JwtFile => "jwt_file",
            BearerSource::PatEnv => "pat_env",
            BearerSource::JwtEnv => "jwt_env",
            BearerSource::KeyringJwt => "keyring_jwt",
        }
    }
}

/// Walk the same resolution order as `ApiClient::current_access_token`
/// and pull out display metadata for `status` to surface. Returns
/// `None` only when no credential exists anywhere.
fn resolve_status_source(
    client: &ApiClient,
) -> Result<Option<(BearerSource, Option<String>, Option<i64>, Option<String>)>, CliError> {
    if let Some(t) = &client.bearer_override {
        let source = if t.starts_with("knack_pat_") {
            BearerSource::PatEnv
        } else {
            BearerSource::JwtEnv
        };
        let prefix = pat_display_prefix(t);
        let expires = if matches!(source, BearerSource::JwtEnv) {
            jwt_exp_seconds_from_now(t)
        } else {
            None
        };
        return Ok(Some((source, prefix, expires, None)));
    }
    if let Some(cred) = client.store.load(&client.account)? {
        if cred.is_pat() {
            return Ok(Some((
                BearerSource::PatFile,
                cred.prefix.or_else(|| pat_display_prefix(&cred.token)),
                cred.expires_at.map(|t| t - chrono::Utc::now().timestamp()),
                cred.token_id,
            )));
        } else {
            return Ok(Some((
                BearerSource::JwtFile,
                None,
                jwt_exp_seconds_from_now(&cred.token),
                None,
            )));
        }
    }
    if let Some(legacy) = &client.legacy_store {
        if let Some(cred) = legacy.load(&client.account).ok().flatten() {
            return Ok(Some((
                BearerSource::KeyringJwt,
                None,
                jwt_exp_seconds_from_now(&cred.token),
                None,
            )));
        }
    }
    Ok(None)
}

fn pat_display_prefix(token: &str) -> Option<String> {
    if !token.starts_with("knack_pat_") {
        return None;
    }
    let cut = token
        .char_indices()
        .nth(16)
        .map(|(i, _)| i)
        .unwrap_or(token.len());
    Some(token[..cut].to_string())
}

async fn refresh(client: ApiClient, mode: OutputMode) -> CliResult<()> {
    // PATs don't have a refresh dance — they live until revoked or
    // expired. Surface that clearly so scripted callers don't break,
    // and so humans don't wait for a server roundtrip that does nothing.
    if let Some(cred) = client.store.load(&client.account)? {
        if cred.is_pat() {
            emit_ok(
                mode,
                json!({
                    "refreshed": false,
                    "reason": "pat_no_refresh_needed",
                    "token_prefix": cred.prefix,
                }),
                || {
                    println!(
                        "✓ using a personal access token; no refresh needed (token persists until revoked)."
                    )
                },
            );
            return Ok(());
        }
    }

    match client.refresh_tokens().await {
        Ok(secs) => {
            emit_ok(mode, json!({ "token_expires_in_seconds": secs }), || {
                println!("✓ refreshed, token valid for {}", human_duration(secs))
            });
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

/// Decode a JWT's payload (middle base64url segment) and return
/// `exp - now` in seconds. Doesn't verify the signature — we trust the
/// token because we just pulled it out of our own keyring. Returns
/// `None` for any decoding failure so callers fall back gracefully.
fn jwt_exp_seconds_from_now(token: &str) -> Option<i64> {
    let payload_b64 = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload_b64)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let exp = value.get("exp")?.as_i64()?;
    Some(exp - chrono::Utc::now().timestamp())
}

/// Minimal base64url decoder (no padding). Avoids pulling a new crate in
/// just for one JWT field.
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity((bytes.len() / 4) * 3 + 2);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        let v = val(b)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// Render a duration like `364d 23h` or `2h 14m` or `45s`.
fn human_duration(seconds: i64) -> String {
    if seconds <= 0 {
        return "0s".to_string();
    }
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3600;
    let mins = (seconds % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m {}s", seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_duration_days_hours() {
        assert_eq!(human_duration(365 * 86_400 + 3 * 3600), "365d 3h");
    }

    #[test]
    fn human_duration_hours_mins() {
        assert_eq!(human_duration(2 * 3600 + 14 * 60), "2h 14m");
    }

    #[test]
    fn human_duration_zero() {
        assert_eq!(human_duration(0), "0s");
        assert_eq!(human_duration(-5), "0s");
    }

    #[test]
    fn base64url_decode_basic() {
        // {"exp": 99} → eyJleHAiOiA5OX0
        assert_eq!(
            base64url_decode("eyJleHAiOiA5OX0"),
            Some(b"{\"exp\": 99}".to_vec())
        );
    }

    #[test]
    fn jwt_exp_decode_with_far_future_exp() {
        // Hand-craft a fake JWT with payload {"exp": <now + 365d>}.
        let future = chrono::Utc::now().timestamp() + 365 * 86_400;
        let payload = serde_json::json!({ "exp": future }).to_string();
        // base64url encode without padding
        let b64 = {
            let mut out = String::new();
            let bytes = payload.as_bytes();
            let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut buf = 0u32;
            let mut bits = 0u32;
            for &b in bytes {
                buf = (buf << 8) | b as u32;
                bits += 8;
                while bits >= 6 {
                    bits -= 6;
                    out.push(alphabet[((buf >> bits) & 0x3f) as usize] as char);
                }
            }
            if bits > 0 {
                out.push(alphabet[((buf << (6 - bits)) & 0x3f) as usize] as char);
            }
            out
        };
        let token = format!("header.{b64}.sig");
        let secs = jwt_exp_seconds_from_now(&token).unwrap();
        // Should be within a few seconds of 365d.
        assert!((secs - 365 * 86_400).abs() < 5, "got {secs}");
    }
}
