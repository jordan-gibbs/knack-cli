use anyhow::{Context, Result};
use chrono::Datelike;
use knack_types::RunLog;
use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Append a run log line to `<repo>/runs/<yyyy-mm>/<yyyy-mm-dd>.jsonl`.
/// The CLI buffers these locally and a separate task batch-pushes them.
pub fn append_run(repo: &Path, log: &RunLog) -> Result<()> {
    let date = log.started_at.date_naive();
    let month_dir = repo
        .join("runs")
        .join(format!("{:04}-{:02}", date.year(), date.month()));
    create_dir_all(&month_dir).context("create runs month dir")?;

    let file = month_dir.join(format!(
        "{:04}-{:02}-{:02}.jsonl",
        date.year(),
        date.month(),
        date.day()
    ));

    let mut handle = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .with_context(|| format!("open {}", file.display()))?;

    let line = serde_json::to_string(log).context("serialize run log")?;
    writeln!(handle, "{}", line).context("write run log")?;
    Ok(())
}
