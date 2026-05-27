//! Output discipline: stdout = data, stderr = chatter.
//!
//! Track E §E6 mandates that stdout never gets human-friendly text mixed in
//! with the `--json` payload. This module is the only thing that writes to
//! stdout in the binary; commands emit their result through it and let the
//! mode (json vs human) decide formatting.

use console::{style, Term};
use serde::Serialize;
use serde_json::{json, Value};

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

/// Error envelope. Mirrors the API's `{ ok: false, error: { code, message } }`
/// shape so agents can branch on `error.code` without per-command parsing.
pub fn emit_err(mode: OutputMode, err: &CliError) {
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
