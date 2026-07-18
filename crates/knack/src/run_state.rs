//! Recent-run registry — the local state behind `knack mark last` and
//! short run-id prefixes.
//!
//! `knack run` appends every run started from this machine; `knack mark`
//! reads it to resolve `last` (newest unmarked run for the active
//! backend) and unique id prefixes (`knack mark f887 succeeded`), then
//! flips `marked` on the entries it closed. This is a convenience cache,
//! not telemetry: the durable record is the server Run row (cloud) or
//! `runs/*.jsonl` (self-host). Losing it costs nothing but the shorthand.
//!
//! Layout (JSON):
//!
//! ```json
//! {
//!   "version": 1,
//!   "entries": [
//!     {"run_id": "f8877899-…", "slug": "monthly-close", "backend": "cloud",
//!      "started_at": "2026-07-18T14:32:18Z", "marked": false}
//!   ],
//!   "updated_at": "2026-07-18T14:32:18Z"
//! }
//! ```
//!
//! Ring buffer capped at [`MAX_ENTRIES`], newest last. Storage location,
//! in order (mirrors `installed.json` / `linked.json`):
//!   1. `$KNACK_RECENT_RUNS_FILE` (env override — used by tests)
//!   2. `~/.knack/recent_runs.json`
//!   3. fallback: `$XDG_CONFIG_HOME/knack/recent_runs.json`
//!
//! Writes are atomic via tempfile + rename. A malformed file self-heals
//! to empty (with a stderr note) instead of erroring: a corrupt cache
//! must never block `knack mark <full-uuid>`.

use std::fs;
use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;

/// Ring-buffer cap. 50 comfortably covers a multi-day agent session
/// while keeping the file trivially small.
const MAX_ENTRIES: usize = 50;

/// Shortest accepted id prefix. Below this, collisions get likely and
/// typos get expensive.
pub const MIN_PREFIX_LEN: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEntry {
    pub run_id: String,
    pub slug: String,
    /// `"cloud"` or `"github"` — which backend started the run, so
    /// `mark last` never closes a cloud run while in self-host mode.
    pub backend: String,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub marked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentRuns {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<RunEntry>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl Default for RecentRuns {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            entries: Vec::new(),
            updated_at: None,
        }
    }
}

/// Outcome of resolving a short id prefix against the registry.
#[derive(Debug, PartialEq, Eq)]
pub enum PrefixMatch {
    One(String),
    Ambiguous(Vec<String>),
    None,
}

/// Resolve the on-disk location for the registry.
pub fn record_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("KNACK_RECENT_RUNS_FILE") {
        return Some(PathBuf::from(custom));
    }
    if let Some(home) = dirs::home_dir() {
        return Some(home.join(".knack").join("recent_runs.json"));
    }
    dirs::config_dir().map(|c| c.join("knack").join("recent_runs.json"))
}

/// Read the registry. Missing file → empty (first run). Malformed file
/// → empty with a stderr note; see module docs for why we self-heal.
pub fn load() -> RecentRuns {
    let Some(path) = record_path() else {
        return RecentRuns::default();
    };
    match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            eprintln!(
                "knack: {} was malformed ({e}); resetting the recent-run cache",
                path.display()
            );
            RecentRuns::default()
        }),
        Err(_) => RecentRuns::default(),
    }
}

/// Persist atomically (tempfile + rename in the same dir).
fn save(rec: &RecentRuns) -> io::Result<()> {
    let Some(path) = record_path() else {
        return Err(io::Error::other(
            "no home or config dir available for recent_runs.json",
        ));
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut record = rec.clone();
    record.version = SCHEMA_VERSION;
    record.updated_at = Some(Utc::now());
    let body = serde_json::to_string_pretty(&record)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Append a freshly started run, evicting the oldest entries past the
/// cap. Call sites treat failure as non-fatal (this is a cache), so the
/// io::Result is advisory.
pub fn push_run(run_id: &str, slug: &str, backend: &str) -> io::Result<()> {
    let mut rec = load();
    rec.entries.push(RunEntry {
        run_id: run_id.to_string(),
        slug: slug.to_string(),
        backend: backend.to_string(),
        started_at: Utc::now(),
        marked: false,
    });
    if rec.entries.len() > MAX_ENTRIES {
        let excess = rec.entries.len() - MAX_ENTRIES;
        rec.entries.drain(..excess);
    }
    save(&rec)
}

/// Newest unmarked run started by this machine for `backend`. This is
/// what `knack mark last …` closes.
pub fn last_open(backend: &str) -> Option<RunEntry> {
    load()
        .entries
        .iter()
        .rev()
        .find(|e| !e.marked && e.backend == backend)
        .cloned()
}

/// All unmarked runs for `backend` other than `except_run_id`, newest
/// first. Used by the auto-close pass in `knack run`.
pub fn open_runs_except(backend: &str, except_run_id: &str) -> Vec<RunEntry> {
    let mut out: Vec<RunEntry> = load()
        .entries
        .into_iter()
        .filter(|e| !e.marked && e.backend == backend && e.run_id != except_run_id)
        .collect();
    out.reverse();
    out
}

/// The subset of [`open_runs_except`] for one skill — the runs a new
/// `knack run <slug>` supersedes. Restricting to the same slug is
/// deliberate: interleaved runs of *different* skills are legitimate
/// parallel work and must stay open.
pub fn stale_open_runs(backend: &str, slug: &str, except_run_id: &str) -> Vec<RunEntry> {
    open_runs_except(backend, except_run_id)
        .into_iter()
        .filter(|e| e.slug == slug)
        .collect()
}

/// Resolve a short prefix against recorded run ids.
pub fn resolve_prefix(prefix: &str) -> PrefixMatch {
    let matches: Vec<String> = load()
        .entries
        .iter()
        .filter(|e| e.run_id.starts_with(prefix))
        .map(|e| e.run_id.clone())
        .collect();
    match matches.len() {
        0 => PrefixMatch::None,
        1 => PrefixMatch::One(matches.into_iter().next().unwrap()),
        _ => {
            let mut m = matches;
            m.dedup();
            if m.len() == 1 {
                PrefixMatch::One(m.into_iter().next().unwrap())
            } else {
                PrefixMatch::Ambiguous(m)
            }
        }
    }
}

/// Flip `marked` on every entry whose id is in `ids`. Best-effort.
pub fn set_marked(ids: &[String]) {
    let mut rec = load();
    let mut changed = false;
    for e in rec.entries.iter_mut() {
        if !e.marked && ids.iter().any(|id| id == &e.run_id) {
            e.marked = true;
            changed = true;
        }
    }
    if changed {
        let _ = save(&rec);
    }
}

/// True when `s` looks like a candidate id prefix: hex/dash charset and
/// long enough to be plausibly unique. Anything else is a typo we should
/// reject with the usual UUID error instead of a confusing prefix miss.
pub fn is_plausible_prefix(s: &str) -> bool {
    s.len() >= MIN_PREFIX_LEN
        && s.len() < 36
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};
    use tempfile::TempDir;

    /// Serialize access to KNACK_RECENT_RUNS_FILE across parallel tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn isolate() -> (MutexGuard<'static, ()>, TempDir, PathBuf) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("recent_runs.json");
        // SAFETY: ENV_LOCK serializes every test touching this env var.
        unsafe {
            std::env::set_var("KNACK_RECENT_RUNS_FILE", &path);
        }
        (guard, dir, path)
    }

    #[test]
    fn push_and_last_open_round_trip() {
        let (_g, _dir, _path) = isolate();
        push_run("aaaa1111-0000-0000-0000-000000000001", "s1", "cloud").unwrap();
        push_run("bbbb2222-0000-0000-0000-000000000002", "s2", "cloud").unwrap();
        let last = last_open("cloud").unwrap();
        assert_eq!(last.run_id, "bbbb2222-0000-0000-0000-000000000002");
        assert_eq!(last.slug, "s2");
    }

    #[test]
    fn last_open_scopes_to_backend() {
        let (_g, _dir, _path) = isolate();
        push_run("aaaa1111-0000-0000-0000-000000000001", "s1", "github").unwrap();
        assert!(last_open("cloud").is_none());
        assert_eq!(
            last_open("github").unwrap().run_id,
            "aaaa1111-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn set_marked_hides_entry_from_last_open() {
        let (_g, _dir, _path) = isolate();
        push_run("aaaa1111-0000-0000-0000-000000000001", "s1", "cloud").unwrap();
        push_run("bbbb2222-0000-0000-0000-000000000002", "s2", "cloud").unwrap();
        set_marked(&["bbbb2222-0000-0000-0000-000000000002".to_string()]);
        assert_eq!(
            last_open("cloud").unwrap().run_id,
            "aaaa1111-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn prefix_resolution_unique_ambiguous_none() {
        let (_g, _dir, _path) = isolate();
        push_run("aaaa1111-0000-0000-0000-000000000001", "s1", "cloud").unwrap();
        push_run("aaab2222-0000-0000-0000-000000000002", "s2", "cloud").unwrap();
        assert_eq!(
            resolve_prefix("aaaa"),
            PrefixMatch::One("aaaa1111-0000-0000-0000-000000000001".into())
        );
        match resolve_prefix("aaa") {
            PrefixMatch::Ambiguous(v) => assert_eq!(v.len(), 2),
            other => panic!("expected ambiguous, got {other:?}"),
        }
        assert_eq!(resolve_prefix("ffff"), PrefixMatch::None);
    }

    #[test]
    fn ring_buffer_caps_entries() {
        let (_g, _dir, _path) = isolate();
        for i in 0..60 {
            push_run(
                &format!("{i:08}-0000-0000-0000-000000000000"),
                "s",
                "cloud",
            )
            .unwrap();
        }
        assert_eq!(load().entries.len(), 50);
        // Oldest evicted, newest kept.
        assert_eq!(
            load().entries.last().unwrap().run_id,
            "00000059-0000-0000-0000-000000000000"
        );
        assert_eq!(
            load().entries.first().unwrap().run_id,
            "00000010-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn malformed_file_self_heals() {
        let (_g, _dir, path) = isolate();
        fs::write(&path, "{not json").unwrap();
        assert!(load().entries.is_empty());
        push_run("aaaa1111-0000-0000-0000-000000000001", "s1", "cloud").unwrap();
        assert_eq!(load().entries.len(), 1);
    }

    #[test]
    fn open_runs_except_skips_marked_and_current() {
        let (_g, _dir, _path) = isolate();
        push_run("aaaa1111-0000-0000-0000-000000000001", "s1", "cloud").unwrap();
        push_run("bbbb2222-0000-0000-0000-000000000002", "s2", "cloud").unwrap();
        push_run("cccc3333-0000-0000-0000-000000000003", "s3", "cloud").unwrap();
        set_marked(&["aaaa1111-0000-0000-0000-000000000001".to_string()]);
        let open = open_runs_except("cloud", "cccc3333-0000-0000-0000-000000000003");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].run_id, "bbbb2222-0000-0000-0000-000000000002");
    }

    #[test]
    fn stale_open_runs_scopes_to_slug_and_skips_current() {
        let (_g, _dir, _path) = isolate();
        push_run("aaaa1111-0000-0000-0000-000000000001", "alpha", "cloud").unwrap();
        push_run("bbbb2222-0000-0000-0000-000000000002", "beta", "cloud").unwrap();
        push_run("cccc3333-0000-0000-0000-000000000003", "alpha", "cloud").unwrap();
        let stale = stale_open_runs("cloud", "alpha", "cccc3333-0000-0000-0000-000000000003");
        // Only the older `alpha` run is superseded — `beta` stays open.
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].run_id, "aaaa1111-0000-0000-0000-000000000001");
    }

    #[test]
    fn plausible_prefix_charset_and_length() {
        assert!(is_plausible_prefix("f887"));
        assert!(is_plausible_prefix("f8877899-cd2a"));
        assert!(!is_plausible_prefix("f88")); // too short
        assert!(!is_plausible_prefix("last")); // 's' and 't' not hex
        assert!(!is_plausible_prefix(
            "f8877899-cd2a-4749-b38a-ae885a1b417c"
        )); // full uuid, not a prefix
    }
}
