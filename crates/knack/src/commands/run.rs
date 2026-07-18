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

    /// Self-host only: skip the auto `git push origin main` that follows
    /// the telemetry commit. The local commit still lands so the next
    /// pushed event catches up. `KNACK_AUTO_PUSH=0` is the equivalent
    /// env-level kill switch; either disables the network hop.
    #[arg(long)]
    pub no_push: bool,

    /// Keep this machine's previous still-open run(s) of the same skill
    /// open instead of auto-closing them as `abandoned`.
    /// `KNACK_NO_AUTO_CLOSE=1` is the env-level equivalent.
    #[arg(long)]
    pub keep_open: bool,

    /// Frontmatter mode to run the skill in (multi-mode skills declare
    /// `modes:` in SKILL.md). Recorded in telemetry; the run output
    /// lists exactly which files that mode needs, so the agent loads
    /// only those instead of the whole folder.
    #[arg(long)]
    pub mode: Option<String>,
}

/// True when auto-close is disabled for this invocation.
fn auto_close_disabled(args: &RunArgs) -> bool {
    if args.keep_open {
        return true;
    }
    matches!(
        std::env::var("KNACK_NO_AUTO_CLOSE").ok().as_deref(),
        Some("1") | Some("true")
    )
}

/// See [`crate::run_state::stale_open_runs`]: a new run of a skill
/// supersedes this machine's previous still-open runs of the SAME
/// skill. They close as `abandoned` (never a fabricated `succeeded`),
/// which every aggregate excludes from success-rate.
use crate::run_state::stale_open_runs;

/// Resolve `--mode` against the local copy's SKILL.md frontmatter.
/// Returns the mode's `load` patterns expanded to the actual matching
/// files (skill-relative posix paths) so the agent can Read exactly
/// that set. Fails hard when the skill declares modes and the name
/// isn't one of them — recording telemetry under a typo'd mode is
/// worse than an error. A skill without modes (or without a local
/// copy to inspect) accepts any label and returns an empty list.
fn resolve_mode(mode: &str, source: Option<&std::path::Path>) -> Result<Vec<String>, CliError> {
    let Some(dir) = source else {
        return Ok(Vec::new());
    };
    let Ok(skill_md) = std::fs::read_to_string(dir.join("SKILL.md")) else {
        return Ok(Vec::new());
    };
    let Ok(Some(fm)) = crate::skill_pack::parse_skill_md_frontmatter(&skill_md) else {
        return Ok(Vec::new());
    };
    let Some(modes) = fm.modes else {
        return Ok(Vec::new());
    };
    let Some(spec) = modes.get(mode) else {
        let available: Vec<&str> = modes.keys().map(String::as_str).collect();
        return Err(CliError::User {
            code: "RUN_UNKNOWN_MODE".into(),
            message: format!("skill has no mode `{mode}`"),
            hint: Some(format!("available modes: {}", available.join(", "))),
        });
    };
    let arcnames: Vec<String> = crate::skill_pack::collect_skill_entries(dir)
        .map(|es| es.into_iter().map(|(a, _)| a).collect())
        .unwrap_or_default();
    let mut files: Vec<String> = arcnames
        .into_iter()
        .filter(|a| {
            spec.load
                .iter()
                .any(|pat| crate::skill_validators::wildcard_matches(pat, a))
        })
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

pub async fn run(args: RunArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        return github_run(&args, local_path, mode);
    }

    let (slug, version_filter) = crate::slug::parse_slug_at_version(&args.slug);

    // The copy of the skill this machine would actually read, resolved
    // the same way `publish` resolves its source (workspace drafts →
    // workspace skills → HOME pool). Printing it makes copy mismatches
    // visible in the transcript instead of discoverable only by git
    // archaeology (see `knack status`).
    let source = {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        crate::workspace::resolve_existing_skill_dir(slug, &cwd, &client.config.skills_dir)
    };

    // Resolve --mode BEFORE opening a run: telemetry recorded under a
    // typo'd mode is worse than an early error.
    let mode_files = match &args.mode {
        Some(m) => match resolve_mode(m, source.as_deref()) {
            Ok(files) => files,
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        },
        None => Vec::new(),
    };

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
    // (possibly multiple) --input paths into an array of {path, filename},
    // plus the frontmatter mode when one was passed — that keys future
    // per-mode success-rate slicing without a schema migration.
    let inputs_summary = if args.input.is_empty() && args.mode.is_none() {
        None
    } else {
        let mut summary = serde_json::Map::new();
        if !args.input.is_empty() {
            summary.insert(
                "files".into(),
                json!(args
                    .input
                    .iter()
                    .map(|p| json!({
                        "path": p,
                        "filename": p.file_name().and_then(|s| s.to_str()),
                    }))
                    .collect::<Vec<_>>()),
            );
        }
        if let Some(m) = &args.mode {
            summary.insert("mode".into(), json!(m));
        }
        Some(serde_json::Value::Object(summary))
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

    // Cache the id so `knack mark last` / prefix marking resolve without
    // UUID bookkeeping in the agent's context. Cache write failure must
    // never fail the run — the id was printed either way.
    let _ = crate::run_state::push_run(&run.id, slug, "cloud");

    // Supersede this machine's stale open runs of the same skill.
    // Best-effort: an older server that predates the `abandoned` status
    // rejects the mark; the run stays open exactly as before this
    // feature, so we warn and move on rather than fail the new run.
    let mut auto_closed: Vec<String> = Vec::new();
    if !auto_close_disabled(&args) {
        for stale in stale_open_runs("cloud", slug, &run.id) {
            let body = api_runs::RunMarkBody {
                status: "abandoned".to_string(),
                note: Some(format!("superseded by run {}", run.id)),
            };
            match api_runs::mark(&client, &stale.run_id, &body).await {
                Ok(_) => auto_closed.push(stale.run_id),
                Err(e) => {
                    eprintln!(
                        "knack: could not auto-close stale run {}: {e}",
                        stale.run_id
                    );
                }
            }
        }
        crate::run_state::set_marked(&auto_closed);
    }

    chatter(
        mode,
        format!(
            "run registered · skill={} · runtime={} — execute the skill, then \
             `knack mark last succeeded` (or `failed --note \"…\"`)",
            args.slug, runtime,
        ),
    );

    // Notify-only update flag: if this skill is linked as a slash command
    // and a newer version has been published upstream (e.g. by a teammate),
    // surface that — but NEVER pull it automatically. Pinned linked copies
    // only change when the user explicitly runs `knack link`. Cheap: a
    // registry read + version compare, no extra network call (we already
    // have the skill). Suppressed by `KNACK_NO_LINK_UPDATE_CHECK`.
    let update = if version_filter.is_none() {
        skill.current_version_semver.as_deref().and_then(|latest| {
            crate::commands::link::pending_update(
                slug,
                latest,
                skill.owner_username.as_deref(),
                skill.owner_team_id.is_some(),
            )
        })
    } else {
        None
    };

    emit_ok(
        mode,
        json!({
            "run_id": run.id,
            "skill_version_id": run.skill_version_id,
            "runtime": runtime,
            "auto_closed": auto_closed,
            "source": source.as_ref().map(|p| display_path(p)),
            "mode": args.mode,
            // Mode-relevant file set (skill-relative). The agent should
            // Read SKILL.md + exactly these instead of the whole folder.
            "mode_load": mode_files,
            "update_available": update.as_ref().map(|u| json!({
                "slug": u.slug,
                "have": u.have,
                "latest": u.latest,
                "author": u.author,
            })),
        }),
        || {
            println!("✓ run registered · {}", run.id);
            if !auto_closed.is_empty() {
                println!(
                    "  auto-closed {} stale run(s) as abandoned: {}",
                    auto_closed.len(),
                    auto_closed.join(", ")
                );
            }
            match &source {
                Some(p) => println!(
                    "  next: read {}/SKILL.md and do the work yourself,",
                    display_path(p)
                ),
                None => println!(
                    "  next: read ~/.knack/skills/{}/SKILL.md and do the work yourself,",
                    args.slug
                ),
            }
            if let Some(m) = &args.mode {
                if mode_files.is_empty() {
                    println!("        mode: {m}");
                } else {
                    println!(
                        "        mode: {m} — load only: {}",
                        mode_files.join(", ")
                    );
                }
            }
            println!("        then `knack mark last succeeded` (or `failed --note \"…\"`).");
            if let Some(u) = &update {
                println!("  ⚠ {}", u.line());
            }
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

    // Resolve --mode against the registry copy BEFORE recording: a
    // typo'd mode should error, not pollute telemetry.
    let mode_files = match &args.mode {
        Some(m) => match resolve_mode(m, Some(&skill_dir)) {
            Ok(files) => files,
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        },
        None => Vec::new(),
    };

    let agent_tag = args.runtime.clone().or_else(|| Some("agent".to_string()));
    let inputs: Vec<String> = args.input.iter().map(|p| p.display().to_string()).collect();

    let push = match resolve_push_flag(local_path, args.no_push) {
        Ok(p) => p,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };
    let run_id = match knack_backend_github::start_run(
        local_path,
        slug,
        &version,
        agent_tag.as_deref(),
        &inputs,
        args.mode.as_deref(),
        push,
    ) {
        Ok(id) => id,
        Err(e) => {
            let err = CliError::Internal(format!("record run: {e}"));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    // Cache for `knack mark last` / prefix resolution (best-effort).
    let _ = crate::run_state::push_run(&run_id.to_string(), slug, "github");

    // Supersede this machine's stale open runs of the same skill (see
    // the cloud path for rationale). Best-effort.
    let mut auto_closed: Vec<String> = Vec::new();
    if !auto_close_disabled(args) {
        for stale in stale_open_runs("github", slug, &run_id.to_string()) {
            let note = format!("superseded by run {run_id}");
            match knack_backend_github::mark_run(
                local_path,
                &stale.run_id,
                "abandoned",
                Some(&note),
                &[],
                push,
            ) {
                Ok(_) => auto_closed.push(stale.run_id),
                Err(e) => {
                    eprintln!(
                        "knack: could not auto-close stale run {}: {e}",
                        stale.run_id
                    );
                }
            }
        }
        crate::run_state::set_marked(&auto_closed);
    }

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
            "auto_closed": auto_closed,
            "source": display_path(&skill_dir),
            "mode": args.mode,
            "mode_load": mode_files,
            "log_file": display_path(&day_file),
        }),
        || {
            println!("✓ {} run-id: {}", slug, run_id);
            println!("  source: {}", display_path(&skill_dir));
            if let Some(m) = &args.mode {
                if mode_files.is_empty() {
                    println!("  mode: {m}");
                } else {
                    println!("  mode: {m} — load only: {}", mode_files.join(", "));
                }
            }
            if !auto_closed.is_empty() {
                println!(
                    "  auto-closed {} stale run(s) as abandoned: {}",
                    auto_closed.len(),
                    auto_closed.join(", ")
                );
            }
            println!("  recorded to {}", display_path(&day_file));
            println!();
            println!("close the loop with: knack mark last succeeded   (or `failed --reason …`)");
        },
    );
    Ok(())
}

/// Resolve the effective `push` flag for self-host telemetry. Thin
/// wrapper around `workspace::PushPolicy::resolve` that surfaces a
/// malformed knack.yaml as a CLI-level error instead of silently
/// defaulting to push-on (the pre-v0.7.10 behavior). The single source
/// of truth for precedence lives in the backend crate.
pub(super) fn resolve_push_flag(
    repo: &std::path::Path,
    cli_no_push: bool,
) -> Result<bool, CliError> {
    knack_backend_github::PushPolicy::resolve(cli_no_push, repo)
        .map(|p| p.should_push())
        .map_err(|e| CliError::User {
            code: "WORKSPACE_CONFIG_INVALID".into(),
            message: format!(
                "could not read auto_push from <repo>/knack.yaml: {e}"
            ),
            hint: Some(
                "edit knack.yaml to fix the YAML, or remove the file to fall back to defaults"
                    .into(),
            ),
        })
}

fn read_meta_version(skill_dir: &std::path::Path) -> Result<String, std::io::Error> {
    let bytes = std::fs::read(skill_dir.join("meta.knack.yaml"))?;
    let parsed: serde_yaml::Value = serde_yaml::from_slice(knack_types::strip_utf8_bom(&bytes))
        .map_err(|e| std::io::Error::other(format!("parse meta: {e}")))?;
    Ok(parsed
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string())
}
