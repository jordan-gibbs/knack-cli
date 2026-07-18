//! Knack CLI — binary entry. Thin shell around `knack_cli::commands::dispatch`.
//!
//! Logic lives in the library so integration tests can drive it without
//! shelling out.

use std::process::ExitCode;

use clap::Parser;

use knack_cli::commands::{build_client, dispatch, Command, GlobalArgs};
use knack_cli::config::Config;
use knack_cli::update_check;

#[derive(Parser, Debug)]
#[command(
    name = "knack",
    version,
    about = "Teach the AI your job. Once.",
    long_about = "knack — author, version, share, and run AI skills. \
Run `knack docs` for the full reference."
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,

    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> ExitCode {
    // RUST_LOG=knack=debug for verbose tracing; default is silent.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let cli = Cli::parse();
    let mode = cli.global.output_mode();

    // Freeze the TLS trust policy before any HTTP client is built. Flags
    // win over KNACK_CA_BUNDLE / SSL_CERT_FILE / KNACK_INSECURE.
    knack_types::tls::init(cli.global.cacert.clone(), cli.global.insecure);

    let config = Config::load();
    let client = build_client(config, &cli.global);

    // Fire-and-forget refresh of the version cache (≤ once per 24h,
    // 3s-capped, never awaited). The banner below reads whatever the
    // cache holds now; a refresh benefits the NEXT invocation.
    update_check::spawn_refresh(env!("CARGO_PKG_VERSION").to_string());

    let result = dispatch(cli.command, client, mode).await;

    // Last stderr line agents see; suppressed under --json / --quiet /
    // KNACK_NO_UPDATE_CHECK and throttled to once per 24h.
    update_check::print_update_banner_once(mode, env!("CARGO_PKG_VERSION"));

    match result {
        // Most error paths inside `dispatch` call `emit_err` at the point of
        // failure. But that contract is enforced by convention, and any
        // `?`-propagated error that skipped it used to exit nonzero with NO
        // output at all (field bug: silent `publish` / `diff` failures on the
        // self-host backend). The boundary is the backstop: if nothing was
        // emitted, emit here so every failure is visible. POSIX caps exit at
        // u8 (255), and our table never goes that high anyway.
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if !knack_cli::output::error_was_emitted() {
                knack_cli::output::emit_err(mode, &e);
            }
            let code = e.exit_code().0;
            ExitCode::from(code.clamp(1, 255) as u8)
        }
    }
}
