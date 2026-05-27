//! `knack info` — print the canonical Knack agent playbook (agent.txt).
//!
//! Fetches `https://getknack.ai/agent.txt` so the output is always current.
//! On any network failure, falls back to a copy bundled into the binary at
//! build time via `include_str!`. The bundled copy lives next to the web
//! origin (`apps/web/public/agent.txt`) so the source of truth is a single
//! file — release builds embed whatever the web is serving at tag time.
//!
//! Output is plain stdout, no decoration: the consumer is an LLM, not a
//! human, and ANSI/structured formatting just adds noise to the prompt.

use clap::Args;

use crate::errors::CliResult;
use crate::output::OutputMode;

/// Embedded fallback agent.txt — synced from the marketing site at release time.
const BUNDLED: &str = include_str!("../../embedded/agent.txt");

const REMOTE_URL: &str = "https://getknack.ai/agent.txt";

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Skip the network fetch and print the bundled copy directly.
    #[arg(long)]
    pub offline: bool,
}

pub async fn run(args: InfoArgs, mode: OutputMode) -> CliResult<()> {
    let (body, source) = if args.offline {
        (BUNDLED.to_string(), "bundled")
    } else {
        match fetch_remote().await {
            Ok(b) => (b, "remote"),
            Err(_) => (BUNDLED.to_string(), "bundled"),
        }
    };

    if mode.json {
        let payload = serde_json::json!({
            "source": source,
            "url": REMOTE_URL,
            "body": body,
        });
        // Avoid pulling output::emit_ok's human-callback for a simple stdout dump.
        println!("{payload}");
    } else {
        if source == "bundled" && !args.offline && !mode.quiet {
            // One-line warning to stderr so it doesn't taint stdout (agents
            // parse stdout as the playbook).
            eprintln!("knack info: using bundled copy (network fetch failed); try again online for the latest.");
        }
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

async fn fetch_remote() -> Result<String, reqwest::Error> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .connect_timeout(std::time::Duration::from_secs(1))
        .build()?;
    let resp = client.get(REMOTE_URL).send().await?.error_for_status()?;
    resp.text().await
}
