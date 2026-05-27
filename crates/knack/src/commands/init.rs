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

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::config::{config_file_path, save_backend_mode, BackendMode};
use crate::errors::{CliError, CliResult};
use crate::output::{display_path, emit_err, emit_ok, OutputMode};
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
    /// If omitted in --self-host mode, prompts interactively for the name.
    #[arg(long)]
    pub github_repo: Option<String>,

    /// Visibility of the repo to create in --self-host mode.
    #[arg(long, value_enum, default_value_t = VisibilityFlag::Private)]
    pub visibility: VisibilityFlag,

    /// Where to clone the self-host repo locally. Defaults to `~/<repo-name>`.
    #[arg(long)]
    pub local_path: Option<PathBuf>,

    /// Skip the interactive prompt even if no flag is passed. Implies cloud.
    /// Useful for unattended setups.
    #[arg(long)]
    pub yes: bool,

    /// Skip the actual GitHub repo creation + clone + push (Phase A bootstrap).
    /// Just write the config. Useful for tests and for users who want to
    /// manage the remote repo themselves.
    #[arg(long)]
    pub skip_bootstrap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum VisibilityFlag {
    Public,
    Private,
}

impl From<VisibilityFlag> for knack_backend_github::Visibility {
    fn from(v: VisibilityFlag) -> Self {
        match v {
            VisibilityFlag::Public => knack_backend_github::Visibility::Public,
            VisibilityFlag::Private => knack_backend_github::Visibility::Private,
        }
    }
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

    // `knack init` should make things, not refuse. If --at points at a
    // missing path, create it. Only error if the path exists but is a file.
    if !cwd.exists() {
        if let Err(e) = std::fs::create_dir_all(&cwd) {
            let err = CliError::User {
                code: "INIT_INVALID_TARGET".into(),
                message: format!("could not create {}: {e}", cwd.display()),
                hint: None,
            };
            emit_err(mode, &err);
            return Err(err);
        }
    } else if !cwd.is_dir() {
        let err = CliError::User {
            code: "INIT_INVALID_TARGET".into(),
            message: format!("{} exists but is not a directory", cwd.display()),
            hint: Some("delete the file or pass --at <another-path>".into()),
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
        let b = configure_self_host(&args, mode).map_err(|e| {
            emit_err(mode, &e);
            e
        })?;
        Some(b)
    } else if args.cloud || args.yes {
        Some(BackendMode::Cloud {
            api_base: "https://api.getknack.ai".into(),
        })
    } else if !already_configured && !mode.quiet && !mode.json {
        // Don't try to prompt if stdin isn't a TTY (agent shells, CI, pipes).
        // The previous behavior was to block on read_line forever, which is
        // what bit a real user in May 2026: their agent's PowerShell context
        // had no TTY and `knack init` hung silently.
        if !std::io::stdin().is_terminal() {
            let err = CliError::User {
                code: "NEEDS_FLAGS".into(),
                message: "non-interactive shell; cannot prompt for backend choice".into(),
                hint: Some(
                    "pass --self-host (with --github-repo OWNER/NAME) or --cloud to pick a backend non-interactively".into(),
                ),
            };
            emit_err(mode, &err);
            return Err(err);
        }
        let b = prompt_for_backend(&args, mode).map_err(|e| {
            emit_err(mode, &e);
            e
        })?;
        Some(b)
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
                println!("✓ workspace ready at {}", display_path(&ws));
            } else {
                println!("✓ initialized {} (skills/, drafts/)", display_path(&ws));
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

fn prompt_for_backend(args: &InitArgs, mode: OutputMode) -> CliResult<BackendMode> {
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
        "1" | "github" | "gh" => configure_self_host(args, mode),
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

fn configure_self_host(args: &InitArgs, mode: OutputMode) -> CliResult<BackendMode> {
    let gh_user = resolve_gh_user()?;

    let (owner, repo) = match args.github_repo.as_deref() {
        Some(spec) => parse_owner_repo(spec)?,
        None => {
            // Same TTY guard as the top-level prompt: agents and CI don't
            // have a terminal, so blocking on read_line is a silent hang.
            // Force them to pass --github-repo explicitly.
            if !std::io::stdin().is_terminal() {
                return Err(CliError::User {
                    code: "NEEDS_REPO_FLAG".into(),
                    message: "non-interactive shell; cannot prompt for repo name".into(),
                    hint: Some(
                        "pass --github-repo OWNER/REPO (e.g. --github-repo jordan-gibbs/knack-skills)".into(),
                    ),
                });
            }
            let name = prompt_for_repo_name()?;
            (gh_user.clone(), name)
        }
    };

    let local_path = match &args.local_path {
        Some(p) => p.clone(),
        None => match dirs::home_dir() {
            Some(home) => home.join(&repo),
            None => PathBuf::from(".").join(&repo),
        },
    };

    if !args.skip_bootstrap {
        if !mode.quiet && !mode.json {
            println!();
            println!("→ bootstrapping github.com/{}/{}", owner, repo);
            println!("  local clone: {}", display_path(&local_path));
        }

        let opts = knack_backend_github::BootstrapOpts {
            owner: owner.clone(),
            repo: repo.clone(),
            visibility: args.visibility.into(),
            local_path: local_path.clone(),
            author_name: gh_user.clone(),
            author_email: format!("{}@users.noreply.github.com", gh_user),
        };
        let result = knack_backend_github::bootstrap_repo(&opts).map_err(|e| CliError::User {
            code: "BOOTSTRAP_FAILED".into(),
            // `{:#}` prints anyhow's full source chain so the user sees
            // the underlying libgit2 / gh CLI failure, not just the
            // top-level "git push origin main".
            message: format!("self-host bootstrap failed: {e:#}"),
            hint: Some(
                "verify `gh auth status` shows the `repo` scope, and that the repo name isn't already taken".into(),
            ),
        })?;
        if !mode.quiet && !mode.json {
            if result.created_repo {
                println!("✓ created {}", result.https_url);
            } else {
                println!("✓ using existing {}", result.https_url);
            }
            println!("✓ scaffolded {}", display_path(&result.local_path));
        }
    }

    Ok(BackendMode::Github {
        owner,
        repo,
        local_path,
    })
}

fn prompt_for_repo_name() -> CliResult<String> {
    println!();
    println!("what should we call your skills repo?");
    println!("  (this becomes github.com/<your-handle>/<name>)");
    print!("repo name: ");
    std::io::stdout().flush().ok();

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let name = line.trim().to_string();
    if name.is_empty() {
        return Err(CliError::User {
            code: "EMPTY_REPO_NAME".into(),
            message: "repo name cannot be empty".into(),
            hint: Some("try `knack-skills` or any name you like".into()),
        });
    }
    // GitHub repo names: alphanumerics, hyphens, underscores, dots, max 100.
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(CliError::User {
            code: "INVALID_REPO_NAME".into(),
            message: format!(
                "repo name '{name}' contains invalid characters (use letters, digits, '-', '_', '.')"
            ),
            hint: None,
        });
    }
    Ok(name)
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

/// Like [`resolve_gh_user`] but returns `None` instead of erroring when gh
/// isn't authenticated. Used by `auth status` in github mode to surface
/// "(not signed in via gh)" without failing the whole command.
pub fn resolve_gh_user_opt() -> Option<String> {
    resolve_gh_user().ok()
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
            visibility: VisibilityFlag::Private,
            local_path: None,
            yes: false,
            skip_bootstrap: true,
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
                visibility: VisibilityFlag::Private,
                local_path: None,
                yes: false,
                skip_bootstrap: true,
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
