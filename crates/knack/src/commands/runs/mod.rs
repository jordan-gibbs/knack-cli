//! `knack runs` — read the telemetry log: list, show, stats, diff.
//!
//! Self-host: scans local JSONL via `knack_backend_github::aggregate`.
//! Cloud: hits `/runs/by-skill/{id}` and `/skills/{id}/stats`.
//! Output: human table + `--json` envelope, identical shape across modes.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use clap::Subcommand;

use crate::api::ApiClient;
use crate::errors::CliResult;
use crate::output::OutputMode;

pub mod diff;
pub mod flush;
pub mod list;
pub mod overview;
pub mod show;
pub mod stats;
pub mod trend;

#[derive(Debug, Subcommand)]
pub enum RunsCmd {
    /// Page through past runs (filter by status, version, agent, date, note text).
    List(list::ListArgs),
    /// Show a single run snapshot by run-id.
    Show(show::ShowArgs),
    /// Aggregate stats over one or more dimensions (default: version).
    Stats(stats::StatsArgs),
    /// Side-by-side compare two versions of a skill: success-rate / duration deltas + top notes.
    Diff(diff::DiffArgs),
    /// Time-series stats: daily/weekly success rate, with optional within-slice grouping.
    Trend(trend::TrendArgs),
    /// Portfolio view: per-skill snapshot + regression + staleness flags.
    Overview(overview::OverviewArgs),
    /// Self-host: push queued telemetry commits now (no-op on cloud).
    Flush(flush::FlushArgs),
}

pub async fn run(cmd: RunsCmd, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match cmd {
        RunsCmd::List(a) => list::run(a, client, mode).await,
        RunsCmd::Show(a) => show::run(a, client, mode).await,
        RunsCmd::Stats(a) => stats::run(a, client, mode).await,
        RunsCmd::Diff(a) => diff::run(a, client, mode).await,
        RunsCmd::Trend(a) => trend::run(a, client, mode).await,
        RunsCmd::Overview(a) => overview::run(a, client, mode).await,
        RunsCmd::Flush(a) => flush::run(a, client, mode).await,
    }
}

/// Shared `--since` / `--until` parser. Accepts `YYYY-MM-DD` or `<N>d`
/// (days ago). Defaults to `[today - DEFAULT_LOOKBACK_DAYS, today]`.
pub(crate) fn parse_window(
    since: Option<&str>,
    until: Option<&str>,
) -> Result<(NaiveDate, NaiveDate), String> {
    let today = Utc::now().date_naive();
    let since = match since {
        Some(s) => parse_date_or_offset(s, today)?,
        None => today - Duration::days(knack_backend_github::DEFAULT_LOOKBACK_DAYS),
    };
    let until = match until {
        Some(s) => parse_date_or_offset(s, today)?,
        None => today,
    };
    if since > until {
        return Err(format!("since ({since}) is after until ({until})"));
    }
    Ok((since, until))
}

fn parse_date_or_offset(s: &str, today: NaiveDate) -> Result<NaiveDate, String> {
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(d);
    }
    if let Some(num) = s.strip_suffix('d') {
        if let Ok(n) = num.parse::<i64>() {
            return Ok(today - Duration::days(n));
        }
    }
    Err(format!("can't parse `{s}` as date or offset"))
}

/// Convert a date to a UTC datetime at midnight (or end-of-day).
pub(crate) fn naive_to_utc(d: NaiveDate, end_of_day: bool) -> DateTime<Utc> {
    let time = if end_of_day {
        chrono::NaiveTime::from_hms_opt(23, 59, 59).unwrap()
    } else {
        chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap()
    };
    DateTime::<Utc>::from_naive_utc_and_offset(d.and_time(time), Utc)
}
