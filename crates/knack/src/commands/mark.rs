//! `knack mark <run_id> <succeeded|failed> [--note=…]` — close the agent loop.
//!
//! `status` is positional now (matches the form agent.txt teaches). The
//! legacy `--status=` flag is still accepted as a deprecated synonym so
//! anyone scripting the old form keeps working.

use clap::Args;
use serde_json::json;

use crate::api::{runs as api_runs, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct MarkArgs {
    /// Run id (UUID) — `knack run` prints it on every invocation.
    pub run_id: String,

    /// Outcome. Positional — preferred form: `knack mark <run-id> succeeded`.
    /// Optional here because `--status=` is also accepted for backward compat;
    /// at runtime we require exactly one of the two forms.
    #[arg(value_parser = ["succeeded", "failed"])]
    pub outcome: Option<String>,

    /// Legacy: `--status=succeeded|failed`. Prefer the positional form.
    /// Hidden from --help so new users see the positional form first.
    #[arg(long = "status", value_parser = ["succeeded", "failed"], hide = true, conflicts_with = "outcome")]
    pub status_flag: Option<String>,

    /// Free-form note. For `failed`, the skill author gets notified with this
    /// text — be specific.
    #[arg(long)]
    pub note: Option<String>,

    /// Alias for `--note`. Spec uses `--reason` for failures.
    #[arg(long, conflicts_with = "note")]
    pub reason: Option<String>,
}

pub async fn run(args: MarkArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let status = match args.outcome.or(args.status_flag) {
        Some(s) => s,
        None => {
            let err = CliError::User {
                code: "MARK_MISSING_STATUS".into(),
                message: "missing status".into(),
                hint: Some("knack mark <run-id> succeeded   (or `failed`)".into()),
            };
            emit_err(mode, &err);
            return Err(err);
        }
    };

    let note = args.note.or(args.reason);
    let result = api_runs::mark(
        &client,
        &args.run_id,
        &api_runs::RunMarkBody {
            status: status.clone(),
            note: note.clone(),
        },
    )
    .await;

    match result {
        Ok(run) => {
            emit_ok(
                mode,
                json!({
                    "run_id": run.id,
                    "status": status,
                    "note": note,
                    "marks_count": run.marks.len(),
                }),
                || println!("✓ marked {} {}", run.id, status),
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    // The mark command's logic is mostly the API call; tests are covered by
    // CliError exit-code mapping in errors.rs and the integration tests.
    use crate::errors::CliError;
    use crate::output::err_envelope;

    #[test]
    fn mark_failed_envelope_includes_hint_when_present() {
        let err = CliError::PlanLimit {
            message: "too many marks".into(),
            hint: Some("upgrade".into()),
        };
        let env = err_envelope(&err);
        assert_eq!(env["error"]["code"], "PLAN_LIMIT_EXCEEDED");
        assert_eq!(env["error"]["hint"], "upgrade");
    }
}
