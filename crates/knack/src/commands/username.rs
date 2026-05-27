//! `knack username <name>` — claim a marketplace username.
//!
//! Usernames are one-time server-side. The CLI surfaces a clean message when
//! you try to change one already claimed (409). Idempotent re-claim of the
//! same value is a no-op.

use clap::Args;
use serde_json::json;

use crate::api::{users as api_users, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct UsernameArgs {
    /// Username to claim. 3-32 chars, lowercase letters / digits / `-` / `_`,
    /// must start with a letter or digit. Case-insensitive; stored
    /// lowercased.
    pub username: String,
}

pub async fn run(args: UsernameArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let candidate = args.username.trim().to_string();
    if candidate.is_empty() {
        let err = CliError::User {
            code: "USERNAME_EMPTY".into(),
            message: "username cannot be empty".into(),
            hint: None,
        };
        emit_err(mode, &err);
        return Err(err);
    }

    // Availability pre-check is mandatory. Usernames are permanent, so a
    // typo or a stale binding would lock the caller in forever; we never
    // want to ship a flag that skips this gate.
    match api_users::check_username(&client, &candidate).await {
        Ok(avail) => {
            if !avail.available {
                let reason = avail.reason.unwrap_or_else(|| "UNAVAILABLE".to_string());
                let err = CliError::User {
                    code: reason.clone(),
                    message: format!("username `{candidate}` is not available ({reason})"),
                    hint: Some("pick a different name (3-32 chars, [a-z0-9_-])".into()),
                };
                emit_err(mode, &err);
                return Err(err);
            }
        }
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    }

    match api_users::put_my_username(&client, &candidate).await {
        Ok(read) => {
            let final_username = read.username.unwrap_or_default();
            emit_ok(
                mode,
                json!({
                    "username": final_username,
                    "permanent": true,
                }),
                || {
                    println!("✓ claimed @{final_username} (permanent — no future changes)");
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
