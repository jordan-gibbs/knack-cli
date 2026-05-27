//! `knack docs [topic]` — print embedded markdown docs.

use clap::Args;
use serde_json::json;

use crate::docs;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct DocsArgs {
    /// Topic slug (omit for table of contents). Use `all` to print everything.
    pub topic: Option<String>,
}

pub fn run(args: DocsArgs, mode: OutputMode) -> CliResult<()> {
    let body = match args.topic.as_deref() {
        None | Some("") => docs::toc(),
        Some("all") => docs::all(),
        Some(slug) => match docs::find(slug) {
            Some(t) => format!("# {}\n\n{}\n", t.title, t.body.trim_end()),
            None => {
                let err = CliError::NotFound(format!("unknown topic: {slug}"));
                emit_err(mode, &err);
                return Err(err);
            }
        },
    };

    if mode.json {
        let payload = json!({
            "topic": args.topic.clone().unwrap_or_else(|| "_toc".to_string()),
            "body": body,
        });
        emit_ok(mode, payload, || {});
    } else {
        // Plain stdout for human mode — pipeable to less, grep, etc.
        print!("{body}");
    }
    Ok(())
}
