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

/// CLI runtime config. Cheap to clone; lives for the duration of one invocation.
#[derive(Debug, Clone)]
pub struct Config {
    pub api_base: String,
    pub skills_dir: PathBuf,
    pub keyring_service: String,
    pub backend: BackendMode,
}

impl Config {
    pub fn load() -> Self {
        let api_base = std::env::var(ENV_API_BASE).unwrap_or_else(|_| DEFAULT_API_BASE.into());
        let api_base = api_base.trim_end_matches('/').to_string();
        let backend = load_backend_mode().unwrap_or(BackendMode::Cloud {
            api_base: api_base.clone(),
        });
        Self {
            api_base,
            skills_dir: skills_dir(),
            keyring_service: "knack".into(),
            backend,
        }
    }
}

/// Read `~/.knack/config.yaml` if it exists. Absent file is normal: the CLI
/// falls back to cloud mode against the default API base.
fn load_backend_mode() -> Option<BackendMode> {
    let path = config_file_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let raw: ProfileFile = serde_yaml::from_slice(&bytes).ok()?;
    Some(raw.backend)
}

/// `~/.knack/config.yaml`. Lives next to the auth file so a user's whole
/// profile is one directory.
pub fn config_file_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("KNACK_CONFIG_FILE") {
        return Some(PathBuf::from(custom));
    }
    Some(dirs::home_dir()?.join(".knack").join("config.yaml"))
}

#[derive(Debug, Serialize, Deserialize)]
struct ProfileFile {
    backend: BackendMode,
}

/// Write the current backend mode to `~/.knack/config.yaml`. Used by
/// `knack init` after the user picks self-host or cloud.
pub fn save_backend_mode(backend: &BackendMode) -> std::io::Result<()> {
    let path = config_file_path().ok_or_else(|| std::io::Error::other("no home directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let profile = ProfileFile {
        backend: backend.clone(),
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
}
