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
//! **Push policy.** Events are NOT auto-committed or auto-pushed. They
//! sit locally in the git working tree as untracked or modified files.
//! The user (or `knack publish <skill>`) is responsible for committing
//! the `runs/` directory when they want it to land on the remote. This
//! keeps `knack run` and `knack mark` fast (no network) and avoids a
//! noisy commit per call.
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
use uuid::Uuid;

const LOOKBACK_DAYS: i64 = 30;

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
        inputs: Vec::new(),
        outputs: Vec::new(),
        status: Some(status.into()),
        note: None,
        at: log.started_at,
    };
    let line = serde_json::to_string(&event).context("serialize run event")?;
    append_to_day_file(repo, &log.started_at, &line)
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
pub fn start_run(
    repo: &Path,
    slug: &str,
    version: &str,
    agent: Option<&str>,
    inputs: &[String],
) -> Result<Uuid> {
    let run_id = Uuid::new_v4();
    let now = Utc::now();
    let event = RunEvent {
        event: "started".into(),
        run_id: run_id.to_string(),
        skill: Some(slug.to_string()),
        version: Some(version.to_string()),
        agent: agent.map(|s| s.to_string()),
        inputs: inputs.to_vec(),
        outputs: Vec::new(),
        status: Some("started".into()),
        note: None,
        at: now,
    };
    let line = serde_json::to_string(&event).context("serialize started event")?;
    append_to_day_file(repo, &now, &line)?;
    Ok(run_id)
}

/// Append a "marked" event for an existing run. Returns the new snapshot
/// with computed duration. Errors if the run id can't be found within the
/// lookback window.
pub fn mark_run(
    repo: &Path,
    run_id: &str,
    status: &str,
    note: Option<&str>,
    outputs: &[String],
) -> Result<RunSnapshot> {
    let existing = find_run(repo, run_id)?.ok_or_else(|| {
        anyhow::anyhow!("run {} not found in last {} days", run_id, LOOKBACK_DAYS)
    })?;

    let now = Utc::now();
    let event = RunEvent {
        event: "marked".into(),
        run_id: run_id.to_string(),
        skill: existing.skill.clone(),
        version: existing.version.clone(),
        agent: existing.agent.clone(),
        // Inputs propagate from the started event so the marked line is
        // self-contained for grep-style audits.
        inputs: existing.inputs.clone(),
        outputs: outputs.to_vec(),
        status: Some(status.to_string()),
        note: note.map(|s| s.to_string()),
        at: now,
    };
    let line = serde_json::to_string(&event).context("serialize marked event")?;
    append_to_day_file(repo, &now, &line)?;

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

/// Scan the last LOOKBACK_DAYS of daily files for events with the given
/// run id. Returns the latest assembled snapshot, or None if not found.
pub fn find_run(repo: &Path, run_id: &str) -> Result<Option<RunSnapshot>> {
    let runs_root = repo.join("runs");
    if !runs_root.exists() {
        return Ok(None);
    }

    let today = Utc::now().date_naive();
    let mut snapshot: Option<RunSnapshot> = None;

    for offset in 0..=LOOKBACK_DAYS {
        let date = today - Duration::days(offset);
        let file = day_file(repo, date.year(), date.month(), date.day());
        if !file.exists() {
            continue;
        }
        let reader = BufReader::new(
            std::fs::File::open(&file).with_context(|| format!("open {}", file.display()))?,
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
                continue; // tolerate legacy RunLog lines and malformed entries
            };
            if ev.run_id != run_id {
                continue;
            }
            snapshot = Some(merge_event(snapshot.take(), ev));
        }
    }
    Ok(snapshot.map(finalize_duration))
}

/// Parse a JSONL line into a `RunEvent`, migrating the pre-v0.7.2 single
/// `input: "<path>"` field into `inputs: ["<path>"]` on the fly so old
/// files keep working with the new readers.
fn parse_event_line(line: &str) -> Option<RunEvent> {
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

fn finalize_duration(mut s: RunSnapshot) -> RunSnapshot {
    if s.duration_ms.is_none() {
        if let (Some(start), Some(end)) = (s.started_at, s.completed_at) {
            if let Ok(ms) = (end - start).num_milliseconds().try_into() {
                s.duration_ms = Some(ms);
            }
        }
    }
    s
}

fn merge_event(prior: Option<RunSnapshot>, ev: RunEvent) -> RunSnapshot {
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

fn day_file(repo: &Path, year: i32, month: u32, day: u32) -> PathBuf {
    repo.join("runs")
        .join(format!("{:04}-{:02}", year, month))
        .join(format!("{:04}-{:02}-{:02}.jsonl", year, month, day))
}

fn append_to_day_file(repo: &Path, at: &DateTime<Utc>, line: &str) -> Result<()> {
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
    Ok(())
}
