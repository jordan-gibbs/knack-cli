//! Runtime config — base URL, on-disk paths, backend selection.
//!
//! Defaults to production cloud mode but every value is overridable via env or
//! the on-disk profile at `~/.knack/config.yaml` so CI, contributors, and
//! self-host users can switch backends without recompiling.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEFAULT_API_BASE: &str = "https://api.getknack.ai";
const ENV_API_BASE: &str = "KNACK_API_URL";

/// Which backend the CLI talks to for skill ops.
///
/// `Cloud` hits `api_base`. `Github` reads and writes a local checkout of a
/// user-owned GitHub repo. Set once at `knack init` and stored in the on-disk
/// profile; runtime is read-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum BackendMode {
    Cloud {
        #[serde(default = "default_api_base")]
        api_base: String,
    },
    Github {
        owner: String,
        repo: String,
        local_path: PathBuf,
    },
}

fn default_api_base() -> String {
    DEFAULT_API_BASE.into()
}

impl Default for BackendMode {
    fn default() -> Self {
        Self::Cloud {
            api_base: DEFAULT_API_BASE.into(),
        }
    }
}

/// Default on-disk scope for `knack link` when neither `--global` nor
/// `--local` is passed. Overridable in `~/.knack/config.yaml` under
/// `defaults.link_scope`. Kept here (not in the install module) so config
/// has no dependency on command internals; `link.rs` maps it onto the
/// install `Scope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkScope {
    /// `~/.<agent>/skills/<slug>/` — available in every project.
    #[default]
    Home,
    /// `<cwd>/.<agent>/skills/<slug>/` — this project only.
    Project,
}

impl LinkScope {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkScope::Home => "home",
            LinkScope::Project => "project",
        }
    }

    /// Parse a config value. Accepts the documented `home`/`project` plus
    /// the flag-aligned synonyms `global`/`local` so a value copied from
    /// the flags still works.
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "home" | "global" => Some(LinkScope::Home),
            "project" | "local" => Some(LinkScope::Project),
            _ => None,
        }
    }
}

/// CLI runtime config. Cheap to clone; lives for the duration of one invocation.
#[derive(Debug, Clone)]
pub struct Config {
    pub api_base: String,
    pub skills_dir: PathBuf,
    pub keyring_service: String,
    pub backend: BackendMode,
    /// Default scope for `knack link` (flags still override per-invocation).
    pub link_scope: LinkScope,
}

impl Config {
    pub fn load() -> Self {
        let api_base = std::env::var(ENV_API_BASE).unwrap_or_else(|_| DEFAULT_API_BASE.into());
        let api_base = api_base.trim_end_matches('/').to_string();
        let profile = load_profile();
        let backend = profile
            .as_ref()
            .map(|p| p.backend.clone())
            .unwrap_or(BackendMode::Cloud {
                api_base: api_base.clone(),
            });
        let link_scope = profile
            .as_ref()
            .and_then(|p| p.defaults.as_ref())
            .and_then(|d| d.link_scope.as_deref())
            .and_then(LinkScope::parse)
            .unwrap_or_default();
        Self {
            api_base,
            skills_dir: skills_dir(),
            keyring_service: "knack".into(),
            backend,
            link_scope,
        }
    }
}

/// Read `~/.knack/config.yaml` if it exists. Absent / malformed file is
/// normal: the CLI falls back to cloud mode against the default API base.
fn load_profile() -> Option<ProfileFile> {
    let path = config_file_path()?;
    let bytes = std::fs::read(&path).ok()?;
    // BOM-tolerant: a hand-edited config.yaml saved by PowerShell 5.1 would
    // otherwise silently fail to parse and drop the user back to cloud mode.
    serde_yaml::from_slice(knack_types::strip_utf8_bom(&bytes)).ok()
}

/// `~/.knack/config.yaml`. Lives next to the auth file so a user's whole
/// profile is one directory.
pub fn config_file_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("KNACK_CONFIG_FILE") {
        return Some(PathBuf::from(custom));
    }
    Some(dirs::home_dir()?.join(".knack").join("config.yaml"))
}

/// User-tunable defaults block in `~/.knack/config.yaml`. Optional and
/// forward-compatible: unknown keys are ignored and an absent block keeps
/// every built-in default.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Defaults {
    /// `"home"` (default) or `"project"` — the no-flag scope for
    /// `knack link`. See [`LinkScope`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    link_scope: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProfileFile {
    backend: BackendMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    defaults: Option<Defaults>,
}

/// Write the current backend mode to `~/.knack/config.yaml`. Used by
/// `knack init` after the user picks self-host or cloud. Preserves any
/// existing `defaults` block so re-running init never silently drops a
/// user's `link_scope`.
pub fn save_backend_mode(backend: &BackendMode) -> std::io::Result<()> {
    let path = config_file_path().ok_or_else(|| std::io::Error::other("no home directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let defaults = load_profile().and_then(|p| p.defaults);
    let profile = ProfileFile {
        backend: backend.clone(),
        defaults,
    };
    let yaml = serde_yaml::to_string(&profile)
        .map_err(|e| std::io::Error::other(format!("serialize yaml: {e}")))?;
    std::fs::write(&path, yaml)
}

/// `~/.knack/skills/` — XDG-compliant on Linux: `$XDG_DATA_HOME/knack/skills`.
fn skills_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("KNACK_SKILLS_DIR") {
        return PathBuf::from(custom);
    }
    if cfg!(target_os = "linux") {
        if let Some(data) = dirs::data_dir() {
            return data.join("knack").join("skills");
        }
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".knack").join("skills");
    }
    // Last-resort: cwd-relative. Should never hit this on real systems.
    PathBuf::from(".knack").join("skills")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_strips_trailing_slash() {
        // SAFETY: tests run single-threaded in this crate by default, but we
        // restore the env var to avoid surprising other tests.
        // SAFETY justification: Rust 1.78+ marks set_var unsafe.
        unsafe {
            std::env::set_var(ENV_API_BASE, "https://example.test/");
        }
        let cfg = Config::load();
        assert_eq!(cfg.api_base, "https://example.test");
        unsafe {
            std::env::remove_var(ENV_API_BASE);
        }
    }

    #[test]
    fn skills_dir_falls_back_when_no_home() {
        // Just smoke-test that we get a non-empty path; real platform logic
        // is exercised in integration where dirs has the right paths.
        let p = skills_dir();
        assert!(!p.as_os_str().is_empty());
    }

    #[test]
    fn link_scope_default_is_home() {
        assert_eq!(LinkScope::default(), LinkScope::Home);
        assert_eq!(LinkScope::Home.as_str(), "home");
        assert_eq!(LinkScope::Project.as_str(), "project");
    }

    #[test]
    fn link_scope_parses_documented_and_synonym_values() {
        assert_eq!(LinkScope::parse("home"), Some(LinkScope::Home));
        assert_eq!(LinkScope::parse("global"), Some(LinkScope::Home));
        assert_eq!(LinkScope::parse("project"), Some(LinkScope::Project));
        assert_eq!(LinkScope::parse("local"), Some(LinkScope::Project));
        assert_eq!(LinkScope::parse(" Project "), Some(LinkScope::Project));
        assert_eq!(LinkScope::parse("nonsense"), None);
    }
}
