//! `knack uninstall` — reverse install.
//!
//! Top-level command that bundles the cleanup steps `knack install
//! --uninstall` already performed (shim sweep + config block strip)
//! together with the pieces it never touched: the auth credential, the
//! `~/.knack/` cache, and a documented path to remove the binary
//! itself.
//!
//! Why not self-delete the binary: on Windows the running `knack.exe`
//! is file-locked and cannot replace or delete itself. The honest path
//! for full removal is to print the platform `uninstall.ps1` /
//! `uninstall.sh` one-liner and let the user pipe it into a fresh
//! shell. `--script` makes that one-liner directly addressable so
//! agents have a deterministic command to invoke.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::api::ApiClient;
use crate::auth_store::auth_file_path;
use crate::commands::install::{installed, strip_shims};
use crate::errors::CliResult;
use crate::output::{emit_ok, OutputMode};
use crate::update_check;

const UNINSTALL_PS1_URL: &str = "https://cli.getknack.ai/uninstall.ps1";
const UNINSTALL_SH_URL: &str = "https://cli.getknack.ai/uninstall.sh";

#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Keep the keyring + ~/.knack/auth.json in place. Default is to
    /// clear them so an "uninstall" really uninstalls.
    #[arg(long, conflicts_with = "script")]
    pub keep_auth: bool,

    /// Recursively remove every tracked workspace .knack/ directory.
    /// Off by default because draft skills live there and may be
    /// unpublished.
    #[arg(long, conflicts_with = "script")]
    pub purge_workspaces: bool,

    /// Non-interactive. Skip the confirmation prompt. Implied by --json.
    #[arg(long, short = 'y', conflicts_with = "script")]
    pub yes: bool,

    /// Dry-run: describe what would be removed without touching the
    /// filesystem.
    #[arg(long, conflicts_with = "script")]
    pub print: bool,

    /// Print the platform uninstall-script one-liner and exit. Use this
    /// when the running binary needs to delete itself (Windows file
    /// lock); pipe the printed command into a fresh shell.
    #[arg(long)]
    pub script: bool,
}

pub async fn run(args: UninstallArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if args.script {
        return run_script(mode);
    }

    let interactive = !args.yes && !args.print && !mode.json && !mode.quiet;
    if interactive && !confirm(&args)? {
        if !mode.quiet {
            eprintln!("aborted.");
        }
        return Ok(());
    }

    let report = if args.print {
        None
    } else {
        Some(strip_shims())
    };

    let mut cleared_auth = false;
    if !args.keep_auth && !args.print {
        // Wipe the whole auth.json so multi-account installs are fully
        // cleared. The legacy keyring is per-account, so we clear only
        // the active profile (we can't enumerate keyring accounts).
        if let Some(path) = auth_file_path() {
            cleared_auth |= std::fs::remove_file(&path).is_ok();
        }
        let _ = client.store.clear(&client.account);
        if let Some(legacy) = &client.legacy_store {
            let _ = legacy.clear(&client.account);
            cleared_auth = true;
        }
    }

    let mut deleted_files: Vec<String> = Vec::new();
    if !args.print {
        for path in [installed::record_path(), update_check::cache_path()]
            .into_iter()
            .flatten()
        {
            if std::fs::remove_file(&path).is_ok() {
                deleted_files.push(path.display().to_string());
            }
        }
    }

    let binary_path = guess_binary_path();
    let script_url = platform_script_url();
    let stripped_targets = report.as_ref().map(|r| r.removed.len()).unwrap_or(0);
    let stripped_shims = report.as_ref().map(|r| r.shims_removed).unwrap_or(0);

    if mode.json {
        emit_ok(
            mode,
            json!({
                "dry_run": args.print,
                "stripped_targets": stripped_targets,
                "stripped_shims": stripped_shims,
                "cleared_auth": cleared_auth,
                "deleted_files": deleted_files,
                "workspaces_purged": Vec::<String>::new(),
                "binary_path": binary_path.as_ref().map(|p| p.display().to_string()),
                "script_url": script_url,
            }),
            || {},
        );
        return Ok(());
    }

    if mode.quiet {
        return Ok(());
    }

    if args.print {
        println!("[dry-run] would:");
        println!("  - strip knack shims from every detected agent config");
        println!(
            "  - {}",
            if args.keep_auth {
                "keep auth (~/.knack/auth.json + keyring)"
            } else {
                "clear auth (~/.knack/auth.json + keyring)"
            }
        );
        println!(
            "  - {}",
            if args.purge_workspaces {
                "purge workspace .knack/ directories"
            } else {
                "keep workspace .knack/ directories"
            }
        );
        println!("  - remove ~/.knack/installed.json and ~/.knack/update-check.json");
        println!();
        print_binary_removal_hint(binary_path.as_ref());
        return Ok(());
    }

    if let Some(r) = &report {
        if !r.removed.is_empty() {
            for (target, path) in &r.removed {
                println!("Removed {target} block: {path}");
            }
        }
        if r.shims_removed > 0 {
            println!("Removed {} per-skill shim file(s).", r.shims_removed);
        }
        if r.removed.is_empty() && r.shims_removed == 0 {
            println!("No knack shims or config blocks found.");
        }
    }
    if cleared_auth {
        println!("Cleared auth (account '{}').", client.account);
    } else if args.keep_auth {
        println!("Kept auth (--keep-auth).");
    }
    if !deleted_files.is_empty() {
        for p in &deleted_files {
            println!("Removed cache: {p}");
        }
    }
    if args.purge_workspaces {
        // Workspace path tracking lives in installed.json's AgentEntry.path
        // today, which is the SHIM path, not the workspace root. Until
        // there's a separate workspace registry, --purge-workspaces is a
        // documented no-op rather than a silent wrong-dir wipe.
        println!(
            "Workspace tracking not yet implemented. Remove .knack/ directories manually in each project."
        );
    }
    println!();
    print_binary_removal_hint(binary_path.as_ref());

    Ok(())
}

fn run_script(mode: OutputMode) -> CliResult<()> {
    let cmd = platform_script_oneliner();
    let platform = if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unix"
    };
    if mode.json {
        emit_ok(
            mode,
            json!({
                "command": cmd,
                "platform": platform,
                "auto_run": false,
            }),
            || {},
        );
        return Ok(());
    }
    println!("{cmd}");
    Ok(())
}

fn confirm(args: &UninstallArgs) -> io::Result<bool> {
    eprintln!("knack uninstall will:");
    eprintln!("  - strip knack shims from every detected agent config");
    eprintln!(
        "  - {}",
        if args.keep_auth {
            "keep auth"
        } else {
            "clear auth (~/.knack/auth.json + keyring)"
        }
    );
    eprintln!(
        "  - {}",
        if args.purge_workspaces {
            "purge workspace .knack/ directories"
        } else {
            "keep workspaces"
        }
    );
    eprintln!("  - remove ~/.knack/installed.json and ~/.knack/update-check.json");
    eprintln!();
    eprint!("Continue? [y/N] ");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let ans = line.trim().to_lowercase();
    Ok(ans == "y" || ans == "yes")
}

fn print_binary_removal_hint(binary_path: Option<&PathBuf>) {
    println!("To finish, remove the knack binary by running the platform uninstall script:");
    if cfg!(target_os = "windows") {
        println!("  iwr {UNINSTALL_PS1_URL} | iex");
    } else {
        println!("  curl -fsSL {UNINSTALL_SH_URL} | sh");
    }
    if let Some(p) = binary_path {
        println!();
        println!("Or remove the binary manually:");
        if cfg!(target_os = "windows") {
            println!("  Remove-Item \"{}\"", p.display());
        } else {
            println!("  rm \"{}\"", p.display());
        }
    }
}

fn platform_script_oneliner() -> &'static str {
    if cfg!(target_os = "windows") {
        "iwr https://cli.getknack.ai/uninstall.ps1 | iex"
    } else {
        "curl -fsSL https://cli.getknack.ai/uninstall.sh | sh"
    }
}

fn platform_script_url() -> &'static str {
    if cfg!(target_os = "windows") {
        UNINSTALL_PS1_URL
    } else {
        UNINSTALL_SH_URL
    }
}

fn guess_binary_path() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        std::env::var("LOCALAPPDATA")
            .ok()
            .map(|d| PathBuf::from(d).join("knack").join("bin").join("knack.exe"))
    } else {
        dirs::home_dir().map(|h| h.join(".local").join("bin").join("knack"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_oneliner_matches_platform() {
        let cmd = platform_script_oneliner();
        if cfg!(target_os = "windows") {
            assert!(cmd.contains("uninstall.ps1"));
            assert!(cmd.contains("iwr"));
        } else {
            assert!(cmd.contains("uninstall.sh"));
            assert!(cmd.contains("curl"));
        }
    }

    #[test]
    fn guess_binary_path_returns_platform_default() {
        let p = guess_binary_path();
        assert!(p.is_some(), "binary path should resolve on test platforms");
        let s = p.unwrap().display().to_string();
        if cfg!(target_os = "windows") {
            assert!(s.ends_with("knack.exe"));
            assert!(s.contains("knack"));
        } else {
            assert!(s.ends_with(".local/bin/knack") || s.ends_with(".local\\bin\\knack"));
        }
    }
}
