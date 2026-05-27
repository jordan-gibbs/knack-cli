//! `knack upgrade` — install the latest knack CLI in place.
//!
//! Thin wrapper around the platform install one-liner:
//!
//!   * macOS / Linux: `curl -fsSL https://cli.getknack.ai/install.sh | sh`
//!     POSIX `unlink + rename` lets the running binary be replaced
//!     safely while it executes (the current process keeps its inode),
//!     so `--run` defaults to true on these platforms.
//!
//!   * Windows: `iwr https://cli.getknack.ai/install.ps1 | iex`
//!     The running `knack.exe` is file-locked. Self-replacement would
//!     deadlock; this command always prints the one-liner and exits
//!     without running it. The user pipes it into a fresh PowerShell.
//!
//! Exists mostly for discoverability: the passive update banner says
//! "Run `knack upgrade`", and `knack --help` should be able to satisfy
//! that without sending the user back to the website.

use std::process::Command as ProcessCommand;

use clap::Args;
use serde_json::json;

use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

const INSTALL_SH_CMD: &str = "curl -fsSL https://cli.getknack.ai/install.sh | sh";
const INSTALL_PS1_CMD: &str = "iwr https://cli.getknack.ai/install.ps1 | iex";

#[derive(Debug, Args)]
pub struct UpgradeArgs {
    /// Print the platform install one-liner and exit. Default on Windows.
    #[arg(long, conflicts_with = "run")]
    pub print: bool,

    /// Execute the install one-liner via the platform shell. Default on
    /// macOS and Linux. On Windows this still just prints the command
    /// (file lock prevents self-replace) and notes why.
    #[arg(long, conflicts_with = "print")]
    pub run: bool,
}

pub fn run(args: UpgradeArgs, mode: OutputMode) -> CliResult<()> {
    let cmd = platform_install_oneliner();
    let platform = platform_label();
    let should_run = decide_should_run(&args);

    if mode.json {
        emit_ok(
            mode,
            json!({
                "command": cmd,
                "platform": platform,
                "auto_run": should_run,
            }),
            || {},
        );
        if should_run {
            return execute(cmd, mode);
        }
        return Ok(());
    }

    if !mode.quiet {
        if cfg!(target_os = "windows") && args.run {
            println!("Self-replace is not safe on Windows (the running knack.exe is file-locked).");
            println!("Run this in a fresh PowerShell to upgrade:");
        } else if should_run {
            println!("Upgrading via:");
        } else {
            println!("Run this to upgrade knack:");
        }
        println!("  {cmd}");
    }

    if should_run {
        return execute(cmd, mode);
    }
    Ok(())
}

fn decide_should_run(args: &UpgradeArgs) -> bool {
    if args.print {
        return false;
    }
    if cfg!(target_os = "windows") {
        // Refuse to self-replace under file lock. Always print on Win.
        return false;
    }
    // POSIX default: --run unless --print was set.
    args.run || !args.print
}

fn execute(cmd: &str, mode: OutputMode) -> CliResult<()> {
    let status = ProcessCommand::new("sh")
        .arg("-c")
        .arg(cmd)
        .status()
        .map_err(|e| CliError::Internal(format!("spawn upgrade: {e}")))?;
    if !status.success() {
        let err = CliError::Internal(format!(
            "upgrade script exited with status {}",
            status.code().unwrap_or(-1)
        ));
        emit_err(mode, &err);
        return Err(err);
    }
    Ok(())
}

fn platform_install_oneliner() -> &'static str {
    if cfg!(target_os = "windows") {
        INSTALL_PS1_CMD
    } else {
        INSTALL_SH_CMD
    }
}

fn platform_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_oneliner_matches_target() {
        let cmd = platform_install_oneliner();
        if cfg!(target_os = "windows") {
            assert!(cmd.contains("install.ps1"));
        } else {
            assert!(cmd.contains("install.sh"));
        }
    }

    #[test]
    fn windows_never_auto_runs() {
        if !cfg!(target_os = "windows") {
            return;
        }
        let should = decide_should_run(&UpgradeArgs {
            print: false,
            run: true,
        });
        assert!(!should, "Windows must not auto-run upgrade under file lock");
    }

    #[test]
    fn print_flag_short_circuits_run() {
        let should = decide_should_run(&UpgradeArgs {
            print: true,
            run: false,
        });
        assert!(!should);
    }
}
