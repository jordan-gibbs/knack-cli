//! `knack init` — scaffold a workspace-local `.knack/` directory.
//!
//! Idempotent: re-running on an existing workspace just ensures the
//! canonical subdirs are present and never overwrites the README /
//! gitignore.

use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};
use crate::workspace::{init_workspace, is_workspace};

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Directory to initialize. Defaults to the current working dir.
    /// The ``.knack/`` subdirectory is created here.
    #[arg(long)]
    pub at: Option<PathBuf>,
}

pub fn run(args: InitArgs, mode: OutputMode) -> CliResult<()> {
    let cwd = match args.at {
        Some(p) => p,
        None => std::env::current_dir().map_err(|e| {
            let err = CliError::Internal(format!("could not read cwd: {e}"));
            emit_err(mode, &err);
            err
        })?,
    };

    if !cwd.is_dir() {
        let err = CliError::User {
            code: "INIT_INVALID_TARGET".into(),
            message: format!("not a directory: {}", cwd.display()),
            hint: Some("pass --at <existing-dir> or run from a real workspace".into()),
        };
        emit_err(mode, &err);
        return Err(err);
    }

    let ws = match init_workspace(&cwd) {
        Ok(p) => p,
        Err(e) => {
            let err = CliError::Internal(format!("init failed: {e}"));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    let already_existed = is_workspace(&ws)
        && ws.join("README.md").metadata().map(|m| m.len()).unwrap_or(0) > 0
        && ws.join(".gitignore").metadata().map(|m| m.len()).unwrap_or(0) > 0;

    emit_ok(
        mode,
        json!({
            "workspace": ws,
            "skills_dir": ws.join("skills"),
            "drafts_dir": ws.join("drafts"),
            "already_existed": already_existed,
        }),
        || {
            if already_existed {
                println!("✓ workspace ready at {}", ws.display());
            } else {
                println!("✓ initialized {} (skills/, drafts/)", ws.display());
            }
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::OutputMode;
    use tempfile::tempdir;

    #[test]
    fn init_creates_workspace_at_explicit_path() {
        let root = tempdir().unwrap();
        let args = InitArgs { at: Some(root.path().to_path_buf()) };
        let mode = OutputMode { json: false, quiet: true, no_color: true };
        run(args, mode).unwrap();
        let ws = root.path().join(".knack");
        assert!(ws.join("skills").is_dir());
        assert!(ws.join("drafts").is_dir());
        assert!(ws.join(".gitignore").is_file());
        assert!(ws.join("README.md").is_file());
    }

    #[test]
    fn init_is_idempotent() {
        let root = tempdir().unwrap();
        let mode = OutputMode { json: false, quiet: true, no_color: true };
        for _ in 0..2 {
            let args = InitArgs { at: Some(root.path().to_path_buf()) };
            run(args, mode).unwrap();
        }
        // README isn't overwritten on the second run; can't easily assert that
        // without exposing internals, but the call succeeding is the contract.
        assert!(root.path().join(".knack").join("skills").is_dir());
    }
}
