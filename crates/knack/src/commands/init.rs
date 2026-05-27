//! `knack init` — first-run bifurcation + workspace scaffold.
//!
//! On first invocation we ask the user where their skills should live:
//!
//!   1. Self-host (GitHub-backed) — stores skills in a user-owned repo
//!   2. Knack Cloud — stores skills in api.getknack.ai
//!
//! The choice is persisted to `~/.knack/config.yaml` so subsequent commands
//! talk to the right backend without asking again. The workspace scaffold
//! (`.knack/skills`, `.knack/drafts`, etc.) is created in either mode so
//! `knack create` works locally before the first push.
//!
//! Non-interactive flags `--self-host` and `--cloud` skip the prompt; CI and
//! agents that already know the user's intent pass one of these.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::config::{config_file_path, save_backend_mode, BackendMode};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};
use crate::workspace::{init_workspace, is_workspace};

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Directory to initialize. Defaults to the current working dir.
    /// The `.knack/` subdirectory is created here.
    #[arg(long)]
    pub at: Option<PathBuf>,

    /// Configure GitHub-backed self-host mode (skips the interactive prompt).
    /// The repo defaults to <user>/knack; override with --github-repo.
    #[arg(long, conflicts_with = "cloud")]
    pub self_host: bool,

    /// Configure Knack Cloud mode (skips the interactive prompt).
    #[arg(long, conflicts_with = "self_host")]
    pub cloud: bool,

    /// GitHub repo to use for self-host mode, in `<owner>/<repo>` form.
    /// If omitted in --self-host mode, defaults to `<your-gh-handle>/knack`.
    #[arg(long)]
    pub github_repo: Option<String>,

    /// Skip the interactive prompt even if no flag is passed. Implies cloud.
    /// Useful for unattended setups.
    #[arg(long)]
    pub yes: bool,
}

pub fn run(args: InitArgs, mode: OutputMode) -> CliResult<()> {
    let cwd = match args.at {
        Some(ref p) => p.clone(),
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

    // Bifurcation: only on first run or when an explicit flag forces it.
    let already_configured = config_file_path().map(|p| p.exists()).unwrap_or(false);

    let backend = if args.self_host {
        Some(configure_self_host(args.github_repo.as_deref(), mode)?)
    } else if args.cloud || args.yes {
        Some(BackendMode::Cloud {
            api_base: "https://api.getknack.ai".into(),
        })
    } else if !already_configured && !mode.quiet && !mode.json {
        Some(prompt_for_backend(mode)?)
    } else {
        None
    };

    if let Some(b) = &backend {
        if let Err(e) = save_backend_mode(b) {
            let err = CliError::Internal(format!("write config.yaml: {e}"));
            emit_err(mode, &err);
            return Err(err);
        }
    }

    let already_existed = is_workspace(&ws)
        && ws
            .join("README.md")
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0)
            > 0
        && ws
            .join(".gitignore")
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0)
            > 0;

    emit_ok(
        mode,
        json!({
            "workspace": ws,
            "skills_dir": ws.join("skills"),
            "drafts_dir": ws.join("drafts"),
            "already_existed": already_existed,
            "backend": backend.as_ref().map(backend_label),
        }),
        || {
            if already_existed {
                println!("✓ workspace ready at {}", ws.display());
            } else {
                println!("✓ initialized {} (skills/, drafts/)", ws.display());
            }
            if let Some(b) = &backend {
                println!("✓ backend: {}", backend_label(b));
                if matches!(b, BackendMode::Cloud { .. }) {
                    println!();
                    println!("next: run `knack auth login` to sign into Knack Cloud.");
                }
            }
        },
    );
    Ok(())
}

fn backend_label(b: &BackendMode) -> String {
    match b {
        BackendMode::Cloud { api_base } => format!("cloud ({api_base})"),
        BackendMode::Github { owner, repo, .. } => format!("github ({owner}/{repo})"),
    }
}

fn prompt_for_backend(mode: OutputMode) -> CliResult<BackendMode> {
    println!();
    println!("welcome to knack.");
    println!();
    println!("where do you want your skills to live?");
    println!();
    println!("  1. github (self-host, free, lives in your own repo)");
    println!("  2. knack cloud (zero setup, free tier, public marketplace)");
    println!();
    print!("pick one [1/2]: ");
    std::io::stdout().flush().ok();

    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let choice = line.trim();

    match choice {
        "1" | "github" | "gh" => configure_self_host(None, mode),
        "2" | "cloud" | "" => Ok(BackendMode::Cloud {
            api_base: "https://api.getknack.ai".into(),
        }),
        other => Err(CliError::User {
            code: "INVALID_CHOICE".into(),
            message: format!("expected 1 or 2, got '{other}'"),
            hint: Some("re-run `knack init` and pick 1 (github) or 2 (cloud)".into()),
        }),
    }
}

fn configure_self_host(repo_override: Option<&str>, _mode: OutputMode) -> CliResult<BackendMode> {
    let gh_user = resolve_gh_user()?;
    let (owner, repo) = match repo_override {
        Some(spec) => parse_owner_repo(spec)?,
        None => (gh_user.clone(), "knack".to_string()),
    };

    let local_path = match dirs::home_dir() {
        Some(home) => home.join("knack"),
        None => PathBuf::from(".").join("knack"),
    };

    Ok(BackendMode::Github {
        owner,
        repo,
        local_path,
    })
}

fn parse_owner_repo(spec: &str) -> CliResult<(String, String)> {
    let (owner, repo) = spec.split_once('/').ok_or_else(|| CliError::User {
        code: "INVALID_REPO".into(),
        message: format!("--github-repo must be '<owner>/<repo>', got '{spec}'"),
        hint: None,
    })?;
    if owner.is_empty() || repo.is_empty() {
        return Err(CliError::User {
            code: "INVALID_REPO".into(),
            message: format!("--github-repo must be '<owner>/<repo>', got '{spec}'"),
            hint: None,
        });
    }
    Ok((owner.to_string(), repo.to_string()))
}

/// Resolve the GitHub username via `gh api user --jq .login`. Falls back to
/// `$GITHUB_USER` if `gh` isn't installed or isn't authenticated.
fn resolve_gh_user() -> CliResult<String> {
    if let Ok(user) = std::env::var("GITHUB_USER") {
        if !user.is_empty() {
            return Ok(user);
        }
    }
    let output = std::process::Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let user = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if user.is_empty() {
                Err(gh_missing_error())
            } else {
                Ok(user)
            }
        }
        _ => Err(gh_missing_error()),
    }
}

fn gh_missing_error() -> CliError {
    CliError::User {
        code: "GH_NOT_AUTHENTICATED".into(),
        message: "could not resolve your GitHub user. install gh and run `gh auth login`, or set $GITHUB_USER".into(),
        hint: Some("https://cli.github.com".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_creates_workspace_at_explicit_path() {
        let root = tempdir().unwrap();
        let args = InitArgs {
            at: Some(root.path().to_path_buf()),
            self_host: false,
            cloud: true, // skip prompt
            github_repo: None,
            yes: false,
        };
        let mode = OutputMode {
            json: false,
            quiet: true,
            no_color: true,
        };
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
        let mode = OutputMode {
            json: false,
            quiet: true,
            no_color: true,
        };
        for _ in 0..2 {
            let args = InitArgs {
                at: Some(root.path().to_path_buf()),
                self_host: false,
                cloud: true,
                github_repo: None,
                yes: false,
            };
            run(args, mode).unwrap();
        }
        assert!(root.path().join(".knack").join("skills").is_dir());
    }

    #[test]
    fn parses_owner_repo() {
        assert_eq!(
            parse_owner_repo("jordan-gibbs/knack").unwrap(),
            ("jordan-gibbs".into(), "knack".into())
        );
        assert!(parse_owner_repo("invalid").is_err());
        assert!(parse_owner_repo("/repo").is_err());
        assert!(parse_owner_repo("owner/").is_err());
    }
}
