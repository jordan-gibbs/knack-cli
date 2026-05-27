//! `knack mark <run_id> <succeeded|failed> [--note=…]` — close the agent loop.
//!
//! `status` is positional now (matches the form agent.txt teaches). The
//! legacy `--status=` flag is still accepted as a deprecated synonym so
//! anyone scripting the old form keeps working.

use clap::Args;
use serde_json::json;

use crate::api::{runs as api_runs, ApiClient};
use crate::config::BackendMode;
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

    /// Output file path. Repeatable: pass `--output` once per file the run
    /// produced. Captured into the run's `outputs` array in the telemetry
    /// log so a future audit knows what artifacts came out.
    #[arg(long)]
    pub output: Vec<std::path::PathBuf>,
}

pub async fn run(args: MarkArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let status = match args.outcome.clone().or(args.status_flag.clone()) {
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

    let note = args.note.clone().or(args.reason.clone());
    let outputs: Vec<String> = args
        .output
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        return github_mark(
            &args.run_id,
            &status,
            note.as_deref(),
            &outputs,
            local_path,
            mode,
        );
    }
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

fn github_mark(
    run_id: &str,
    status: &str,
    note: Option<&str>,
    outputs: &[String],
    local_path: &std::path::Path,
    mode: OutputMode,
) -> CliResult<()> {
    match knack_backend_github::mark_run(local_path, run_id, status, note, outputs) {
        Ok(snapshot) => {
            emit_ok(
                mode,
                json!({
                    "run_id": snapshot.run_id,
                    "status": snapshot.status,
                    "note": snapshot.note,
                    "skill": snapshot.skill,
                    "version": snapshot.version,
                    "agent": snapshot.agent,
                    "inputs": snapshot.inputs,
                    "outputs": snapshot.outputs,
                    "started_at": snapshot.started_at,
                    "completed_at": snapshot.completed_at,
                    "duration_ms": snapshot.duration_ms,
                    "backend": "github",
                }),
                || {
                    println!("✓ marked {} {}", snapshot.run_id, snapshot.status);
                    if let Some(ms) = snapshot.duration_ms {
                        println!("  duration: {} ms", ms);
                    }
                    if !snapshot.outputs.is_empty() {
                        println!("  outputs: {}", snapshot.outputs.join(", "));
                    }
                    if let Some(n) = &snapshot.note {
                        println!("  note: {n}");
                    }
                },
            );
            Ok(())
        }
        Err(e) => {
            let err = CliError::NotFound(format!("mark run: {e}"));
            emit_err(mode, &err);
            Err(err)
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
