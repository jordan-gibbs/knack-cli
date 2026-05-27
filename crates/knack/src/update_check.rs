//! Passive update-check notifier.
//!
//! On every CLI invocation, `main.rs` spawns a background task that
//! refreshes `~/.knack/update-check.json` from
//! `${KNACK_R2_BASE}/cli/latest/version.txt` if the cache is missing or
//! older than 24h. After dispatch returns, the same `main.rs` calls
//! [`print_update_banner_once`] which reads the (possibly just-refreshed)
//! cache and writes a one-line stderr notice when a newer version is
//! available.
//!
//! Stale-while-revalidate: the banner decision uses whatever the cache
//! holds *now*, even if the background refresh is still racing. The
//! refresh is purely for the *next* invocation. We never block the
//! current command on a network round-trip.
//!
//! Suppressors (any of these short-circuits the banner):
//!
//!   * `--json` (banner would corrupt parseable stderr/stdout output)
//!   * `--quiet`
//!   * `KNACK_NO_UPDATE_CHECK=1` (env opt-out, for CI)
//!
//! The banner is intentionally NOT TTY-gated. Agents tail CLI stderr
//! programmatically, and that audience is the whole reason this exists.

use std::io;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use console::Term;
use serde::{Deserialize, Serialize};

use crate::output::OutputMode;

const DEFAULT_R2_BASE: &str = "https://cli.getknack.ai";
const VERSION_PATH: &str = "/cli/latest/version.txt";
const TTL_HOURS: i64 = 24;
const FETCH_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

static UPDATE_BANNER_PRINTED: OnceLock<()> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFile {
    checked_at: DateTime<Utc>,
    latest_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_version_at_check: Option<String>,
}

/// Cache file location. `$KNACK_UPDATE_CHECK_FILE` wins for tests and
/// advanced users; otherwise `~/.knack/update-check.json`, with
/// `$XDG_CONFIG_HOME/knack/update-check.json` as a last-resort fallback.
pub fn cache_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("KNACK_UPDATE_CHECK_FILE") {
        return Some(PathBuf::from(custom));
    }
    if let Some(home) = dirs::home_dir() {
        return Some(home.join(".knack").join("update-check.json"));
    }
    dirs::config_dir().map(|c| c.join("knack").join("update-check.json"))
}

fn opt_out() -> bool {
    matches!(
        std::env::var("KNACK_NO_UPDATE_CHECK").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    )
}

fn r2_base() -> String {
    std::env::var("KNACK_R2_BASE").unwrap_or_else(|_| DEFAULT_R2_BASE.to_string())
}

/// Parse `MAJOR.MINOR.PATCH` (optionally `v`-prefixed) into a tuple.
/// Pre-release / build metadata (`-rc.1`, `+build.7`) is tolerated and
/// ignored. Returns `None` on any parse failure so a malformed
/// `version.txt` (or a dev build with a non-standard tag) never spams
/// the banner with a wrong comparison.
fn parse_triple(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches('v');
    let head = s.split(['-', '+']).next().unwrap_or(s);
    let mut parts = head.split('.');
    let a = parts.next()?.parse().ok()?;
    let b = parts.next()?.parse().ok()?;
    let c = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((a, b, c))
}

fn read_cache() -> Option<CacheFile> {
    let path = cache_path()?;
    let body = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&body).ok()
}

fn write_cache(cache: &CacheFile) -> io::Result<()> {
    let Some(path) = cache_path() else {
        return Err(io::Error::other(
            "no home or config dir for update-check.json",
        ));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(cache)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn is_stale(cache: &CacheFile) -> bool {
    let age = Utc::now().signed_duration_since(cache.checked_at);
    age.num_hours() >= TTL_HOURS
}

/// Banner text if the cached `latest_version` is newer than `current`,
/// else `None`. Honors `--json`, `--quiet`, and `KNACK_NO_UPDATE_CHECK`.
pub fn banner_line(current: &str, mode: OutputMode) -> Option<String> {
    if mode.json || mode.quiet || opt_out() {
        return None;
    }
    let cache = read_cache()?;
    let cur = parse_triple(current)?;
    let latest = parse_triple(&cache.latest_version)?;
    if latest <= cur {
        return None;
    }
    Some(format!(
        "knack {} available (you have {}). Run `knack upgrade` to update.",
        cache.latest_version, current
    ))
}

/// Print the banner once per process. Subsequent calls are no-ops via
/// the `UPDATE_BANNER_PRINTED` guard. Intended to be called from
/// `main.rs` after `dispatch` returns so the banner is the last stderr
/// line agents see.
pub fn print_update_banner_once(mode: OutputMode, current: &str) {
    if UPDATE_BANNER_PRINTED.set(()).is_err() {
        return;
    }
    let Some(line) = banner_line(current, mode) else {
        return;
    };
    let _ = Term::stderr().write_line(&line);
}

/// Fire-and-forget background refresh of the version cache.
///
/// Spawned by `main.rs` before `dispatch`. Returns immediately; the
/// task completes (or silently fails) on its own. Never blocks the
/// main command.
pub fn spawn_refresh(current: String) {
    if opt_out() {
        return;
    }
    tokio::spawn(async move {
        let _ = refresh(current).await;
    });
}

async fn refresh(current: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(cache) = read_cache() {
        if !is_stale(&cache) {
            return Ok(());
        }
    }
    let url = format!("{}{}", r2_base().trim_end_matches('/'), VERSION_PATH);
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()?;
    let resp = client.get(&url).send().await?.error_for_status()?;
    let body = resp.text().await?;
    let latest = body.lines().next().unwrap_or("").trim().to_string();
    if parse_triple(&latest).is_none() {
        return Ok(());
    }
    let cache = CacheFile {
        checked_at: Utc::now(),
        latest_version: latest,
        current_version_at_check: Some(current),
    };
    write_cache(&cache)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// Serialize env-var mutation across tests in this module. `cargo
    /// test` runs tests in parallel by default; without this lock,
    /// `banner_suppressed_by_env_opt_out` setting `KNACK_NO_UPDATE_CHECK`
    /// races with `banner_some_when_cache_newer` reading it and the
    /// latter spuriously returns None. Per-test mutex holds for the
    /// test's lifetime via the returned guard.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Point `cache_path()` at a fresh tempdir per test so we never
    /// touch the developer's real `~/.knack/update-check.json`. Holds
    /// the env-var mutex for the test's duration.
    fn isolate() -> (MutexGuard<'static, ()>, TempDir, PathBuf) {
        let lock = env_lock();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("update-check.json");
        unsafe {
            std::env::set_var("KNACK_UPDATE_CHECK_FILE", &path);
            std::env::remove_var("KNACK_NO_UPDATE_CHECK");
        }
        (lock, dir, path)
    }

    fn write_cache_for_test(path: &PathBuf, latest: &str, checked_at: DateTime<Utc>) {
        let cache = CacheFile {
            checked_at,
            latest_version: latest.into(),
            current_version_at_check: None,
        };
        std::fs::write(path, serde_json::to_string(&cache).unwrap()).unwrap();
    }

    #[test]
    fn parse_triple_handles_plain_semver() {
        assert_eq!(parse_triple("0.7.3"), Some((0, 7, 3)));
        assert_eq!(parse_triple("v0.7.3"), Some((0, 7, 3)));
        assert_eq!(parse_triple("12.34.56"), Some((12, 34, 56)));
    }

    #[test]
    fn parse_triple_strips_prerelease_and_build() {
        assert_eq!(parse_triple("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_triple("1.2.3+build.7"), Some((1, 2, 3)));
    }

    #[test]
    fn parse_triple_rejects_garbage() {
        assert_eq!(parse_triple("not-a-version"), None);
        assert_eq!(parse_triple("1.2"), None);
        assert_eq!(parse_triple("1.2.3.4"), None);
        assert_eq!(parse_triple(""), None);
    }

    #[test]
    fn banner_none_when_cache_missing() {
        let (_lock, _dir, _path) = isolate();
        assert!(banner_line("0.5.0", OutputMode::human()).is_none());
    }

    #[test]
    fn banner_some_when_cache_newer() {
        let (_lock, _dir, path) = isolate();
        write_cache_for_test(&path, "0.7.3", Utc::now());
        let line = banner_line("0.5.0", OutputMode::human()).expect("banner expected");
        assert!(line.contains("0.7.3"));
        assert!(line.contains("knack upgrade"));
    }

    #[test]
    fn banner_none_when_cache_equal_or_older() {
        let (_lock, _dir, path) = isolate();
        write_cache_for_test(&path, "0.5.0", Utc::now());
        assert!(banner_line("0.5.0", OutputMode::human()).is_none());
        write_cache_for_test(&path, "0.4.0", Utc::now());
        assert!(banner_line("0.5.0", OutputMode::human()).is_none());
    }

    #[test]
    fn banner_suppressed_in_json_mode() {
        let (_lock, _dir, path) = isolate();
        write_cache_for_test(&path, "99.0.0", Utc::now());
        let mode = OutputMode {
            json: true,
            quiet: false,
            no_color: true,
        };
        assert!(banner_line("0.5.0", mode).is_none());
    }

    #[test]
    fn banner_suppressed_in_quiet_mode() {
        let (_lock, _dir, path) = isolate();
        write_cache_for_test(&path, "99.0.0", Utc::now());
        let mode = OutputMode {
            json: false,
            quiet: true,
            no_color: false,
        };
        assert!(banner_line("0.5.0", mode).is_none());
    }

    #[test]
    fn banner_suppressed_by_env_opt_out() {
        let (_lock, _dir, path) = isolate();
        write_cache_for_test(&path, "99.0.0", Utc::now());
        unsafe {
            std::env::set_var("KNACK_NO_UPDATE_CHECK", "1");
        }
        let none = banner_line("0.5.0", OutputMode::human());
        unsafe {
            std::env::remove_var("KNACK_NO_UPDATE_CHECK");
        }
        assert!(none.is_none());
    }

    #[test]
    fn is_stale_threshold() {
        let fresh = CacheFile {
            checked_at: Utc::now(),
            latest_version: "0.7.3".into(),
            current_version_at_check: None,
        };
        assert!(!is_stale(&fresh));

        let old = CacheFile {
            checked_at: Utc::now() - chrono::Duration::hours(25),
            latest_version: "0.7.3".into(),
            current_version_at_check: None,
        };
        assert!(is_stale(&old));
    }
}
