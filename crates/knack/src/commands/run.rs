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
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{chatter, display_path, emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Skill identifier — `<slug>`, `<slug>@<semver>`, or `@<author>/<slug>`
    /// (with optional `@<semver>`). When a semver is specified, the run is
    /// attributed to that historical version instead of the current one.
    pub slug: String,

    /// Input file path. Repeatable: pass `--input` once per file the agent
    /// will work on. Captured into the run's `inputs` array so the telemetry
    /// timeline shows exactly what was read.
    #[arg(long)]
    pub input: Vec<PathBuf>,

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
    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        return github_run(&args, local_path, mode);
    }

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
                let err = CliError::NotFound(format!("skill `{slug}` has no version `{semver}`"));
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

    // Cloud's `inputs_summary` is a free-form structured field. Pack the
    // (possibly multiple) --input paths into an array of {path, filename}.
    let inputs_summary = if args.input.is_empty() {
        None
    } else {
        Some(json!({
            "files": args
                .input
                .iter()
                .map(|p| json!({
                    "path": p,
                    "filename": p.file_name().and_then(|s| s.to_str()),
                }))
                .collect::<Vec<_>>(),
        }))
    };

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
            println!(
                "  next: read ~/.knack/skills/{}/SKILL.md and do the work yourself,",
                args.slug
            );
            println!(
                "        then `knack mark {} succeeded` (or `failed --note \"…\"`).",
                run.id
            );
        },
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    // Behavior tests live in tests/runs.rs (the wiremock integration suite).
    // The command's logic is the API call + a slug parse, both covered there.
}

fn github_run(args: &RunArgs, local_path: &std::path::Path, mode: OutputMode) -> CliResult<()> {
    let (slug, version_filter) = crate::slug::parse_slug_at_version(&args.slug);

    // Resolve the skill from the local clone and read its current version
    // from meta.knack.yaml. If --slug@version was passed, prefer that.
    let skill_dir = local_path.join("skills").join(slug);
    if !skill_dir.is_dir() {
        let err = CliError::NotFound(format!(
            "skill `{slug}` not found in {}",
            local_path.display()
        ));
        emit_err(mode, &err);
        return Err(err);
    }
    let version = match version_filter {
        Some(v) => v.trim_start_matches('v').to_string(),
        None => read_meta_version(&skill_dir).unwrap_or_else(|_| "0.0.0".to_string()),
    };

    let agent_tag = args.runtime.clone().or_else(|| Some("agent".to_string()));
    let inputs: Vec<String> = args.input.iter().map(|p| p.display().to_string()).collect();

    let run_id = match knack_backend_github::start_run(
        local_path,
        slug,
        &version,
        agent_tag.as_deref(),
        &inputs,
    ) {
        Ok(id) => id,
        Err(e) => {
            let err = CliError::Internal(format!("record run: {e}"));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    let day_file = local_path
        .join("runs")
        .join(format!("{}", chrono::Utc::now().format("%Y-%m")))
        .join(format!("{}.jsonl", chrono::Utc::now().format("%Y-%m-%d")));

    emit_ok(
        mode,
        json!({
            "run_id": run_id.to_string(),
            "slug": slug,
            "version": version,
            "agent": agent_tag,
            "inputs": inputs,
            "status": "started",
            "backend": "github",
            "log_file": day_file.display().to_string(),
        }),
        || {
            println!("✓ {} run-id: {}", slug, run_id);
            println!("  recorded to {}", display_path(&day_file));
            println!();
            println!(
                "close the loop with: knack mark {} succeeded   (or `failed --reason …`)",
                run_id
            );
        },
    );
    Ok(())
}

fn read_meta_version(skill_dir: &std::path::Path) -> Result<String, std::io::Error> {
    let bytes = std::fs::read(skill_dir.join("meta.knack.yaml"))?;
    let parsed: serde_yaml::Value = serde_yaml::from_slice(&bytes)
        .map_err(|e| std::io::Error::other(format!("parse meta: {e}")))?;
    Ok(parsed
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string())
}
