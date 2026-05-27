//! `knack run <slug> [--input <path>] [--runtime <tag>]` — register a Run.
//!
//! Telemetry-only by design. The CALLING AGENT (Claude Code, Cursor, Codex,
//! Cowork, etc.) is responsible for actually performing the work: it reads
//! `~/.knack/skills/<slug>/SKILL.md` and follows the procedure inline using
//! its own tools. `knack run` just registers a Run row on the server, prints
//! the resulting `run-id`, and exits. The agent then calls `knack mark
//! <run-id> succeeded|failed` once it's done.
//!
//! Why no shell-out: there's no portable way to inject a skill folder's
//! context into "the agent that's calling us" — by definition that agent
//! already has its own tools and prompt. Trying to dispatch `claude
//! <input.xlsx>` was a v0 placeholder that did not produce real runs and
//! confused agents that read the playbook literally. The right contract is
//! "the agent runs the skill itself; the CLI handles auth + telemetry."
//!
//! Captures: skill version pin (current OR `@<semver>`), input filename,
//! optional inputs_summary, runtime tag (free-form, defaults to "agent").

use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::api::runs as api_runs;
use crate::api::{skills as api_skills, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{chatter, emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Skill identifier — `<slug>`, `<slug>@<semver>`, or `@<author>/<slug>`
    /// (with optional `@<semver>`). When a semver is specified, the run is
    /// attributed to that historical version instead of the current one.
    pub slug: String,

    /// Input file path. Captured as part of the Run's `inputs_summary` so
    /// the telemetry timeline shows what the agent worked on.
    #[arg(long)]
    pub input: Option<PathBuf>,

    /// Free-form tag for the calling agent (e.g. "claude-code", "cursor",
    /// "codex", "cowork"). Stored as metadata on the Run row so multiple
    /// agents can be told apart in stats. Defaults to "agent".
    #[arg(long)]
    pub runtime: Option<String>,

    /// Identifier for the calling agent instance (so multiple sessions of
    /// the same agent can be distinguished). Optional.
    #[arg(long)]
    pub agent_id: Option<String>,

    /// Deprecated no-op. Kept for backward compat with v0.2 callers — every
    /// `knack run` is telemetry-only now.
    #[arg(long, hide = true)]
    pub no_exec: bool,

    /// Deprecated no-op. Equivalent to no_exec above; kept for backward
    /// compat with the v0 `--dry` flag.
    #[arg(long, hide = true, conflicts_with = "no_exec")]
    pub dry: bool,
}

pub async fn run(args: RunArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let (slug, version_filter) = crate::slug::parse_slug_at_version(&args.slug);

    let skill = match api_skills::find_by_slug(&client, slug).await? {
        Some(s) => s,
        None => {
            let err = CliError::NotFound(format!("skill `{slug}` not found"));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    // Resolve which version to pin to. `@semver` overrides the skill's
    // current_version_id, so agents can replay against a stable historical
    // version even after newer versions ship.
    let version_id = match version_filter {
        Some(semver) => match api_skills::get_version(&client, &skill.id, semver).await {
            Ok(v) => v.id,
            Err(CliError::NotFound(_)) => {
                let err = CliError::NotFound(format!(
                    "skill `{slug}` has no version `{semver}`"
                ));
                emit_err(mode, &err);
                return Err(err);
            }
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        },
        None => skill.current_version_id.clone().ok_or_else(|| {
            CliError::NotFound(format!("skill `{slug}` has no published version"))
        })?,
    };

    let runtime = args.runtime.clone().unwrap_or_else(|| "agent".to_string());

    let inputs_summary = args.input.as_ref().map(|p| {
        json!({
            "path": p,
            "filename": p.file_name().and_then(|s| s.to_str()),
        })
    });

    let run = api_runs::start(
        &client,
        &api_runs::RunCreate {
            skill_version_id: version_id,
            agent_id: args.agent_id.clone(),
            runtime: Some(runtime.clone()),
            inputs_summary,
        },
    )
    .await?;

    chatter(
        mode,
        format!(
            "run registered · skill={} · runtime={} — execute the skill, then \
             `knack mark {} succeeded` (or `failed --note \"…\"`)",
            args.slug, runtime, run.id,
        ),
    );

    emit_ok(
        mode,
        json!({
            "run_id": run.id,
            "skill_version_id": run.skill_version_id,
            "runtime": runtime,
        }),
        || {
            println!("✓ run registered · {}", run.id);
            println!("  next: read ~/.knack/skills/{}/SKILL.md and do the work yourself,", args.slug);
            println!("        then `knack mark {} succeeded` (or `failed --note \"…\"`).", run.id);
        },
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    // Behavior tests live in tests/runs.rs (the wiremock integration suite).
    // The command's logic is the API call + a slug parse, both covered there.
}
