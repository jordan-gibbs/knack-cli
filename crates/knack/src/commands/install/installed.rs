//! Persistent record of which agents `knack install` has touched.
//!
//! Written by `knack install` on success; read by `knack sync` and by
//! the post-pull / post-publish hooks so we know which runtime shim
//! roots to refresh without re-running autodetect every time.
//!
//! Layout (JSON):
//!
//! ```json
//! {
//!   "version": 1,
//!   "agents": [
//!     {"slug": "claude", "scope": "home",    "path": "/Users/jane/.claude/CLAUDE.md"},
//!     {"slug": "cursor", "scope": "project", "path": "/repos/foo/.cursor/rules/knack.mdc"}
//!   ],
//!   "updated_at": "2026-05-12T14:32:18Z"
//! }
//! ```
//!
//! Storage location, in order:
//!   1. `$KNACK_INSTALLED_FILE` (env override — used by tests)
//!   2. `~/.knack/installed.json`
//!   3. fallback: `$XDG_CONFIG_HOME/knack/installed.json` on Linux when
//!      no home directory is visible (rare; matches the rest of the CLI)
//!
//! Writes are atomic via tempfile + rename so a crash mid-write never
//! leaves a half-written JSON blob on disk.

use std::fs;
use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;

/// Which scope this agent's shims live in.
///
/// `Home` → shims belong under `~/.<agent>/...` (HOME-shared); written
/// when the user passed `--global`.
/// `Project` → shims belong under `<workspace>/.<agent>/...` (per-repo).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Home,
    Project,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Home => "home",
            Scope::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    /// Slug from `targets::AgentTarget::name`.
    pub slug: String,
    pub scope: Scope,
    /// Path to the install config file (CLAUDE.md, AGENTS.md, knack.mdc).
    /// We keep this in the record so a future `knack sync --purge` can
    /// find the install block without re-running config_path resolvers
    /// — handy when CLAUDE_CONFIG_DIR or CWD has changed since install.
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledRecord {
    pub version: u32,
    #[serde(default)]
    pub agents: Vec<AgentEntry>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl Default for InstalledRecord {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            agents: Vec::new(),
            updated_at: None,
        }
    }
}

/// Resolve the on-disk location for the record.
pub fn record_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("KNACK_INSTALLED_FILE") {
        return Some(PathBuf::from(custom));
    }
    if let Some(home) = dirs::home_dir() {
        return Some(home.join(".knack").join("installed.json"));
    }
    dirs::config_dir().map(|c| c.join("knack").join("installed.json"))
}

/// Read the record. Missing file is normal — first-run users have
/// nothing to load; we return an empty record. Malformed JSON is
/// surfaced because silently dropping it would mask deeper corruption.
pub fn load() -> io::Result<InstalledRecord> {
    let Some(path) = record_path() else {
        return Ok(InstalledRecord::default());
    };
    match fs::read_to_string(&path) {
        Ok(s) => {
            let rec: InstalledRecord = serde_json::from_str(&s).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("{path:?}: {e}"))
            })?;
            Ok(rec)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(InstalledRecord::default()),
        Err(e) => Err(e),
    }
}

/// Persist the record atomically (tempfile + rename in the same dir).
pub fn save(rec: &InstalledRecord) -> io::Result<()> {
    let Some(path) = record_path() else {
        return Err(io::Error::other(
            "no home or config dir available for installed.json",
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

    // Atomic write: write to <path>.<pid>.tmp, then rename. Same parent
    // dir so the rename stays on one filesystem.
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Add or replace an entry for `slug`. Two entries for the same slug at
/// different scopes both coexist (e.g. a user can have `claude/home` and
/// `claude/project` simultaneously if they ran install at both scopes).
pub fn add(slug: &str, scope: Scope, path: PathBuf) -> io::Result<()> {
    let mut rec = load()?;
    rec.agents.retain(|e| !(e.slug == slug && e.scope == scope));
    rec.agents.push(AgentEntry {
        slug: slug.to_string(),
        scope,
        path,
    });
    save(&rec)
}

/// Remove every entry for `slug` regardless of scope. Returns true when
/// at least one entry was removed.
pub fn remove(slug: &str) -> io::Result<bool> {
    let mut rec = load()?;
    let before = rec.agents.len();
    rec.agents.retain(|e| e.slug != slug);
    let changed = rec.agents.len() != before;
    if changed {
        save(&rec)?;
    }
    Ok(changed)
}

/// All currently-known entries. Cheap clone of the in-memory list.
pub fn list() -> io::Result<Vec<AgentEntry>> {
    Ok(load()?.agents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Point ``record_path`` at a fresh tempdir per test so we don't
    /// pollute the user's real ``~/.knack/installed.json``.
    fn isolate() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("installed.json");
        // SAFETY: tests in this crate run single-threaded by default; the
        // env var is the documented override seam.
        unsafe {
            std::env::set_var("KNACK_INSTALLED_FILE", &path);
        }
        (dir, path)
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let (_dir, _) = isolate();
        let rec = load().unwrap();
        assert!(rec.agents.is_empty());
        assert_eq!(rec.version, SCHEMA_VERSION);
    }

    #[test]
    fn add_then_remove_round_trips() {
        let (_dir, path) = isolate();
        add("claude", Scope::Home, PathBuf::from("/x/CLAUDE.md")).unwrap();
        assert!(path.exists());
        let entries = list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].slug, "claude");

        assert!(remove("claude").unwrap());
        assert!(list().unwrap().is_empty());
        assert!(!remove("claude").unwrap()); // idempotent
    }

    #[test]
    fn add_dedupes_same_slug_same_scope() {
        let (_dir, _) = isolate();
        add("claude", Scope::Home, PathBuf::from("/a")).unwrap();
        add("claude", Scope::Home, PathBuf::from("/b")).unwrap();
        let entries = list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("/b"));
    }

    #[test]
    fn add_keeps_same_slug_different_scope() {
        let (_dir, _) = isolate();
        add("claude", Scope::Home, PathBuf::from("/home/x")).unwrap();
        add("claude", Scope::Project, PathBuf::from("/repo/x")).unwrap();
        let entries = list().unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn save_is_atomic_via_tempfile_rename() {
        let (_dir, path) = isolate();
        // Pre-populate an existing record to ensure we don't truncate
        // before the rename lands.
        add("claude", Scope::Home, PathBuf::from("/orig")).unwrap();
        let first = fs::read_to_string(&path).unwrap();

        add("cursor", Scope::Project, PathBuf::from("/repo/.cursor")).unwrap();
        let second = fs::read_to_string(&path).unwrap();

        assert!(second.contains("cursor"));
        assert!(second.contains("/orig"));
        // The interim tempfile must not be left behind.
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tempfile leaked: {leftovers:?}");
        // First read still parses fine (sanity check on JSON shape).
        let _: InstalledRecord = serde_json::from_str(&first).unwrap();
    }
}
