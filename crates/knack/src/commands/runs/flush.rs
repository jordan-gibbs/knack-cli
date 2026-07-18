//! `knack runs flush` — push queued self-host telemetry now.
//!
//! Telemetry commits batch locally (see the push policy in
//! `knack_backend_github::runs`) and normally push once the batch
//! reaches the threshold or the next `knack publish` carries them.
//! This command is the on-demand escape hatch: end of a work session,
//! before handing off to a teammate, before a laptop goes offline.
//!
//! Cloud mode has nothing to flush (telemetry is an API call, never
//! queued) — that's reported as a no-op, not an error, so agents can
//! call it unconditionally.

use clap::Args;
use serde_json::json;

use crate::api::ApiClient;
use crate::config::BackendMode;
use crate::errors::CliResult;
use crate::output::{emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct FlushArgs {}

pub async fn run(_args: FlushArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let BackendMode::Github { local_path, .. } = &client.config.backend else {
        emit_ok(
            mode,
            json!({"backend": "cloud", "pushed": false, "pending": 0}),
            || println!("nothing to flush — cloud telemetry is never queued locally"),
        );
        return Ok(());
    };

    let pending = knack_backend_github::pending_events(local_path);
    match pending {
        Some(0) => {
            emit_ok(
                mode,
                json!({"backend": "github", "pushed": false, "pending": 0}),
                || println!("nothing to flush — no telemetry queued"),
            );
            return Ok(());
        }
        _ => {
            let target = knack_backend_github::resolve_remote(local_path);
            // push_pending reports failures on stderr itself; a failed
            // push leaves the queue intact for the next flush/publish.
            knack_backend_github::push_pending(local_path, &target);
            emit_ok(
                mode,
                json!({
                    "backend": "github",
                    "pushed": true,
                    "pending": pending,
                    "remote": target.remote,
                    "branch": target.branch,
                }),
                || match pending {
                    Some(n) => println!("✓ pushed {n} queued telemetry event(s)"),
                    None => println!("✓ pushed (queued count unknown — no remote-tracking ref)"),
                },
            );
        }
    }
    Ok(())
}
