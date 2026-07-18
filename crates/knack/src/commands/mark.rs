//! `knack mark <run_id> <succeeded|failed> [--note=…]` — close the agent loop.
//!
//! `status` is positional now (matches the form agent.txt teaches). The
//! legacy `--status=` flag is still accepted as a deprecated synonym so
//! anyone scripting the old form keeps working.
//!
//! Id shorthand (backed by the [`crate::run_state`] registry of runs
//! started from this machine):
//!   * `knack mark last succeeded` — closes the newest unmarked run for
//!     the active backend; no UUID bookkeeping in agent context.
//!   * `knack mark f887 succeeded` — a unique id prefix (≥4 hex chars)
//!     resolves against the same registry, git-style.

use clap::Args;
use serde_json::json;

use crate::api::{runs as api_runs, ApiClient};
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};
use crate::run_state;

#[derive(Debug, Args)]
pub struct MarkArgs {
    /// Run id — a full UUID, a unique prefix (≥4 chars), or `last` (the
    /// newest unmarked run started from this machine). `knack run`
    /// prints the full id on every invocation. Pass a comma-separated
    /// list (`a,b,c`) to mark several runs in one call; the same
    /// `--note` / `--output` applies to every id.
    pub run_id: String,

    /// Outcome. Positional — preferred form: `knack mark <run-id> succeeded`.
    /// Optional here because `--status=` is also accepted for backward compat;
    /// at runtime we require exactly one of the two forms. `abandoned`
    /// closes a run without a verdict (excluded from success-rate) —
    /// normally the auto-close in `knack run` does this for you.
    #[arg(value_parser = ["succeeded", "failed", "abandoned"])]
    pub outcome: Option<String>,

    /// Legacy: `--status=succeeded|failed`. Prefer the positional form.
    /// Hidden from --help so new users see the positional form first.
    #[arg(long = "status", value_parser = ["succeeded", "failed", "abandoned"], hide = true, conflicts_with = "outcome")]
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

    /// Self-host only: skip the auto `git push origin main` that follows
    /// the telemetry commit. The local commit still lands so the next
    /// pushed event catches up. `KNACK_AUTO_PUSH=0` is the equivalent
    /// env-level kill switch; either disables the network hop.
    #[arg(long)]
    pub no_push: bool,
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

    let backend_tag = match &client.config.backend {
        BackendMode::Github { .. } => "github",
        _ => "cloud",
    };

    // Resolve id shorthand, then validate, before fanning out N HTTP/git
    // calls. A typo'd `"abc,def"` would have burned two round trips and
    // returned two 404s; local validation short-circuits.
    let mut run_ids: Vec<String> = Vec::new();
    let mut invalid: Vec<String> = Vec::new();
    for piece in args.run_id.split(',') {
        let s = piece.trim();
        if s.is_empty() {
            continue;
        }
        if s.eq_ignore_ascii_case("last") {
            if args.run_id.contains(',') {
                let err = CliError::User {
                    code: "MARK_LAST_IN_LIST".into(),
                    message: "`last` cannot be combined with other run ids".into(),
                    hint: Some("use `knack mark last <status>` on its own".into()),
                };
                emit_err(mode, &err);
                return Err(err);
            }
            match run_state::last_open(backend_tag) {
                Some(entry) => run_ids.push(entry.run_id),
                None => {
                    let err = CliError::User {
                        code: "MARK_NO_OPEN_RUN".into(),
                        message: format!(
                            "no unmarked {backend_tag} run started from this machine"
                        ),
                        hint: Some(
                            "start one with `knack run <slug>`, or pass the full run id \
                             (`knack runs list` shows recent runs)"
                                .into(),
                        ),
                    };
                    emit_err(mode, &err);
                    return Err(err);
                }
            }
            continue;
        }
        if uuid::Uuid::parse_str(s).is_ok() {
            run_ids.push(s.to_string());
            continue;
        }
        if run_state::is_plausible_prefix(s) {
            match run_state::resolve_prefix(s) {
                run_state::PrefixMatch::One(id) => {
                    run_ids.push(id);
                    continue;
                }
                run_state::PrefixMatch::Ambiguous(candidates) => {
                    let err = CliError::User {
                        code: "MARK_AMBIGUOUS_PREFIX".into(),
                        message: format!(
                            "run-id prefix `{s}` matches {} runs: {}",
                            candidates.len(),
                            candidates.join(", ")
                        ),
                        hint: Some("add more characters to make the prefix unique".into()),
                    };
                    emit_err(mode, &err);
                    return Err(err);
                }
                run_state::PrefixMatch::None => {
                    let err = CliError::User {
                        code: "MARK_UNKNOWN_PREFIX".into(),
                        message: format!(
                            "no run started from this machine matches prefix `{s}`"
                        ),
                        hint: Some(
                            "prefixes only resolve against this machine's recent runs; \
                             pass the full run id otherwise"
                                .into(),
                        ),
                    };
                    emit_err(mode, &err);
                    return Err(err);
                }
            }
        }
        invalid.push(s.to_string());
    }
    if !invalid.is_empty() {
        let err = CliError::User {
            code: "MARK_INVALID_RUN_ID".into(),
            message: format!(
                "{} run-id(s) not valid UUIDs: {}",
                invalid.len(),
                invalid.join(", ")
            ),
            hint: Some(
                "run-ids look like `f8877899-cd2a-4749-b38a-ae885a1b417c`; \
                 shorthand: `last`, or a unique prefix of ≥4 hex chars"
                    .into(),
            ),
        };
        emit_err(mode, &err);
        return Err(err);
    }
    if run_ids.is_empty() {
        let err = CliError::User {
            code: "MARK_MISSING_RUN_ID".into(),
            message: "no run ids supplied".into(),
            hint: Some("knack mark <run-id>[,<run-id>...] <status>".into()),
        };
        emit_err(mode, &err);
        return Err(err);
    }

    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        let push = match super::run::resolve_push_flag(local_path, args.no_push) {
            Ok(p) => p,
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        };
        if run_ids.len() == 1 {
            return github_mark(
                &run_ids[0],
                &status,
                note.as_deref(),
                &outputs,
                local_path,
                push,
                mode,
            );
        }
        return github_mark_bulk(&run_ids, &status, note.as_deref(), &outputs, local_path, push, mode);
    }

    if run_ids.len() == 1 {
        return cloud_mark_single(&run_ids[0], &status, note.as_deref(), &client, mode).await;
    }
    cloud_mark_bulk(&run_ids, &status, note.as_deref(), &client, mode).await
}

async fn cloud_mark_single(
    run_id: &str,
    status: &str,
    note: Option<&str>,
    client: &ApiClient,
    mode: OutputMode,
) -> CliResult<()> {
    match api_runs::mark(
        client,
        run_id,
        &api_runs::RunMarkBody {
            status: status.to_string(),
            note: note.map(str::to_string),
        },
    )
    .await
    {
        Ok(run) => {
            run_state::set_marked(std::slice::from_ref(&run.id));
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

/// Concurrency cap for cloud bulk-mark. Tuned to match the server's
/// typical per-user concurrent-request budget — high enough that a
/// 10-id batch finishes in roughly one RTT instead of ten, low enough
/// that a 100-id batch doesn't stampede the API. If a customer hits
/// the limit, surface it later via `KNACK_BULK_CONCURRENCY` env.
const BULK_MARK_CONCURRENCY: usize = 8;

async fn cloud_mark_bulk(
    run_ids: &[String],
    status: &str,
    note: Option<&str>,
    client: &ApiClient,
    mode: OutputMode,
) -> CliResult<()> {
    use futures_util::stream::{self, StreamExt};

    // Fan out N parallel POSTs (capped at BULK_MARK_CONCURRENCY). Each
    // task returns its id + Result so the rolled-up output preserves
    // per-id success/failure breakdown without losing order.
    let results: Vec<(String, Result<(), String>)> = stream::iter(run_ids.iter().cloned())
        .map(|id| {
            let body = api_runs::RunMarkBody {
                status: status.to_string(),
                note: note.map(str::to_string),
            };
            async move {
                let outcome = api_runs::mark(client, &id, &body)
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("{e}"));
                (id, outcome)
            }
        })
        .buffer_unordered(BULK_MARK_CONCURRENCY)
        .collect()
        .await;

    let mut succeeded: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    for (id, outcome) in results {
        match outcome {
            Ok(()) => succeeded.push(id),
            Err(why) => failed.push((id, why)),
        }
    }
    run_state::set_marked(&succeeded);
    let all_ok = failed.is_empty();
    emit_ok(
        mode,
        json!({
            "status": status,
            "note": note,
            "marked": succeeded,
            "failed": failed.iter().map(|(id, why)| json!({"run_id": id, "error": why})).collect::<Vec<_>>(),
            "total": run_ids.len(),
            "ok": all_ok,
        }),
        || {
            println!("marked {} run(s) {}", succeeded.len(), status);
            for (id, why) in &failed {
                println!("  ! {}: {}", id, why);
            }
        },
    );
    if all_ok {
        Ok(())
    } else {
        Err(CliError::Partial {
            message: format!(
                "{} of {} marks succeeded; {} failed",
                succeeded.len(),
                run_ids.len(),
                failed.len()
            ),
            succeeded: succeeded.len(),
            failed: failed.len(),
        })
    }
}

fn github_mark_bulk(
    run_ids: &[String],
    status: &str,
    note: Option<&str>,
    outputs: &[String],
    local_path: &std::path::Path,
    push: bool,
    mode: OutputMode,
) -> CliResult<()> {
    let mut marked: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    for id in run_ids {
        match knack_backend_github::mark_run(local_path, id, status, note, outputs, push) {
            Ok(_) => marked.push(id.clone()),
            Err(e) => failed.push((id.clone(), format!("{e}"))),
        }
    }
    run_state::set_marked(&marked);
    let all_ok = failed.is_empty();
    emit_ok(
        mode,
        json!({
            "status": status,
            "note": note,
            "marked": marked,
            "failed": failed.iter().map(|(id, why)| json!({"run_id": id, "error": why})).collect::<Vec<_>>(),
            "total": run_ids.len(),
            "ok": all_ok,
            "backend": "github",
        }),
        || {
            println!("marked {} run(s) {}", marked.len(), status);
            for (id, why) in &failed {
                println!("  ! {}: {}", id, why);
            }
        },
    );
    if all_ok {
        Ok(())
    } else {
        Err(CliError::Partial {
            message: format!(
                "{} of {} marks succeeded; {} failed",
                marked.len(),
                run_ids.len(),
                failed.len()
            ),
            succeeded: marked.len(),
            failed: failed.len(),
        })
    }
}

fn github_mark(
    run_id: &str,
    status: &str,
    note: Option<&str>,
    outputs: &[String],
    local_path: &std::path::Path,
    push: bool,
    mode: OutputMode,
) -> CliResult<()> {
    match knack_backend_github::mark_run(local_path, run_id, status, note, outputs, push) {
        Ok(snapshot) => {
            run_state::set_marked(std::slice::from_ref(&snapshot.run_id));
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
