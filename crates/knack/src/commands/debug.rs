//! `knack debug` — dump environment + config + recent state.
//!
//! For bug reports. Always emits JSON (the human format would mostly be the
//! same JSON pretty-printed). Tokens are never included in the output —
//! presence is reported as a boolean only.

use clap::Args;
use serde_json::json;

use crate::api::ApiClient;
use crate::errors::CliResult;
use crate::output::{emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct DebugArgs {}

pub fn run(_args: DebugArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    // We deliberately do not call /auth/me — the user might be debugging an
    // unreachable API; this command must work offline.
    let token_present = client.bearer_override.is_some()
        || client.store.load(&client.account).ok().flatten().is_some();

    let payload = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "config": {
            "api_base": client.config.api_base,
            "skills_dir": client.config.skills_dir,
            "keyring_service": client.config.keyring_service,
            "account": client.account,
        },
        "auth": {
            "token_present": token_present,
            "via_override": client.bearer_override.is_some(),
        },
        "env_relevant": {
            "KNACK_API_URL": std::env::var("KNACK_API_URL").ok(),
            "KNACK_SKILLS_DIR": std::env::var("KNACK_SKILLS_DIR").ok(),
            "KNACK_AUTH_TOKEN": std::env::var("KNACK_AUTH_TOKEN").ok().map(|_| "<set>".to_string()),
            "EDITOR": std::env::var("EDITOR").ok(),
            "VISUAL": std::env::var("VISUAL").ok(),
        },
    });

    emit_ok(mode, payload, || {
        println!("knack v{}", env!("CARGO_PKG_VERSION"));
        println!(
            "  os/arch     {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        println!("  api         {}", client.config.api_base);
        println!("  skills_dir  {}", client.config.skills_dir.display());
        println!("  account     {}", client.account);
        println!(
            "  token       {}",
            if token_present {
                "present"
            } else {
                "(not signed in)"
            }
        );
    });
    Ok(())
}
