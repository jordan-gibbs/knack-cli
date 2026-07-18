//! Aggregation utilities for self-host run telemetry.
//!
//! Reads JSONL day-files, reduces events on `run_id` into [`RunSnapshot`]s,
//! and groups them into [`StatsBucket`]s. Used by `knack runs list`,
//! `knack runs stats`, and `knack runs diff`.
//!
//! No on-disk index: every call is a fresh scan of the requested window.
//! That's cheap (the data is small, the format is line-oriented) and keeps
//! the storage layer dumb.

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::runs::{day_file, finalize_duration, merge_event, parse_event_line, RunSnapshot};

/// Per-skill rollup used by `knack runs overview`. Carries enough to
/// flag regressions (current version vs prior) and staleness (no runs
/// in the window) without a second pass.
#[derive(Debug, Clone, Serialize)]
pub struct SkillOverview {
    pub slug: String,
    pub current_version: Option<String>,
    pub runs_total: u64,
    /// Serialized as `runs_succeeded` / `runs_failed` to match the
    /// stats bucket field naming. The bare names exist in Rust for
    /// terse use in renderers.
    #[serde(rename = "runs_succeeded")]
    pub succeeded: u64,
    #[serde(rename = "runs_failed")]
    pub failed: u64,
    pub success_rate: Option<f64>,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `Some` when the current version has both a prior version with
    /// at least one marked run AND a measurably lower success rate.
    /// `None` when there's no comparator or no regression.
    pub regression: Option<RegressionInfo>,
    /// `true` when the skill had zero runs in the window. Surfaced
    /// separately from `runs_total == 0` so agents can filter on it.
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegressionInfo {
    pub current_version: String,
    pub prior_version: String,
    /// `current_rate - prior_rate`. Negative means current is worse.
    pub delta_success_rate: f64,
    pub current_success_rate: Option<f64>,
    pub prior_success_rate: Option<f64>,
}

/// Detect a regression in a skill's most recent version cohort.
/// Compares the lexicographically-greatest version (treated as
/// "current") to the next-greatest. Returns `None` when there's no
/// prior version, when either side is unmarked, or when current
/// matches or beats prior.
pub fn detect_regression(buckets: &[StatsBucket]) -> Option<RegressionInfo> {
    let mut versioned: Vec<&StatsBucket> = buckets
        .iter()
        .filter(|b| b.key.get("version").and_then(|v| v.as_deref()).is_some())
        .collect();
    versioned.sort_by(|a, b| {
        let av = a.key.get("version").unwrap().as_deref().unwrap();
        let bv = b.key.get("version").unwrap().as_deref().unwrap();
        av.cmp(bv)
    });
    if versioned.len() < 2 {
        return None;
    }
    let current = versioned[versioned.len() - 1];
    let prior = versioned[versioned.len() - 2];
    let current_rate = current.success_rate?;
    let prior_rate = prior.success_rate?;
    let delta = current_rate - prior_rate;
    if delta >= 0.0 {
        return None;
    }
    Some(RegressionInfo {
        current_version: current.key.get("version").unwrap().clone().unwrap(),
        prior_version: prior.key.get("version").unwrap().clone().unwrap(),
        delta_success_rate: delta,
        current_success_rate: Some(current_rate),
        prior_success_rate: Some(prior_rate),
    })
}

/// Build an [`SkillOverview`] for one skill from a slice of its
/// snapshots. Shared between self-host (walks the local clone) and
/// any future "summarize a fetched skill" caller.
pub fn build_overview(slug: &str, snapshots: &[RunSnapshot]) -> SkillOverview {
    let by_version = group_buckets(snapshots, &["version"]);
    let overall = build_bucket(BTreeMap::new(), &snapshots.iter().collect::<Vec<_>>());

    let current_version = by_version
        .iter()
        .filter_map(|b| b.key.get("version").and_then(|v| v.clone()))
        .max();

    let regression = detect_regression(&by_version);

    SkillOverview {
        slug: slug.to_string(),
        current_version,
        runs_total: overall.total,
        succeeded: overall.succeeded,
        failed: overall.failed,
        success_rate: overall.success_rate,
        p50_ms: overall.p50_ms,
        p95_ms: overall.p95_ms,
        last_run_at: overall.last_run_at,
        regression,
        stale: overall.total == 0,
    }
}

/// Walk every JSONL day-file in `[since, until]` (inclusive) and reduce
/// events into one snapshot per `run_id`. Sorted newest-first by
/// `completed_at` (falling back to `started_at`).
///
/// If `slug` is `Some`, only snapshots for that skill are returned. The
/// filter is applied AFTER the per-run-id merge so a `marked` event that
/// happened to omit the skill field still gets matched (rare, but it
/// preserves the same tolerance `find_run` has).
pub fn scan_snapshots(
    repo: &Path,
    slug: Option<&str>,
    since: NaiveDate,
    until: NaiveDate,
) -> Result<Vec<RunSnapshot>> {
    let runs_root = repo.join("runs");
    if !runs_root.exists() {
        return Ok(Vec::new());
    }
    let mut by_id: HashMap<String, RunSnapshot> = HashMap::new();
    let mut date = since;
    while date <= until {
        let file = day_file(repo, date.year(), date.month(), date.day());
        if file.exists() {
            let reader = BufReader::new(
                std::fs::File::open(&file)
                    .with_context(|| format!("open {}", file.display()))?,
            );
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                if line.trim().is_empty() {
                    continue;
                }
                let Some(ev) = parse_event_line(&line) else {
                    continue;
                };
                let run_id = ev.run_id.clone();
                let prior = by_id.remove(&run_id);
                by_id.insert(run_id, merge_event(prior, ev));
            }
        }
        date = match date.succ_opt() {
            Some(d) => d,
            None => break,
        };
    }

    let mut snapshots: Vec<RunSnapshot> = by_id
        .into_values()
        .filter(|s| match slug {
            Some(want) => s.skill.as_deref() == Some(want),
            None => true,
        })
        .map(finalize_duration)
        .collect();
    snapshots.sort_by(|a, b| {
        let ka = a.completed_at.or(a.started_at);
        let kb = b.completed_at.or(b.started_at);
        kb.cmp(&ka)
    });
    Ok(snapshots)
}

/// Result of aggregating a cohort of runs. Mirrors the cloud
/// `/skills/{id}/stats?group_by=...` response shape so `--json`
/// output looks identical across modes.
///
/// "Aborted" status (only ever written by the legacy `record_run` trait
/// path that no live code calls) is rolled into `failed` so the two
/// modes' bucket schemas line up. New code only ever writes
/// `succeeded` / `failed` via `knack mark`.
///
/// `key` is the cohort coordinate — one entry per dimension passed to
/// [`group_buckets`]. For `--group-by version` it's `{"version": "0.1.4"}`;
/// for `--group-by version,agent` it's `{"version": "0.1.4", "agent":
/// "claude-code"}`. `None` values mean the dimension was unset on those
/// rows (legacy data without version, or unidentified agent).
#[derive(Debug, Clone, Serialize)]
pub struct StatsBucket {
    pub key: BTreeMap<String, Option<String>>,
    /// Serialized as `runs_total` / `runs_succeeded` / `runs_failed` /
    /// `runs_unmarked` to match the cloud `StatsBucketDto` shape and
    /// the `agent.txt` envelope spec. Internal Rust callers still use
    /// the short names for terse arithmetic.
    #[serde(rename = "runs_total")]
    pub total: u64,
    #[serde(rename = "runs_succeeded")]
    pub succeeded: u64,
    #[serde(rename = "runs_failed")]
    pub failed: u64,
    #[serde(rename = "runs_unmarked")]
    pub unmarked: u64,
    /// Runs superseded without a verdict (auto-closed when a newer run
    /// of the same skill started). In `runs_total`, out of
    /// `success_rate` and `runs_unmarked` — mirrors the cloud
    /// semantics, where an abandoned mark is neither a verdict nor
    /// missing. Additive field; cloud parity consumers can ignore it.
    #[serde(rename = "runs_abandoned")]
    pub abandoned: u64,
    /// `succeeded / (succeeded + failed)`. `None` when there are no
    /// marked runs (avoids `0/0` and the misleading "0% success" framing
    /// that comes with it).
    pub success_rate: Option<f64>,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    /// Most recent activity timestamp in the cohort — preferred over
    /// the parent skill's `last_run_at` when judging cohort freshness.
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub top_notes: Vec<NoteCount>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NoteCount {
    pub note: String,
    pub count: u64,
}

/// One point in a [`group_time_buckets`] series. `bucket_start` and
/// `bucket_end` are inclusive on both ends, in the same timezone the
/// JSONL `at` field uses (UTC). `buckets` is the sub-grouping by
/// `dimensions` within this time slice (or one bucket with empty `key`
/// when no dimensions were requested).
#[derive(Debug, Clone, Serialize)]
pub struct TrendPoint {
    pub bucket_start: NaiveDate,
    pub bucket_end: NaiveDate,
    pub buckets: Vec<StatsBucket>,
}

/// Time-bucket interval. Daily slices line up with the JSONL day-files
/// (cheap, exact). Weekly slices start on Monday — ISO week alignment,
/// not US Sunday weeks.
#[derive(Debug, Clone, Copy)]
pub enum TrendInterval {
    Day,
    Week,
}

impl TrendInterval {
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "day" | "daily" | "d" => Ok(Self::Day),
            "week" | "weekly" | "w" => Ok(Self::Week),
            _ => Err(format!("unknown interval `{s}` (try: day, week)")),
        }
    }

    fn period_start(self, d: NaiveDate) -> NaiveDate {
        match self {
            Self::Day => d,
            // ISO week: Monday start. `weekday().num_days_from_monday()`
            // returns 0..6 with Mon=0.
            Self::Week => {
                d - Duration::days(d.weekday().num_days_from_monday() as i64)
            }
        }
    }

    fn period_end(self, start: NaiveDate) -> NaiveDate {
        match self {
            Self::Day => start,
            Self::Week => start + Duration::days(6),
        }
    }

    fn advance(self, start: NaiveDate) -> NaiveDate {
        match self {
            Self::Day => start.succ_opt().unwrap_or(start),
            Self::Week => start + Duration::days(7),
        }
    }
}

/// Bucket snapshots into time slices and, within each slice, into
/// sub-cohorts keyed by `dimensions`. Every interval in the requested
/// window gets a point even if it had zero runs — agents can render a
/// gap-free sparkline without padding the series themselves.
pub fn group_time_buckets(
    snapshots: &[RunSnapshot],
    interval: TrendInterval,
    dimensions: &[&str],
    since: NaiveDate,
    until: NaiveDate,
) -> Vec<TrendPoint> {
    let mut by_period: BTreeMap<NaiveDate, Vec<&RunSnapshot>> = BTreeMap::new();

    // Pre-seed every period in the window so empty intervals still
    // emit a point. Agents need this for gap-free trend rendering.
    let mut cursor = interval.period_start(since);
    let final_period = interval.period_start(until);
    while cursor <= final_period {
        by_period.entry(cursor).or_default();
        cursor = interval.advance(cursor);
    }

    for s in snapshots {
        // Use completed_at when available (most accurate "when did this
        // happen"), fall back to started_at for in-progress runs.
        let at = match s.completed_at.or(s.started_at) {
            Some(dt) => dt.date_naive(),
            None => continue,
        };
        if at < since || at > until {
            continue;
        }
        let period = interval.period_start(at);
        by_period.entry(period).or_default().push(s);
    }

    by_period
        .into_iter()
        .map(|(start, rows)| {
            let snapshots_in_period: Vec<RunSnapshot> = rows.into_iter().cloned().collect();
            let buckets = group_buckets(&snapshots_in_period, dimensions);
            TrendPoint {
                bucket_start: start,
                bucket_end: interval.period_end(start),
                buckets,
            }
        })
        .collect()
}

/// Group snapshots by an arbitrary set of dimensions and emit one
/// [`StatsBucket`] per unique key tuple.
///
/// Supported dimension names: `"version"`, `"agent"`. Other strings are
/// silently ignored — callers should validate at the CLI/route layer.
///
/// Buckets sort by their key tuple in lexicographic order; `None`
/// (missing-dimension) lands first within each level. Empty
/// `dimensions` returns one bucket whose `key` is `{}` — the
/// uniform "no grouping" shape the rest of the surface relies on.
pub fn group_buckets(snapshots: &[RunSnapshot], dimensions: &[&str]) -> Vec<StatsBucket> {
    let mut groups: BTreeMap<Vec<Option<String>>, Vec<&RunSnapshot>> = BTreeMap::new();
    for s in snapshots {
        let coords: Vec<Option<String>> = dimensions
            .iter()
            .map(|d| match *d {
                "version" => s.version.clone(),
                "agent" => s.agent.clone(),
                _ => None,
            })
            .collect();
        groups.entry(coords).or_default().push(s);
    }
    groups
        .into_iter()
        .map(|(coords, rows)| {
            let key: BTreeMap<String, Option<String>> = dimensions
                .iter()
                .zip(coords.into_iter())
                .map(|(d, v)| (d.to_string(), v))
                .collect();
            build_bucket(key, &rows)
        })
        .collect()
}

/// Aggregate one cohort into a [`StatsBucket`]. Public so callers like
/// `runs diff` and the time-bucketed `runs trend` can build buckets
/// from pre-filtered slices.
pub fn build_bucket(key: BTreeMap<String, Option<String>>, rows: &[&RunSnapshot]) -> StatsBucket {
    let total = rows.len() as u64;
    let mut succeeded = 0u64;
    let mut failed = 0u64;
    let mut unmarked = 0u64;
    let mut abandoned = 0u64;
    let mut durations: Vec<u64> = Vec::new();
    // Notes are bucketed by `normalize_note` so cosmetic variants ("Edge
    // case BROKE", "edge case broke ", "edge case broke.") collapse to one
    // top-3 entry. Display preserves the first-seen original form.
    let mut notes: HashMap<String, (String, u64)> = HashMap::new();
    let mut last_run_at: Option<chrono::DateTime<chrono::Utc>> = None;

    for s in rows {
        match s.status.as_str() {
            "succeeded" => succeeded += 1,
            // Roll legacy "aborted" into "failed" so back-compat data
            // from the dead `record_run` trait path doesn't create a
            // third bucket the cloud schema lacks. Notes still flow
            // through to the top-notes counter below.
            "failed" | "aborted" => failed += 1,
            // Superseded without a verdict — its own bucket, so it
            // neither drags success_rate nor masquerades as unmarked.
            "abandoned" => abandoned += 1,
            _ => unmarked += 1,
        }
        if let Some(d) = s.duration_ms {
            durations.push(d);
        }
        if matches!(s.status.as_str(), "failed" | "aborted") {
            if let Some(n) = &s.note {
                let trimmed = n.trim();
                if !trimmed.is_empty() {
                    let key = normalize_note(trimmed);
                    if !key.is_empty() {
                        let entry = notes
                            .entry(key)
                            .or_insert_with(|| (trimmed.to_string(), 0));
                        entry.1 += 1;
                    }
                }
            }
        }
        let row_at = s.completed_at.or(s.started_at);
        if let Some(at) = row_at {
            last_run_at = Some(match last_run_at {
                Some(prev) if prev >= at => prev,
                _ => at,
            });
        }
    }

    let marked = succeeded + failed;
    let success_rate = if marked > 0 {
        Some(succeeded as f64 / marked as f64)
    } else {
        None
    };

    durations.sort_unstable();
    let p50_ms = percentile(&durations, 0.50);
    let p95_ms = percentile(&durations, 0.95);

    let mut top_notes: Vec<NoteCount> = notes
        .into_values()
        .map(|(note, count)| NoteCount { note, count })
        .collect();
    top_notes.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.note.cmp(&b.note)));
    top_notes.truncate(3);

    StatsBucket {
        key,
        total,
        succeeded,
        failed,
        unmarked,
        abandoned,
        success_rate,
        p50_ms,
        p95_ms,
        last_run_at,
        top_notes,
    }
}

/// Bucket key for `top_notes` clustering. Lowercase, collapse internal
/// whitespace, strip trailing ASCII sentence punctuation.
fn normalize_note(raw: &str) -> String {
    let lowered = raw.to_lowercase();
    let collapsed = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.trim_end_matches(|c: char| ".!?,;:".contains(c)).to_string()
}

fn percentile(sorted: &[u64], q: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted.get(idx).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn snap_full(
        version: &str,
        agent: &str,
        status: &str,
        ms: Option<u64>,
        note: Option<&str>,
    ) -> RunSnapshot {
        let at = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        RunSnapshot {
            run_id: format!("{}-{}-{}", version, agent, status),
            skill: Some("triage".into()),
            version: Some(version.into()),
            agent: Some(agent.into()),
            inputs: vec![],
            outputs: vec![],
            status: status.into(),
            note: note.map(|s| s.into()),
            started_at: Some(at),
            completed_at: ms.map(|_| at),
            duration_ms: ms,
        }
    }

    fn snap(version: &str, status: &str, ms: Option<u64>, note: Option<&str>) -> RunSnapshot {
        snap_full(version, "claude-code", status, ms, note)
    }

    fn version_key(v: &str) -> BTreeMap<String, Option<String>> {
        let mut m = BTreeMap::new();
        m.insert("version".into(), Some(v.into()));
        m
    }

    #[test]
    fn bucket_counts_and_rate() {
        let rows = vec![
            snap("0.1.0", "succeeded", Some(100), None),
            snap("0.1.0", "succeeded", Some(200), None),
            snap("0.1.0", "failed", Some(300), Some("timeout")),
            snap("0.1.0", "started", None, None),
        ];
        let refs: Vec<&RunSnapshot> = rows.iter().collect();
        let b = build_bucket(version_key("0.1.0"), &refs);
        assert_eq!(b.total, 4);
        assert_eq!(b.succeeded, 2);
        assert_eq!(b.failed, 1);
        assert_eq!(b.unmarked, 1);
        assert!((b.success_rate.unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(b.top_notes.len(), 1);
        assert_eq!(b.top_notes[0].note, "timeout");
        assert_eq!(b.top_notes[0].count, 1);
        assert_eq!(b.p50_ms, Some(200));
        assert!(b.last_run_at.is_some());
        assert_eq!(b.key.get("version").unwrap().as_deref(), Some("0.1.0"));
    }

    #[test]
    fn legacy_aborted_status_rolls_into_failed() {
        let rows = vec![
            snap("0.1.0", "aborted", Some(500), Some("user killed it")),
            snap("0.1.0", "succeeded", Some(100), None),
        ];
        let refs: Vec<&RunSnapshot> = rows.iter().collect();
        let b = build_bucket(version_key("0.1.0"), &refs);
        assert_eq!(b.failed, 1);
        assert_eq!(b.succeeded, 1);
        assert!((b.success_rate.unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(b.top_notes[0].note, "user killed it");
    }

    #[test]
    fn bucket_no_marked_runs_returns_no_rate() {
        let rows = vec![snap("0.2.0", "started", None, None)];
        let refs: Vec<&RunSnapshot> = rows.iter().collect();
        let b = build_bucket(version_key("0.2.0"), &refs);
        assert!(b.success_rate.is_none());
    }

    #[test]
    fn group_buckets_by_version_splits_cohorts() {
        let rows = vec![
            snap("0.1.0", "succeeded", Some(100), None),
            snap("0.1.0", "failed", Some(200), Some("a")),
            snap("0.2.0", "succeeded", Some(50), None),
        ];
        let groups = group_buckets(&rows, &["version"]);
        assert_eq!(groups.len(), 2);
        let v010 = groups
            .iter()
            .find(|b| b.key.get("version").unwrap().as_deref() == Some("0.1.0"))
            .unwrap();
        let v020 = groups
            .iter()
            .find(|b| b.key.get("version").unwrap().as_deref() == Some("0.2.0"))
            .unwrap();
        assert_eq!(v010.total, 2);
        assert_eq!(v020.total, 1);
        assert_eq!(v020.succeeded, 1);
    }

    #[test]
    fn group_buckets_cross_tab_version_and_agent() {
        let rows = vec![
            snap_full("0.1.4", "claude-code", "succeeded", Some(100), None),
            snap_full("0.1.4", "claude-code", "succeeded", Some(120), None),
            snap_full("0.1.4", "cursor", "failed", Some(900), Some("schema")),
            snap_full("0.1.3", "claude-code", "succeeded", Some(200), None),
        ];
        let groups = group_buckets(&rows, &["version", "agent"]);
        assert_eq!(groups.len(), 3);
        // One bucket should be {version: 0.1.4, agent: claude-code} with 2 runs
        let cohort = groups
            .iter()
            .find(|b| {
                b.key.get("version").unwrap().as_deref() == Some("0.1.4")
                    && b.key.get("agent").unwrap().as_deref() == Some("claude-code")
            })
            .unwrap();
        assert_eq!(cohort.total, 2);
        assert_eq!(cohort.succeeded, 2);
        assert!(cohort.key.contains_key("version"));
        assert!(cohort.key.contains_key("agent"));
    }

    #[test]
    fn group_buckets_no_dimensions_returns_one_overall_bucket() {
        let rows = vec![
            snap("0.1.0", "succeeded", Some(100), None),
            snap("0.2.0", "failed", Some(200), Some("x")),
        ];
        let groups = group_buckets(&rows, &[]);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].key.is_empty());
        assert_eq!(groups[0].total, 2);
        assert_eq!(groups[0].succeeded, 1);
        assert_eq!(groups[0].failed, 1);
    }

    #[test]
    fn top_notes_orders_by_count_then_text() {
        let rows = vec![
            snap("0.1.0", "failed", Some(100), Some("timeout")),
            snap("0.1.0", "failed", Some(100), Some("timeout")),
            snap("0.1.0", "failed", Some(100), Some("oom")),
            snap("0.1.0", "failed", Some(100), Some("oom")),
            snap("0.1.0", "failed", Some(100), Some("schema")),
        ];
        let refs: Vec<&RunSnapshot> = rows.iter().collect();
        let b = build_bucket(version_key("0.1.0"), &refs);
        assert_eq!(b.top_notes.len(), 3);
        assert_eq!(b.top_notes[0].note, "oom");
        assert_eq!(b.top_notes[1].note, "timeout");
        assert_eq!(b.top_notes[2].note, "schema");
    }

    #[test]
    fn top_notes_collapses_cosmetic_variants() {
        let rows = vec![
            snap("0.1.0", "failed", Some(100), Some("Edge case BROKE")),
            snap("0.1.0", "failed", Some(100), Some("edge case broke ")),
            snap("0.1.0", "failed", Some(100), Some("edge case broke.")),
        ];
        let refs: Vec<&RunSnapshot> = rows.iter().collect();
        let b = build_bucket(version_key("0.1.0"), &refs);
        assert_eq!(b.top_notes.len(), 1);
        assert_eq!(b.top_notes[0].count, 3);
        // First-seen original form is preserved for display.
        assert_eq!(b.top_notes[0].note, "Edge case BROKE");
    }
}
