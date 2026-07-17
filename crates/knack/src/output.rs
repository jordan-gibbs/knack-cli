//! Output discipline: stdout = data, stderr = chatter.
//!
//! Track E §E6 mandates that stdout never gets human-friendly text mixed in
//! with the `--json` payload. This module is the only thing that writes to
//! stdout in the binary; commands emit their result through it and let the
//! mode (json vs human) decide formatting.

use console::{style, Term};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;

/// Render a path with forward slashes for user-visible output.
///
/// On Windows, joining a bash-style root (`/c/Users/...`) with native
/// `PathBuf::join` produces mixed separators (`C:/Users/Jordan\skills\foo`).
/// That looks broken even though it works. Use this helper anywhere a path
/// is going into a human-readable string (println!, error messages). Generally
/// don't use it for paths going into JSON envelopes — those should be the
/// native shape so downstream tooling can pass them to the OS without
/// rewriting. Exception: JSON fields that are themselves hints for human
/// consumption (e.g. a "log_file" the user might `cat`) read better with the
/// normalized form.
///
/// Also strips Windows' verbatim/UNC prefix (`\\?\C:\...`), which
/// `Path::canonicalize` leaves on but which is meaningless to humans and to
/// `cat` / `tail` on typical Windows shells.
pub fn display_path(p: &Path) -> String {
    let raw = p.display().to_string();
    let unstripped = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    unstripped.replace('\\', "/")
}

use crate::errors::CliError;

/// CLI-wide flags that affect output. Set once at parse time.
#[derive(Debug, Clone, Copy)]
pub struct OutputMode {
    pub json: bool,
    pub quiet: bool,
    pub no_color: bool,
}

impl OutputMode {
    pub fn human() -> Self {
        Self {
            json: false,
            quiet: false,
            no_color: false,
        }
    }
}

/// Stable JSON envelope schema — agents key on `$schema` to detect breaking
/// changes. Bump to `knack://cli/v2` only if a field's meaning changes.
pub const SCHEMA: &str = "knack://cli/v1";

/// Successful result. `data` is whatever the command returns.
pub fn emit_ok<T: Serialize>(mode: OutputMode, data: T, human: impl FnOnce()) {
    if mode.json {
        let env = json!({
            "$schema": SCHEMA,
            "ok": true,
            "data": data,
        });
        println!("{}", env);
    } else {
        if !mode.quiet {
            human();
        }
    }
}

/// Set once the process has printed at least one error envelope. The main
/// boundary checks this to guarantee no command can exit nonzero silently:
/// most error sites call [`emit_err`] themselves, but any `?`-propagated
/// error that skipped it gets emitted at the boundary instead. (See
/// `main.rs` — this closed a real field bug where `publish` and `diff` on
/// the self-host backend exited 1/2 with zero bytes of output.)
static ERROR_EMITTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True if [`emit_err`] has run at least once in this process.
pub fn error_was_emitted() -> bool {
    ERROR_EMITTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Error envelope. Mirrors the API's `{ ok: false, error: { code, message } }`
/// shape so agents can branch on `error.code` without per-command parsing.
pub fn emit_err(mode: OutputMode, err: &CliError) {
    ERROR_EMITTED.store(true, std::sync::atomic::Ordering::Relaxed);
    if mode.json {
        let env = json!({
            "$schema": SCHEMA,
            "ok": false,
            "error": {
                "code": err.code(),
                "message": err.to_string(),
                "hint": err.hint(),
            },
        });
        // Errors go to stderr in human mode and stdout in JSON mode — agents
        // capture stdout to read the envelope.
        println!("{}", env);
        return;
    }

    let term = Term::stderr();
    let prefix = if mode.no_color {
        "error:".to_string()
    } else {
        style("error:").red().bold().to_string()
    };
    let _ = term.write_line(&format!("{} {}", prefix, err));
    if let Some(hint) = err.hint() {
        let hint_prefix = if mode.no_color {
            "hint:".to_string()
        } else {
            style("hint:").dim().to_string()
        };
        let _ = term.write_line(&format!("  {} {}", hint_prefix, hint));
    }
}

/// Human-readable progress / chatter. No-op in `--quiet` and never in JSON
/// mode (agents would have to filter it).
pub fn chatter(mode: OutputMode, line: impl AsRef<str>) {
    if mode.json || mode.quiet {
        return;
    }
    let _ = Term::stderr().write_line(line.as_ref());
}

/// Build a JSON envelope (without printing) — used by tests and by paths that
/// need to decide between stdout/stderr based on something more than the mode.
pub fn ok_envelope<T: Serialize>(data: T) -> Value {
    json!({
        "$schema": SCHEMA,
        "ok": true,
        "data": data,
    })
}

pub fn err_envelope(err: &CliError) -> Value {
    json!({
        "$schema": SCHEMA,
        "ok": false,
        "error": {
            "code": err.code(),
            "message": err.to_string(),
            "hint": err.hint(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_envelope_has_schema_marker() {
        let env = ok_envelope(json!({"x": 1}));
        assert_eq!(env["$schema"], SCHEMA);
        assert_eq!(env["ok"], true);
        assert_eq!(env["data"]["x"], 1);
    }

    #[test]
    fn err_envelope_carries_code_and_hint() {
        let err = CliError::AuthRequired;
        let env = err_envelope(&err);
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "AUTH_REQUIRED");
        assert!(env["error"]["hint"].is_string());
    }

    #[test]
    fn err_envelope_omits_hint_when_none() {
        let err = CliError::Network("dns".into());
        let env = err_envelope(&err);
        assert!(env["error"]["hint"].is_null());
    }

    #[test]
    fn schema_pins_v1() {
        // Changing this is a breaking contract change for every agent.
        assert_eq!(SCHEMA, "knack://cli/v1");
    }
}
