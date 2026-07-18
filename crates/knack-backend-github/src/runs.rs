//! Local run telemetry for self-host mode.
//!
//! ## Rules (the consistent shape)
//!
//! **Storage.** One JSONL file per day at
//! `<local_clone>/runs/<YYYY-MM>/<YYYY-MM-DD>.jsonl`. Append-only. Files
//! are organized by month to keep directory listings small.
//!
//! **Schema.** Every line is a [`RunEvent`] with these fields. Optional
//! fields are omitted when missing; required fields are always present.
//!
//!   * `event`     (required, "started" | "marked") — the kind of record.
//!   * `run_id`    (required, UUID string) — stable id linking events of
//!                 the same run across the day's file.
//!   * `at`        (required, RFC3339 UTC) — when the event was recorded.
//!   * `skill`     (set on `started`, propagated to `marked`) — slug.
//!   * `version`   (set on `started`) — semver at the time of the run.
//!   * `agent`     (set on `started`) — caller-supplied tag
//!                 ("claude-code", "cursor", "codex", ...).
//!   * `inputs`    (set on `started`) — array of file paths or short
//!                 summaries describing what the agent worked on.
//!                 Empty array if the run had no input.
//!   * `outputs`   (set on `marked`) — array of file paths or summaries
//!                 describing what the run produced.
//!   * `status`    (set on every event) — "started" | "succeeded" |
//!                 "failed" | "aborted".
//!   * `note`      (set on `marked` when --note / --reason was passed) —
//!                 free-form text. For "failed", explain what went wrong.
//!
//! **Lifecycle.** A run goes through two events:
//!
//!   1. `knack run <slug> [--input PATH]...` writes a `started` event and
//!      prints the generated `run_id`. `--input` is repeatable.
//!   2. `knack mark <run_id> succeeded|failed [--note …] [--output PATH]...`
//!      writes a `marked` event that closes the loop.
//!
//! [`RunSnapshot`] (built by [`find_run`]) also computes a `duration_ms`
//! field from `started_at` to `completed_at` so consumers don't have to.
//!
//! **Lookback.** [`find_run`] scans the last [`LOOKBACK_DAYS`] of daily
//! files (newest first). A `marked` event is considered authoritative;
//! a lone `started` reports `status = "started"`.
//!
//! **Push policy (batched).** Every `start_run` and `mark_run`
//! auto-commits the affected JSONL file, but the network push is
//! batched — a field report measured 36 of 48 registry commits as
//! telemetry noise, with a push round-trip on the hot path of every
//! skill use. Two mechanisms keep the history readable and the hot
//! path local:
//!
//!   * **Commit collapsing.** When HEAD is an *unpushed* telemetry
//!     commit, the new event amends it instead of stacking another
//!     commit (`telemetry: batch (N events)`); pushed commits are never
//!     amended. Registry history reads as a changelog of skills with
//!     one telemetry commit per batch.
//!   * **Threshold pushes.** The branch is pushed only once the
//!     unpushed batch reaches [`telemetry_batch_threshold`] events
//!     (default 10; `KNACK_TELEMETRY_BATCH=1` restores push-per-event).
//!     `knack runs flush` pushes on demand, and any `knack publish`
//!     carries pending telemetry commits along with it.
//!
//! Only the specific day's JSONL file is staged, so unrelated
//! working-tree changes are NOT swept into the telemetry commit.
//!
//! Failures are best-effort: if the push fails (offline, branch diverged,
//! whatever), the local JSONL append still succeeds and the function
//! returns Ok with a stderr warning. The next successful flush, threshold
//! push, or `publish` will pick up the queued commit(s) and push them.
//!
//! **Tolerance.** The reader skips lines it can't parse (malformed,
//! truncated, or a legacy [`knack_types::RunLog`] line from an older
//! binary) so a stray line doesn't break `find_run`. The reader also
//! migrates the pre-v0.7.2 single-`input` field into `inputs: [...]` on
//! the fly so older files keep working.

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Utc};
use knack_types::RunLog;
use serde::{Deserialize, Serialize};
use std::fs::{create_dir_all, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

/// Default window (in days) for run lookups that take a `--since` flag.
/// `find_run` uses it as a hard cap; `aggregate::scan_snapshots` uses it
/// as the default when the caller doesn't pass an explicit window.
pub const DEFAULT_LOOKBACK_DAYS: i64 = 30;
const LOOKBACK_DAYS: i64 = DEFAULT_LOOKBACK_DAYS;

/// Append a structured `RunLog` (the [`Backend::record_run`] trait surface).
///
/// Converts the legacy `RunLog` into the canonical [`RunEvent`] shape so
/// every line in the JSONL has the same schema regardless of which writer
/// produced it.
pub fn append_run(repo: &Path, log: &RunLog) -> Result<()> {
    let status = match log.status {
        knack_types::RunStatus::Succeeded => "succeeded",
        knack_types::RunStatus::Failed => "failed",
        knack_types::RunStatus::Aborted => "aborted",
    };
    let event = RunEvent {
        event: "marked".into(),
        run_id: log.run_id.to_string(),
        skill: Some(log.skill.clone()),
        version: None,
        agent: Some(log.agent.clone()),
        mode: None,
        inputs: Vec::new(),
        outputs: Vec::new(),
        status: Some(status.into()),
        note: None,
        at: log.started_at,
    };
    let line = serde_json::to_string(&event).context("serialize run event")?;
    append_to_day_file(repo, &log.started_at, &line).map(|_| ())
}

/// One JSONL record. The `event` field discriminates between `started` and
/// `marked`. Other fields are populated as documented in the module-level
/// schema comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEvent {
    pub event: String, // "started" | "marked"
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Which frontmatter mode the agent ran the skill in (partial
    /// loading). Recorded on `started`; absent for modeless skills and
    /// pre-modes CLIs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub at: DateTime<Utc>,
}

/// Compact snapshot of a run's latest state, assembled from the JSONL log.
#[derive(Debug, Clone, Serialize)]
pub struct RunSnapshot {
    pub run_id: String,
    pub skill: Option<String>,
    pub version: Option<String>,
    pub agent: Option<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub status: String, // "started" | "succeeded" | "failed" | "aborted"
    pub note: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Convenience field computed from `completed_at - started_at` when both
    /// are present. Saves consumers from doing the subtraction.
    pub duration_ms: Option<u64>,
}

/// Write a "started" event and return the generated run id.
///
/// `push` controls the post-commit `git push` to origin: callers pass
/// `false` to opt out (via CLI `--no-push`, workspace `auto_push: false`,
/// or `KNACK_AUTO_PUSH=0`). When `false` the local append + commit still
/// happen so the next push catches up; only the network hop is skipped.
pub fn start_run(
    repo: &Path,
    slug: &str,
    version: &str,
    agent: Option<&str>,
    inputs: &[String],
    mode: Option<&str>,
    push: bool,
) -> Result<Uuid> {
    let run_id = Uuid::new_v4();
    let now = Utc::now();
    let event = RunEvent {
        event: "started".into(),
        run_id: run_id.to_string(),
        skill: Some(slug.to_string()),
        version: Some(version.to_string()),
        agent: agent.map(|s| s.to_string()),
        mode: mode.map(|s| s.to_string()),
        inputs: inputs.to_vec(),
        outputs: Vec::new(),
        status: Some("started".into()),
        note: None,
        at: now,
    };
    let line = serde_json::to_string(&event).context("serialize started event")?;
    let day_path = append_to_day_file(repo, &now, &line)?;
    commit_and_push_event(repo, &day_path, slug, "started", &run_id.to_string(), push);
    Ok(run_id)
}

/// Append a "marked" event for an existing run. Returns the new snapshot
/// with computed duration. Errors if the run id can't be found.
///
/// See [`start_run`] for the `push` flag's semantics.
pub fn mark_run(
    repo: &Path,
    run_id: &str,
    status: &str,
    note: Option<&str>,
    outputs: &[String],
    push: bool,
) -> Result<RunSnapshot> {
    let existing = find_run(repo, run_id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "run {} not found in any runs/YYYY-MM/ daily file under {}",
            run_id,
            repo.display()
        )
    })?;

    let now = Utc::now();
    let event = RunEvent {
        event: "marked".into(),
        run_id: run_id.to_string(),
        skill: existing.skill.clone(),
        version: existing.version.clone(),
        agent: existing.agent.clone(),
        mode: None,
        // Inputs propagate from the started event so the marked line is
        // self-contained for grep-style audits.
        inputs: existing.inputs.clone(),
        outputs: outputs.to_vec(),
        status: Some(status.to_string()),
        note: note.map(|s| s.to_string()),
        at: now,
    };
    let line = serde_json::to_string(&event).context("serialize marked event")?;
    let day_path = append_to_day_file(repo, &now, &line)?;
    commit_and_push_event(
        repo,
        &day_path,
        existing.skill.as_deref().unwrap_or("unknown"),
        &format!("marked-{status}"),
        run_id,
        push,
    );

    let duration_ms = existing
        .started_at
        .and_then(|start| (now - start).num_milliseconds().try_into().ok());

    Ok(RunSnapshot {
        run_id: run_id.into(),
        skill: existing.skill,
        version: existing.version,
        agent: existing.agent,
        inputs: existing.inputs,
        outputs: outputs.to_vec(),
        status: status.into(),
        note: note.map(|s| s.into()),
        started_at: existing.started_at,
        completed_at: Some(now),
        duration_ms,
    })
}

/// Scan daily JSONL files for events matching `run_id` and return the
/// assembled snapshot.
///
/// Two-pass: the fast path walks the last [`LOOKBACK_DAYS`] of files in
/// the order callers typically need (newest first). Most marks land within
/// hours of the matching `run`, so this covers the common case in O(30).
/// If nothing matched, the slow path walks every `runs/YYYY-MM/` directory
/// on disk so a mark days/months later still finds its parent run. This
/// removes the day-31 footgun without paying the full scan on every call.
pub fn find_run(repo: &Path, run_id: &str) -> Result<Option<RunSnapshot>> {
    let runs_root = repo.join("runs");
    if !runs_root.exists() {
        return Ok(None);
    }

    let today = Utc::now().date_naive();

    // Fast path: scan the last LOOKBACK_DAYS daily files newest-first.
    let mut snapshot: Option<RunSnapshot> = None;
    let mut seen_files: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    for offset in 0..=LOOKBACK_DAYS {
        let date = today - Duration::days(offset);
        let file = day_file(repo, date.year(), date.month(), date.day());
        if !file.exists() {
            continue;
        }
        seen_files.insert(file.clone());
        snapshot = scan_day_file_for_run(&file, run_id, snapshot.take())?;
    }
    if snapshot.is_some() {
        return Ok(snapshot.map(finalize_duration));
    }

    // Slow path: walk every month directory under runs/ so a mark after
    // the fast-path window still resolves its parent.
    //
    // Validate every directory and file name against the canonical
    // `YYYY-MM/` and `YYYY-MM-DD.jsonl` shapes before opening. Without
    // this gate the slow path would walk `runs/archive/`, `.DS_Store`,
    // `runs/scratch/*.jsonl.bak`, etc. — both wasted I/O and a quiet
    // privacy leak if a user parked unrelated content under runs/.
    let month_dirs = std::fs::read_dir(&runs_root)
        .with_context(|| format!("read {}", runs_root.display()))?;
    for entry in month_dirs {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Month dir must be exactly `YYYY-MM`.
        if !is_valid_month_dir_name(&entry.file_name()) {
            continue;
        }
        let day_files = match std::fs::read_dir(&path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for day in day_files {
            let day = match day {
                Ok(d) => d,
                Err(_) => continue,
            };
            let day_path = day.path();
            if !day_path.is_file() {
                continue;
            }
            // Day file must be exactly `YYYY-MM-DD.jsonl`.
            if !is_valid_day_file_name(&day.file_name()) {
                continue;
            }
            if seen_files.contains(&day_path) {
                continue;
            }
            snapshot = scan_day_file_for_run(&day_path, run_id, snapshot.take())?;
        }
    }
    Ok(snapshot.map(finalize_duration))
}

/// True iff `name` matches the canonical zero-padded `YYYY-MM` shape.
/// Rejects `2026-13`, `archive`, `.DS_Store`, `2026-05-extra`, `2026-5`
/// (single-digit month). The byte-level shape check is paired with a
/// chrono validity check so `2026-13` doesn't slip through.
fn is_valid_month_dir_name(name: &std::ffi::OsStr) -> bool {
    let Some(s) = name.to_str() else { return false };
    if s.len() != 7 || s.as_bytes()[4] != b'-' {
        return false;
    }
    if !s[..4].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if !s[5..].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    chrono::NaiveDate::parse_from_str(&format!("{s}-01"), "%Y-%m-%d").is_ok()
}

/// True iff `name` matches the canonical `YYYY-MM-DD.jsonl` shape.
/// Rejects `.gitkeep`, `README.md`, `2026-05-29.jsonl.bak`, `2026-5-1.jsonl`.
fn is_valid_day_file_name(name: &std::ffi::OsStr) -> bool {
    let Some(s) = name.to_str() else { return false };
    let Some(stem) = s.strip_suffix(".jsonl") else { return false };
    if stem.len() != 10 || stem.as_bytes()[4] != b'-' || stem.as_bytes()[7] != b'-' {
        return false;
    }
    if !stem[..4].chars().all(|c| c.is_ascii_digit())
        || !stem[5..7].chars().all(|c| c.is_ascii_digit())
        || !stem[8..].chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d").is_ok()
}

fn scan_day_file_for_run(
    file: &Path,
    run_id: &str,
    mut snapshot: Option<RunSnapshot>,
) -> Result<Option<RunSnapshot>> {
    let reader = BufReader::new(
        std::fs::File::open(file).with_context(|| format!("open {}", file.display()))?,
    );
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let Some(ev) = parse_event_line(&line) else {
            continue;
        };
        if ev.run_id != run_id {
            continue;
        }
        snapshot = Some(merge_event(snapshot.take(), ev));
    }
    Ok(snapshot)
}

/// Parse a JSONL line into a `RunEvent`, migrating the pre-v0.7.2 single
/// `input: "<path>"` field into `inputs: ["<path>"]` on the fly so old
/// files keep working with the new readers.
pub(crate) fn parse_event_line(line: &str) -> Option<RunEvent> {
    let mut value: serde_json::Value = serde_json::from_str(line).ok()?;
    if let Some(obj) = value.as_object_mut() {
        if !obj.contains_key("inputs") {
            if let Some(legacy) = obj.remove("input") {
                if let Some(s) = legacy.as_str() {
                    obj.insert(
                        "inputs".into(),
                        serde_json::Value::Array(vec![serde_json::Value::String(s.into())]),
                    );
                }
            }
        }
    }
    serde_json::from_value(value).ok()
}

pub(crate) fn finalize_duration(mut s: RunSnapshot) -> RunSnapshot {
    if s.duration_ms.is_none() {
        if let (Some(start), Some(end)) = (s.started_at, s.completed_at) {
            if let Ok(ms) = (end - start).num_milliseconds().try_into() {
                s.duration_ms = Some(ms);
            }
        }
    }
    s
}

pub(crate) fn merge_event(prior: Option<RunSnapshot>, ev: RunEvent) -> RunSnapshot {
    let mut s = prior.unwrap_or_else(|| RunSnapshot {
        run_id: ev.run_id.clone(),
        skill: None,
        version: None,
        agent: None,
        inputs: Vec::new(),
        outputs: Vec::new(),
        status: "unknown".into(),
        note: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    });
    // First non-empty / non-None wins for identity fields. Handles the case
    // where a `marked` event lands before its `started` in the scan order
    // (defensive against clock skew).
    if s.skill.is_none() {
        s.skill = ev.skill;
    }
    if s.version.is_none() {
        s.version = ev.version;
    }
    if s.agent.is_none() {
        s.agent = ev.agent;
    }
    if s.inputs.is_empty() {
        s.inputs = ev.inputs;
    }
    if s.outputs.is_empty() && !ev.outputs.is_empty() {
        s.outputs = ev.outputs;
    }

    match ev.event.as_str() {
        "started" => {
            if s.started_at.is_none() {
                s.started_at = Some(ev.at);
            }
            if s.status == "unknown" {
                s.status = "started".into();
            }
        }
        "marked" => {
            if let Some(st) = ev.status {
                s.status = st;
            }
            s.completed_at = Some(ev.at);
            if ev.note.is_some() {
                s.note = ev.note;
            }
        }
        _ => {}
    }
    s
}

pub(crate) fn day_file(repo: &Path, year: i32, month: u32, day: u32) -> PathBuf {
    repo.join("runs")
        .join(format!("{:04}-{:02}", year, month))
        .join(format!("{:04}-{:02}-{:02}.jsonl", year, month, day))
}

/// Append `line` to the JSONL file for `at`'s date. Returns the path it
/// wrote to so the caller can hand it to [`commit_and_push_event`].
fn append_to_day_file(repo: &Path, at: &DateTime<Utc>, line: &str) -> Result<PathBuf> {
    let date = at.date_naive();
    let month_dir = repo
        .join("runs")
        .join(format!("{:04}-{:02}", date.year(), date.month()));
    create_dir_all(&month_dir).context("create runs month dir")?;
    let file = day_file(repo, date.year(), date.month(), date.day());
    let mut handle = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .with_context(|| format!("open {}", file.display()))?;
    writeln!(handle, "{}", line).context("write run event")?;
    Ok(file)
}

/// Stage just the affected JSONL file, commit it with a telemetry message,
/// push to origin/main. Best-effort: prints a warning to stderr on failure
/// and returns; never errors out the caller's command. The local append is
/// already done, so the caller's snapshot is still correct.
fn commit_and_push_event(
    repo: &Path,
    jsonl_path: &Path,
    skill: &str,
    event: &str,
    run_id: &str,
    push: bool,
) {
    let rel = match jsonl_path.strip_prefix(repo) {
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => {
            eprintln!(
                "knack: telemetry recorded locally but couldn't compute repo-relative path for {}",
                jsonl_path.display()
            );
            return;
        }
    };

    // git add <rel>: only the day's JSONL. Don't sweep in unrelated edits.
    match Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["add", &rel])
        .output()
    {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            eprintln!(
                "knack: telemetry recorded locally but `git add {}` failed: {}",
                rel,
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return;
        }
        Err(e) => {
            eprintln!("knack: telemetry recorded locally but couldn't invoke `git add`: {e}");
            return;
        }
    }

    let target = crate::git::resolve_remote(repo);

    // Collapse into HEAD when it's an unpushed telemetry commit: one
    // batch commit per push instead of two commits per skill use. Never
    // amend anything the remote has already seen.
    let prior_events = unpushed_telemetry_head_events(repo, &target);
    let (commit_args, msg): (Vec<&str>, String) = match prior_events {
        Some(n) => {
            let m = format!("telemetry: batch ({} events)", n + 1);
            (vec!["commit", "--amend", "-m"], m)
        }
        None => {
            let m = format!("telemetry: {event} {skill} {run_id}");
            (vec!["commit", "-m"], m)
        }
    };
    match Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(&commit_args)
        .arg(&msg)
        .output()
    {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let stdout = String::from_utf8_lossy(&o.stdout);
            let combined = format!("{stdout}{stderr}");
            // No-op when the JSONL file is already at this content (rare
            // race, idempotent retry, etc.). Not a real failure.
            if combined.contains("nothing to commit") || combined.contains("nothing added") {
                return;
            }
            eprintln!(
                "knack: telemetry recorded locally but `git commit` failed: {}",
                stderr.trim()
            );
            return;
        }
        Err(e) => {
            eprintln!("knack: telemetry recorded locally but couldn't invoke `git commit`: {e}");
            return;
        }
    }

    // Opt-out gate. The caller resolved the policy via
    // `workspace::PushPolicy::resolve` BEFORE calling us; we just trust
    // the resolved bool and short-circuit. The single resolver owns
    // CLI/env/workspace precedence — checking the env again here would
    // be a second source of truth that could drift.
    if !push {
        return;
    }

    // Threshold gate: hold the push until enough events queue up. When
    // the pending count can't be determined (no remote-tracking ref yet,
    // e.g. a fresh repo), fall through to pushing — the conservative
    // direction is "data reaches the team", not "data sits local".
    let threshold = telemetry_batch_threshold();
    if let Some(pending) = pending_telemetry_events(repo, &target) {
        if pending < threshold {
            return;
        }
    }

    push_pending(repo, &target);
}

/// Push the branch via system git (inherits the user's gh credential
/// helper). Shared by the threshold path and `knack runs flush`.
pub fn push_pending(repo: &Path, target: &crate::git::RemoteTarget) {
    match Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["push", &target.remote, &target.branch])
        .output()
    {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            eprintln!(
                "knack: telemetry committed locally but `git push {} {}` failed: {}\n      run `git -C {} push {} {}` once you're back online (or rebase if there's a divergence).",
                target.remote,
                target.branch,
                stderr.trim(),
                repo.display(),
                target.remote,
                target.branch,
            );
        }
        Err(e) => {
            eprintln!("knack: telemetry committed locally but couldn't invoke `git push`: {e}");
        }
    }
}

/// Events to accumulate before a telemetry push. Default 10; env
/// `KNACK_TELEMETRY_BATCH` overrides (min 1 — i.e. push every event,
/// the pre-batching behavior).
pub fn telemetry_batch_threshold() -> usize {
    std::env::var("KNACK_TELEMETRY_BATCH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|n| n.max(1))
        .unwrap_or(10)
}

/// Events represented by a telemetry commit subject: `batch (N events)`
/// carries N, a single `telemetry: …` line carries 1, anything else 0.
fn telemetry_events_in_subject(subject: &str) -> usize {
    let Some(rest) = subject.strip_prefix("telemetry: ") else {
        return 0;
    };
    if let Some(inner) = rest
        .strip_prefix("batch (")
        .and_then(|r| r.strip_suffix(" events)").or_else(|| r.strip_suffix(" event)")))
    {
        return inner.parse::<usize>().unwrap_or(1);
    }
    1
}

/// When HEAD is a telemetry commit the remote has NOT seen, return how
/// many events it already holds (so the caller can amend and bump the
/// count). Any uncertainty — no HEAD, no remote-tracking ref, HEAD
/// already pushed — returns None, and the caller stacks a new commit
/// instead of rewriting anything shared.
fn unpushed_telemetry_head_events(
    repo: &Path,
    target: &crate::git::RemoteTarget,
) -> Option<usize> {
    let head_subject = git_stdout(repo, &["log", "-1", "--format=%s"])?;
    let events = telemetry_events_in_subject(head_subject.trim());
    if events == 0 {
        return None;
    }
    let remote_ref = format!("refs/remotes/{}/{}", target.remote, target.branch);
    // No remote-tracking ref → can't prove HEAD is unpushed → don't amend.
    git_stdout(repo, &["rev-parse", "--verify", "--quiet", &remote_ref])?;
    // Exit 0 ⇒ HEAD is an ancestor of the remote tip ⇒ already pushed.
    let pushed = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", "HEAD", &remote_ref])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(true);
    if pushed {
        None
    } else {
        Some(events)
    }
}

/// Total telemetry events sitting in commits the remote hasn't seen.
/// None when it can't be determined (no remote-tracking ref).
fn pending_telemetry_events(repo: &Path, target: &crate::git::RemoteTarget) -> Option<usize> {
    let remote_ref = format!("refs/remotes/{}/{}", target.remote, target.branch);
    git_stdout(repo, &["rev-parse", "--verify", "--quiet", &remote_ref])?;
    let range = format!("{remote_ref}..HEAD");
    let subjects = git_stdout(repo, &["log", "--format=%s", &range])?;
    Some(
        subjects
            .lines()
            .map(|l| telemetry_events_in_subject(l.trim()))
            .sum(),
    )
}

/// Public wrapper for `knack runs flush`: how many telemetry events are
/// queued locally, or None when there's no remote-tracking ref to
/// compare against.
pub fn pending_events(repo: &Path) -> Option<usize> {
    let target = crate::git::resolve_remote(repo);
    pending_telemetry_events(repo, &target)
}

/// Run git with `args`, returning trimmed stdout on success (non-empty
/// or not — the caller decides), None on any failure.
fn git_stdout(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_event_at(repo: &Path, year: i32, month: u32, day: u32, line: &str) {
        let dir = repo.join("runs").join(format!("{year:04}-{month:02}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{year:04}-{month:02}-{day:02}.jsonl"));
        let mut existing = fs::read_to_string(&path).unwrap_or_default();
        if !existing.is_empty() && !existing.ends_with('\n') {
            existing.push('\n');
        }
        existing.push_str(line);
        existing.push('\n');
        fs::write(&path, existing).unwrap();
    }

    #[test]
    fn find_run_recovers_run_outside_lookback_window() {
        // Regression for the silent "run not found" cliff at day 31. A run
        // started >LOOKBACK_DAYS ago should still resolve via the
        // directory-walk fallback so a late mark doesn't strand the agent.
        let dir = tempdir().unwrap();
        let repo = dir.path();

        let old = Utc::now().date_naive() - Duration::days(LOOKBACK_DAYS + 60);
        let started_at = format!("{}T12:00:00Z", old.format("%Y-%m-%d"));
        let line = serde_json::json!({
            "event": "started",
            "run_id": "very-old-run",
            "skill": "x",
            "agent": "claude-code",
            "inputs": ["./a.txt"],
            "status": "started",
            "at": started_at,
        })
        .to_string();
        write_event_at(repo, old.year(), old.month(), old.day(), &line);

        let found = find_run(repo, "very-old-run").unwrap();
        assert!(found.is_some(), "slow-path fallback should recover the old run");
        let snap = found.unwrap();
        assert_eq!(snap.run_id, "very-old-run");
        assert_eq!(snap.skill.as_deref(), Some("x"));
    }

    #[test]
    fn find_run_skips_non_ymd_dirs_and_files() {
        // Plant a legit old run alongside several garbage entries that
        // would have been opened pre-#15: a `runs/archive/` directory,
        // a `.DS_Store` directory (macOS reality), and a stray `.bak`
        // backup file inside an otherwise-valid month directory. After
        // #15 only `runs/<YYYY-MM>/<YYYY-MM-DD>.jsonl` files get opened.
        let dir = tempdir().unwrap();
        let repo = dir.path();

        let old = Utc::now().date_naive() - Duration::days(LOOKBACK_DAYS + 5);

        // Garbage that should be SKIPPED (no panic, no open):
        fs::create_dir_all(repo.join("runs").join("archive")).unwrap();
        fs::write(repo.join("runs").join("archive").join("garbage.jsonl"), b"{").unwrap();
        fs::create_dir_all(repo.join("runs").join(".DS_Store")).unwrap();
        // A stray .bak inside a real month dir — also skipped.
        let month_dir = repo
            .join("runs")
            .join(format!("{:04}-{:02}", old.year(), old.month()));
        fs::create_dir_all(&month_dir).unwrap();
        fs::write(
            month_dir.join(format!("{:04}-{:02}-{:02}.jsonl.bak", old.year(), old.month(), old.day())),
            b"{",
        )
        .unwrap();
        // A README in runs/ root.
        fs::write(repo.join("runs").join("README.md"), b"hi").unwrap();

        // The legit run in the canonical-named file.
        let started_at = format!("{}T12:00:00Z", old.format("%Y-%m-%d"));
        let line = serde_json::json!({
            "event": "started",
            "run_id": "selective-find",
            "skill": "ok",
            "agent": "claude-code",
            "inputs": ["./a.txt"],
            "status": "started",
            "at": started_at,
        })
        .to_string();
        write_event_at(repo, old.year(), old.month(), old.day(), &line);

        // Should find the legit run without blowing up on the garbage.
        let found = find_run(repo, "selective-find").unwrap();
        assert!(
            found.is_some(),
            "find_run should locate the legit run despite garbage in runs/"
        );
        assert_eq!(found.unwrap().skill.as_deref(), Some("ok"));
    }

    #[test]
    fn is_valid_month_dir_name_validates() {
        use std::ffi::OsStr;
        assert!(is_valid_month_dir_name(OsStr::new("2026-05")));
        assert!(is_valid_month_dir_name(OsStr::new("2025-12")));
        assert!(!is_valid_month_dir_name(OsStr::new("archive")));
        assert!(!is_valid_month_dir_name(OsStr::new(".DS_Store")));
        assert!(!is_valid_month_dir_name(OsStr::new("2026-13")));
        assert!(!is_valid_month_dir_name(OsStr::new("2026-5"))); // not zero-padded
        assert!(!is_valid_month_dir_name(OsStr::new("2026-05-extra")));
    }

    #[test]
    fn is_valid_day_file_name_validates() {
        use std::ffi::OsStr;
        assert!(is_valid_day_file_name(OsStr::new("2026-05-31.jsonl")));
        assert!(!is_valid_day_file_name(OsStr::new("2026-05-31.jsonl.bak")));
        assert!(!is_valid_day_file_name(OsStr::new("README.md")));
        assert!(!is_valid_day_file_name(OsStr::new(".gitkeep")));
        assert!(!is_valid_day_file_name(OsStr::new("2026-13-01.jsonl")));
    }

    // === telemetry batching ===

    /// KNACK_TELEMETRY_BATCH is process-global; serialize the tests
    /// that set it.
    static BATCH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Registry repo + local bare origin with one pushed baseline
    /// commit, so `refs/remotes/origin/main` exists and the batching
    /// logic can prove what the remote has seen.
    fn init_pushed_registry(root: &Path) -> PathBuf {
        let bare = root.join("origin.git");
        git2::Repository::init_bare(&bare).unwrap();
        let registry = root.join("registry");
        let repo = git2::Repository::init_opts(
            &registry,
            git2::RepositoryInitOptions::new()
                .initial_head("main")
                .external_template(false),
        )
        .unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "test").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        repo.remote("origin", bare.to_str().unwrap()).unwrap();
        fs::write(registry.join("README.md"), "baseline
").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README.md")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
        let ok = Command::new("git")
            .arg("-C")
            .arg(&registry)
            .args(["push", "origin", "main"])
            .output()
            .unwrap();
        assert!(ok.status.success(), "baseline push failed: {}", String::from_utf8_lossy(&ok.stderr));
        registry
    }

    fn head_subject(repo: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["log", "-1", "--format=%s"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn unpushed_count(repo: &Path) -> usize {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-list", "--count", "origin/main..HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    #[test]
    fn telemetry_events_in_subject_parses_all_shapes() {
        assert_eq!(telemetry_events_in_subject("telemetry: started foo abc"), 1);
        assert_eq!(telemetry_events_in_subject("telemetry: batch (7 events)"), 7);
        assert_eq!(telemetry_events_in_subject("telemetry: batch (1 event)"), 1);
        assert_eq!(telemetry_events_in_subject("feat: real work"), 0);
        assert_eq!(telemetry_events_in_subject(""), 0);
    }

    #[test]
    fn consecutive_unpushed_telemetry_commits_collapse_into_one_batch() {
        let _guard = BATCH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let registry = init_pushed_registry(dir.path());

        // Three events, push disabled: history must show ONE unpushed
        // telemetry commit carrying all three, not three commits.
        let id = start_run(&registry, "alpha", "1.0.0", Some("agent"), &[], None, false).unwrap();
        assert_eq!(unpushed_count(&registry), 1);
        assert!(head_subject(&registry).starts_with("telemetry: started alpha"));

        mark_run(&registry, &id.to_string(), "succeeded", None, &[], false).unwrap();
        assert_eq!(unpushed_count(&registry), 1);
        assert_eq!(head_subject(&registry), "telemetry: batch (2 events)");

        let _id2 = start_run(&registry, "alpha", "1.0.0", Some("agent"), &[], None, false).unwrap();
        assert_eq!(unpushed_count(&registry), 1);
        assert_eq!(head_subject(&registry), "telemetry: batch (3 events)");
    }

    #[test]
    fn threshold_batches_pushes_and_never_amends_pushed_commits() {
        let _guard = BATCH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("KNACK_TELEMETRY_BATCH", "2");
        let dir = tempdir().unwrap();
        let registry = init_pushed_registry(dir.path());
        let bare = dir.path().join("origin.git");
        let bare_tip = || {
            let r = git2::Repository::open(&bare).unwrap();
            r.refname_to_id("refs/heads/main").unwrap()
        };
        let baseline = bare_tip();

        // Event 1: below threshold — committed locally, NOT pushed.
        let id = start_run(&registry, "alpha", "1.0.0", Some("agent"), &[], None, true).unwrap();
        assert_eq!(bare_tip(), baseline, "first event must not push yet");
        assert_eq!(pending_events(&registry), Some(1));

        // Event 2: threshold reached — batch of 2 pushes.
        mark_run(&registry, &id.to_string(), "succeeded", None, &[], true).unwrap();
        assert_ne!(bare_tip(), baseline, "threshold must trigger the push");
        assert_eq!(pending_events(&registry), Some(0));

        // Event 3: HEAD is now pushed — must stack a NEW commit, never
        // amend shared history.
        let pushed_tip = bare_tip();
        let _id2 = start_run(&registry, "alpha", "1.0.0", Some("agent"), &[], None, true).unwrap();
        assert_eq!(bare_tip(), pushed_tip, "one fresh event stays below threshold");
        assert_eq!(unpushed_count(&registry), 1);
        assert!(head_subject(&registry).starts_with("telemetry: started alpha"));

        std::env::remove_var("KNACK_TELEMETRY_BATCH");
    }

}
