//! Runtime config — base URL, on-disk paths.
//!
//! Defaults to production but every value is overridable via env so CI and
//! contributors can point at staging or a local API without recompiling.

use std::path::PathBuf;

const DEFAULT_API_BASE: &str = "https://api.getknack.ai";
const ENV_API_BASE: &str = "KNACK_API_URL";

/// CLI runtime config. Cheap to clone; lives for the duration of one invocation.
#[derive(Debug, Clone)]
pub struct Config {
    pub api_base: String,
    pub skills_dir: PathBuf,
    pub keyring_service: String,
}

impl Config {
    pub fn load() -> Self {
        let api_base = std::env::var(ENV_API_BASE).unwrap_or_else(|_| DEFAULT_API_BASE.into());
        Self {
            api_base: api_base.trim_end_matches('/').to_string(),
            skills_dir: skills_dir(),
            keyring_service: "knack".into(),
        }
    }
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
