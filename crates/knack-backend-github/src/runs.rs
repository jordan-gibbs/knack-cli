//! Local run telemetry for self-host mode.
//!
//! Records live in `<repo>/runs/<yyyy-mm>/<yyyy-mm-dd>.jsonl`, append-only,
//! one event per line. Two event kinds exist:
//!
//!   - `started`: written by `knack run <slug>`. Pins the skill version and
//!     timestamps the start.
//!   - `marked`: written by `knack mark <run_id> succeeded|failed`. Records
//!     outcome + optional note.
//!
//! Finding a run looks across the last 30 days of JSONL files (chronologically
//! recent first). The latest event for a given `run_id` wins; if a `started`
//! exists without a `marked`, status is reported as `started`.

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Utc};
use knack_types::RunLog;
use serde::{Deserialize, Serialize};
use std::fs::{create_dir_all, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const LOOKBACK_DAYS: i64 = 30;

/// Append a structured RunLog (used by the legacy `Backend::record_run`
/// surface). Kept for compatibility with the existing trait impl.
pub fn append_run(repo: &Path, log: &RunLog) -> Result<()> {
    let line = serde_json::to_string(log).context("serialize run log")?;
    append_to_day_file(repo, &log.started_at, &line)
}

/// One JSONL record. The `event` field discriminates between `started` and
/// `marked`. Untagged fields are optional and depend on the event type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEvent {
    pub event: String, // "started" | "marked"
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
    pub input: Option<String>,
    pub status: String, // "started" | "succeeded" | "failed" | "aborted"
    pub note: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Write a "started" event and return the generated run id.
pub fn start_run(
    repo: &Path,
    slug: &str,
    version: &str,
    agent: Option<&str>,
    input: Option<&str>,
) -> Result<Uuid> {
    let run_id = Uuid::new_v4();
    let now = Utc::now();
    let event = RunEvent {
        event: "started".into(),
        run_id: run_id.to_string(),
        skill: Some(slug.to_string()),
        version: Some(version.to_string()),
        agent: agent.map(|s| s.to_string()),
        input: input.map(|s| s.to_string()),
        status: Some("started".into()),
        note: None,
        at: now,
    };
    let line = serde_json::to_string(&event).context("serialize started event")?;
    append_to_day_file(repo, &now, &line)?;
    Ok(run_id)
}

/// Append a "marked" event for an existing run. Returns the new snapshot.
/// Errors if the run id can't be found within the lookback window.
pub fn mark_run(
    repo: &Path,
    run_id: &str,
    status: &str,
    note: Option<&str>,
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
        input: existing.input.clone(),
        status: Some(status.to_string()),
        note: note.map(|s| s.to_string()),
        at: now,
    };
    let line = serde_json::to_string(&event).context("serialize marked event")?;
    append_to_day_file(repo, &now, &line)?;

    Ok(RunSnapshot {
        run_id: run_id.into(),
        skill: existing.skill,
        version: existing.version,
        agent: existing.agent,
        input: existing.input,
        status: status.into(),
        note: note.map(|s| s.into()),
        started_at: existing.started_at,
        completed_at: Some(now),
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

    // Walk newest -> oldest so we encounter the latest event first and can
    // cheaply update `status`/`completed_at` if we see an earlier `started`
    // for the same id later.
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
            let Ok(ev) = serde_json::from_str::<RunEvent>(&line) else {
                continue; // tolerate legacy RunLog lines and malformed entries
            };
            if ev.run_id != run_id {
                continue;
            }
            snapshot = Some(merge_event(snapshot.take(), ev));
        }
    }
    Ok(snapshot)
}

fn merge_event(prior: Option<RunSnapshot>, ev: RunEvent) -> RunSnapshot {
    let mut s = prior.unwrap_or_else(|| RunSnapshot {
        run_id: ev.run_id.clone(),
        skill: None,
        version: None,
        agent: None,
        input: None,
        status: "unknown".into(),
        note: None,
        started_at: None,
        completed_at: None,
    });
    // First non-None wins for identity fields. This handles the case where
    // a `marked` event lands before its `started` in the chronological scan
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
    if s.input.is_none() {
        s.input = ev.input;
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
