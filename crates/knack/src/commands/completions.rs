//! `knack completions <shell>` — emit a shell completion script to stdout.
//!
//! Users redirect into the right place per shell:
//!
//!   bash       → `knack completions bash > /etc/bash_completion.d/knack`
//!   zsh        → `knack completions zsh  > /usr/local/share/zsh/site-functions/_knack`
//!   fish       → `knack completions fish > ~/.config/fish/completions/knack.fish`
//!   powershell → `knack completions powershell | Out-File ...`
//!
//! We rebuild the same Command tree the introspect path uses; clap_complete
//! generates the script from that.

use std::io;

use clap::Args;
use clap_complete::Shell;

use crate::errors::CliResult;
use crate::output::OutputMode;

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell flavor.
    #[arg(value_enum)]
    pub shell: Shell,
}

pub fn run(args: CompletionsArgs, _mode: OutputMode) -> CliResult<()> {
    let mut cmd = build_root_command();
    clap_complete::generate(args.shell, &mut cmd, "knack", &mut io::stdout());
    Ok(())
}

fn build_root_command() -> clap::Command {
    use clap::{Args as _, Subcommand as _};
    let base = clap::Command::new("knack")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Teach the AI your job. Once.");
    let base = crate::commands::GlobalArgs::augment_args(base);
    crate::commands::Command::augment_subcommands(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_command_renders_for_every_shell() {
        // Each enum variant should be reachable; this catches a clap_complete
        // version skew at compile + smoke time.
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Elvish,
        ] {
            let mut cmd = build_root_command();
            let mut out: Vec<u8> = Vec::new();
            clap_complete::generate(shell, &mut cmd, "knack", &mut out);
            assert!(!out.is_empty(), "no output for {:?}", shell);
        }
    }
}
