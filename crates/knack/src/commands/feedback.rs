//! `knack feedback` subcommands.
//!
//!   knack feedback open  --subject "…" --body "…" [--run R] [--skill S] [--cli-meta]
//!   knack feedback list  [--status open|closed|all] [--json]
//!   knack feedback show  <thread-id>
//!   knack feedback reply <thread-id> --body "…"
//!
//! A feedback thread is a two-sided support conversation: the user's
//! account (web + CLI agents) on one side, Knack staff on the other.
//! The optional `--run` / `--skill` / `--cli-meta` switches attach
//! machine-readable evidence so staff can land directly on the right
//! Run or Skill row without back-and-forth.
//!
//! Reading a thread (`show`) advances the server-side read pointer so
//! the `X-Knack-Notices: feedback` banner stops firing on the next API
//! call.

use std::io::Read;

use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::api::{feedback as api_feedback, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Subcommand)]
pub enum FeedbackCmd {
    /// Open a new thread (sends the first message in the same request)
    Open(OpenArgs),
    /// List your threads (defaults to all statuses)
    List(ListArgs),
    /// Show a single thread + advance the read pointer
    Show(ShowArgs),
    /// Reply to an existing open thread
    Reply(ReplyArgs),
}

#[derive(Debug, Args)]
pub struct OpenArgs {
    /// Subject line (1-160 chars).
    #[arg(long)]
    pub subject: String,

    /// Body for the first message. Pass `-` to read from stdin so long
    /// stack traces can be piped: `cat trace.log | knack feedback open
    /// --subject "..." --body -`.
    #[arg(long)]
    pub body: String,

    /// Optional Run id to attach as evidence.
    #[arg(long)]
    pub run: Option<String>,

    /// Optional Skill id to attach as evidence.
    #[arg(long)]
    pub skill: Option<String>,

    /// Auto-attach the CLI version + OS as `cli_context`. Useful for
    /// bugs that aren't tied to a single skill or run.
    #[arg(long)]
    pub cli_meta: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Filter by status. Defaults to "all".
    #[arg(long, value_parser = ["open", "closed", "all"])]
    pub status: Option<String>,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Thread id (UUID).
    pub thread_id: String,
}

#[derive(Debug, Args)]
pub struct ReplyArgs {
    /// Thread id (UUID).
    pub thread_id: String,

    /// Reply body. Pass `-` to read from stdin.
    #[arg(long)]
    pub body: String,
}

pub async fn run(cmd: FeedbackCmd, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match cmd {
        FeedbackCmd::Open(a) => open(a, client, mode).await,
        FeedbackCmd::List(a) => list(a, client, mode).await,
        FeedbackCmd::Show(a) => show(a, client, mode).await,
        FeedbackCmd::Reply(a) => reply(a, client, mode).await,
    }
}

fn resolve_body(raw: &str) -> Result<String, CliError> {
    if raw == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| CliError::User {
                code: "STDIN_READ_FAILED".into(),
                message: format!("could not read --body from stdin: {e}"),
                hint: None,
            })?;
        let trimmed = buf.trim().to_string();
        if trimmed.is_empty() {
            return Err(CliError::User {
                code: "EMPTY_BODY".into(),
                message: "stdin produced an empty body".into(),
                hint: Some("pipe non-empty content or pass --body \"...\"".into()),
            });
        }
        Ok(trimmed)
    } else {
        Ok(raw.to_string())
    }
}

fn cli_meta_blob() -> Value {
    json!({
        "cli_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    })
}

async fn open(args: OpenArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let body = match resolve_body(&args.body) {
        Ok(b) => b,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };
    let cli_context = if args.cli_meta {
        Some(cli_meta_blob())
    } else {
        None
    };

    match api_feedback::open(
        &client,
        &args.subject,
        &body,
        args.run.as_deref(),
        args.skill.as_deref(),
        cli_context.as_ref(),
    )
    .await
    {
        Ok(thread) => {
            emit_ok(
                mode,
                json!({
                    "id": thread.id,
                    "subject": thread.subject,
                    "status": thread.status,
                    "message_count": thread.messages.len(),
                }),
                || {
                    println!("✓ thread opened — id: {}", thread.id);
                    println!("  subject: {}", thread.subject);
                    println!(
                        "  reply later: knack feedback reply {} --body \"…\"",
                        thread.id
                    );
                },
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

async fn list(args: ListArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let status_filter = args.status.as_deref();
    // "all" means no server-side filter; treat as None.
    let status_for_query = match status_filter {
        Some("open") => Some("open"),
        Some("closed") => Some("closed"),
        _ => None,
    };

    match api_feedback::list(&client, status_for_query).await {
        Ok(items) => {
            emit_ok(
                mode,
                json!({
                    "threads": items.iter().map(|t| json!({
                        "id": t.id,
                        "subject": t.subject,
                        "status": t.status,
                        "has_unread_admin_replies": t.has_unread_admin_replies,
                        "message_count": t.message_count,
                        "updated_at": t.updated_at,
                    })).collect::<Vec<_>>(),
                }),
                || {
                    if items.is_empty() {
                        println!("(no threads)");
                        return;
                    }
                    for t in &items {
                        let unread = if t.has_unread_admin_replies {
                            "•"
                        } else {
                            " "
                        };
                        println!(
                            "  {} {:<10} {:<8} {}",
                            unread,
                            short_id(&t.id),
                            t.status,
                            t.subject
                        );
                    }
                    let unread_count = items.iter().filter(|t| t.has_unread_admin_replies).count();
                    if unread_count > 0 {
                        eprintln!(
                            "  ({} thread(s) with unread replies — `knack feedback show <id>`)",
                            unread_count
                        );
                    }
                },
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

async fn show(args: ShowArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match api_feedback::show(&client, &args.thread_id).await {
        Ok(thread) => {
            emit_ok(
                mode,
                serde_json::to_value(&thread).unwrap_or(Value::Null),
                || {
                    println!(
                        "thread {} — {} ({})",
                        thread.id, thread.subject, thread.status
                    );
                    if thread.run_id.is_some() || thread.skill_id.is_some() {
                        if let Some(r) = &thread.run_id {
                            println!("  attached run:   {r}");
                        }
                        if let Some(s) = &thread.skill_id {
                            println!("  attached skill: {s}");
                        }
                    }
                    for m in &thread.messages {
                        let who = if m.from_side == "admin" {
                            "knack"
                        } else {
                            "you"
                        };
                        println!(
                            "\n  [{}] {}",
                            who,
                            m.created_at.format("%Y-%m-%d %H:%M UTC")
                        );
                        for line in m.body.lines() {
                            println!("    {}", line);
                        }
                    }
                    if thread.status == "open" {
                        println!("\n  reply: knack feedback reply {} --body \"…\"", thread.id);
                    }
                },
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

async fn reply(args: ReplyArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let body = match resolve_body(&args.body) {
        Ok(b) => b,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };
    match api_feedback::reply(&client, &args.thread_id, &body).await {
        Ok(thread) => {
            emit_ok(
                mode,
                json!({
                    "id": thread.id,
                    "status": thread.status,
                    "message_count": thread.messages.len(),
                }),
                || println!("✓ reply sent to thread {}", thread.id),
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}
