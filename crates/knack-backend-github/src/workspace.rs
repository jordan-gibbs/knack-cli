//! Self-host workspace config reader (`<repo>/knack.yaml`) + push-policy
//! resolver.
//!
//! `bootstrap_repo` seeds knack.yaml with `owner`, `repo`, and `format`.
//! Users can add `auto_push: false` to opt every telemetry event in this
//! workspace out of the synchronous `git push origin <default-branch>`
//! that follows the commit. The CLI's `--no-push` flag and the
//! `KNACK_AUTO_PUSH=0` env var are the per-invocation and per-shell
//! equivalents.
//!
//! Single resolver: [`PushPolicy::resolve`]. Three layers, one
//! precedence order, one source of truth. Before v0.7.10 the env layer
//! was checked separately inside `commit_and_push_event` while the CLI
//! and workspace layers were checked in the CLI command code; the two
//! sites drifted and one of them silently swallowed malformed-YAML
//! errors. This module owns the policy now.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct WorkspaceConfig {
    #[serde(default)]
    auto_push: Option<bool>,
}

/// Resolved push intent. The wrapper exists so a future caller can match
/// on "was this disabled by the user (silent OK) vs by a config error
/// (caller should surface)?" without re-implementing the precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushPolicy {
    /// Default state: the CLI should push after committing.
    Push,
    /// Explicit opt-out at any of the three layers.
    Skip,
}

impl PushPolicy {
    /// True iff the resulting policy says "go push."
    pub fn should_push(self) -> bool {
        matches!(self, Self::Push)
    }

    /// Resolve the policy from all three layers, in precedence order:
    ///
    /// 1. CLI flag `--no-push` (most-specific, per-invocation)
    /// 2. Env var `KNACK_AUTO_PUSH=0|false|no` (per-shell)
    /// 3. Workspace `<repo>/knack.yaml` `auto_push: false` (per-workspace)
    /// 4. Default → Push
    ///
    /// Returns the policy plus a `Result` because a malformed knack.yaml
    /// is a real error the caller should surface (the user changed a
    /// config file and got something silently different than what they
    /// asked for). A missing knack.yaml is `Ok(Push)` because that's
    /// the default-state, no-config case.
    pub fn resolve(cli_no_push: bool, repo: &Path) -> Result<Self> {
        // Layer 1: --no-push wins outright.
        if cli_no_push {
            return Ok(Self::Skip);
        }
        // Layer 2: env-level kill switch. Any falsy spelling counts so
        // a user can pick whichever feels most natural in their shell.
        if let Ok(v) = std::env::var("KNACK_AUTO_PUSH") {
            if matches!(v.as_str(), "0" | "false" | "no") {
                return Ok(Self::Skip);
            }
        }
        // Layer 3: workspace config. Propagate parse errors so the user
        // sees "your knack.yaml is malformed" instead of silently
        // defaulting back to push-on. read_workspace_auto_push already
        // returns Ok(None) for a missing file, which is the no-config
        // case we want to default to Push.
        if let Some(false) = read_workspace_auto_push(repo)? {
            return Ok(Self::Skip);
        }
        Ok(Self::Push)
    }
}

/// Return the workspace's `auto_push` setting from `<repo>/knack.yaml`.
///
/// `Ok(Some(true))` / `Ok(Some(false))` if explicitly set. `Ok(None)` if the
/// file or field is missing — caller picks the default. Errors only on a
/// genuinely malformed YAML file (so a stray typo doesn't silently flip the
/// telemetry-push behavior).
pub fn read_workspace_auto_push(repo: &Path) -> Result<Option<bool>> {
    let path = repo.join("knack.yaml");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let cfg: WorkspaceConfig = serde_yaml::from_str(knack_types::strip_utf8_bom_str(&raw))
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(cfg.auto_push)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn missing_file_returns_none() {
        let dir = tempdir().unwrap();
        assert_eq!(read_workspace_auto_push(dir.path()).unwrap(), None);
    }

    #[test]
    fn yaml_without_auto_push_returns_none() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("knack.yaml"), "owner: someone\nrepo: x\n").unwrap();
        assert_eq!(read_workspace_auto_push(dir.path()).unwrap(), None);
    }

    #[test]
    fn yaml_with_auto_push_false_returns_false() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("knack.yaml"),
            "owner: someone\nrepo: x\nauto_push: false\n",
        )
        .unwrap();
        assert_eq!(read_workspace_auto_push(dir.path()).unwrap(), Some(false));
    }

    #[test]
    fn yaml_with_auto_push_true_returns_true() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("knack.yaml"),
            "owner: someone\nrepo: x\nauto_push: true\n",
        )
        .unwrap();
        assert_eq!(read_workspace_auto_push(dir.path()).unwrap(), Some(true));
    }

    // ── PushPolicy::resolve precedence tests ─────────────────────────
    //
    // These tests pin the documented precedence (CLI → env → workspace
    // → default) so a future refactor that inverts any pair gets caught
    // before it ships. `std::env::set_var` is process-global, so tests
    // that touch `KNACK_AUTO_PUSH` serialize through a module-level
    // Mutex — without it, cargo's parallel test runner makes the env
    // race-prone.
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write_workspace_auto_push(dir: &Path, value: bool) {
        fs::write(
            dir.join("knack.yaml"),
            format!("owner: t\nrepo: x\nauto_push: {value}\n"),
        )
        .unwrap();
    }

    #[test]
    fn policy_default_is_push() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        std::env::remove_var("KNACK_AUTO_PUSH");
        assert!(PushPolicy::resolve(false, dir.path()).unwrap().should_push());
    }

    #[test]
    fn policy_cli_no_push_beats_workspace_true() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        write_workspace_auto_push(dir.path(), true);
        std::env::remove_var("KNACK_AUTO_PUSH");
        assert!(!PushPolicy::resolve(true, dir.path()).unwrap().should_push());
    }

    #[test]
    fn policy_workspace_false_disables_push() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        write_workspace_auto_push(dir.path(), false);
        std::env::remove_var("KNACK_AUTO_PUSH");
        assert!(!PushPolicy::resolve(false, dir.path()).unwrap().should_push());
    }

    #[test]
    fn policy_workspace_true_keeps_push() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        write_workspace_auto_push(dir.path(), true);
        std::env::remove_var("KNACK_AUTO_PUSH");
        assert!(PushPolicy::resolve(false, dir.path()).unwrap().should_push());
    }

    #[test]
    fn policy_env_kill_beats_workspace_true() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        write_workspace_auto_push(dir.path(), true);
        std::env::set_var("KNACK_AUTO_PUSH", "0");
        let resolved = PushPolicy::resolve(false, dir.path()).unwrap();
        std::env::remove_var("KNACK_AUTO_PUSH");
        assert!(!resolved.should_push());
    }

    #[test]
    fn policy_env_accepts_false_and_no() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        for v in ["false", "no", "0"] {
            std::env::set_var("KNACK_AUTO_PUSH", v);
            let resolved = PushPolicy::resolve(false, dir.path()).unwrap();
            std::env::remove_var("KNACK_AUTO_PUSH");
            assert!(!resolved.should_push(), "env={v}");
        }
    }

    #[test]
    fn policy_cli_beats_env() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        write_workspace_auto_push(dir.path(), true);
        std::env::set_var("KNACK_AUTO_PUSH", "1");
        // CLI says no-push; env says push-on; CLI wins.
        let resolved = PushPolicy::resolve(true, dir.path()).unwrap();
        std::env::remove_var("KNACK_AUTO_PUSH");
        assert!(!resolved.should_push());
    }

    #[test]
    fn policy_malformed_yaml_propagates_error() {
        // Per item #13 — a malformed knack.yaml should surface, not
        // silently default to push-on. The CLI command layer turns this
        // into a user-visible error rather than swallowing it.
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("knack.yaml"), "owner: [unclosed-list").unwrap();
        std::env::remove_var("KNACK_AUTO_PUSH");
        assert!(PushPolicy::resolve(false, dir.path()).is_err());
    }
}
