//! `knack interview` — agent-driven skill authoring.
//!
//! The Knack interview is conducted by the user's own agent (Claude Code,
//! Cursor, Codex, etc.), not by a server-side LLM. This command's job is
//! purely state plumbing:
//!
//!   1. Drop the embedded interview skill into the current project's agent
//!      skills directory so the agent picks it up.
//!   2. Hold session state on disk between turns so the conversation can
//!      span multiple invocations.
//!   3. Move the session through the six phases as the agent reports each
//!      phase complete.

use clap::{Args, Subcommand};
use include_dir::{include_dir, Dir};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::errors::{CliError, CliResult};
use crate::output::OutputMode;

/// The embedded interview skill — bundled into the binary at compile time.
/// At runtime we write its contents into `<cwd>/.claude/skills/knack-interview/`
/// (or the equivalent for whichever agent surface is in use) so the agent
/// loads it like any other skill.
static INTERVIEW_SKILL: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../skills/interview");

#[derive(Debug, Args)]
pub struct InterviewArgs {
    #[command(subcommand)]
    pub cmd: InterviewCmd,
}

#[derive(Debug, Subcommand)]
pub enum InterviewCmd {
    /// Begin a new interview. Drops the interview skill into the current
    /// project and prints a session id the agent passes to subsequent calls.
    Start(StartArgs),
    /// Re-emit the interview skill for an existing session.
    Resume(ResumeArgs),
    /// Persist captured data for the current phase.
    Save(SaveArgs),
    /// Mark the current phase complete and advance to the next.
    Advance(AdvanceArgs),
    /// Show the current phase and captured state.
    Status(StatusArgs),
}

#[derive(Debug, Args)]
pub struct StartArgs {
    /// Where to drop the skill files. Defaults to `<cwd>/.claude/skills/`.
    #[arg(long)]
    pub target_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ResumeArgs {
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub target_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SaveArgs {
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub phase: Phase,
    /// JSON payload to persist for the named phase.
    #[arg(long)]
    pub data: String,
}

#[derive(Debug, Args)]
pub struct AdvanceArgs {
    #[arg(long)]
    pub session: String,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[arg(long)]
    pub session: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Genesis,
    Artifacts,
    Intuition,
    Compile,
    Refine,
    Publish,
}

impl Phase {
    fn next(self) -> Option<Phase> {
        match self {
            Phase::Genesis => Some(Phase::Artifacts),
            Phase::Artifacts => Some(Phase::Intuition),
            Phase::Intuition => Some(Phase::Compile),
            Phase::Compile => Some(Phase::Refine),
            Phase::Refine => Some(Phase::Publish),
            Phase::Publish => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Session {
    id: String,
    phase: Phase,
    data: serde_json::Value,
}

pub async fn run(args: InterviewArgs, mode: OutputMode) -> CliResult<()> {
    match args.cmd {
        InterviewCmd::Start(a) => start(a, mode),
        InterviewCmd::Resume(a) => resume(a, mode),
        InterviewCmd::Save(a) => save(a, mode),
        InterviewCmd::Advance(a) => advance(a, mode),
        InterviewCmd::Status(a) => status(a, mode),
    }
}

fn start(args: StartArgs, mode: OutputMode) -> CliResult<()> {
    let session_id = Uuid::new_v4().to_string();
    let target = resolve_target(args.target_dir)?;
    write_skill(&target)?;

    let session = Session {
        id: session_id.clone(),
        phase: Phase::Genesis,
        data: serde_json::json!({}),
    };
    save_session(&session)?;

    if mode.json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "session": session_id,
                "phase": "genesis",
                "skill_dir": target.display().to_string(),
            })
        );
    } else if !mode.quiet {
        println!("knack interview: session {session_id} started (phase: genesis)");
        println!("skill written to {}", target.display());
        println!("the agent should load .claude/skills/knack-interview/SKILL.md to begin");
    }
    Ok(())
}

fn resume(args: ResumeArgs, mode: OutputMode) -> CliResult<()> {
    let session = load_session(&args.session)?;
    let target = resolve_target(args.target_dir)?;
    write_skill(&target)?;

    if mode.json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "session": session.id,
                "phase": phase_str(session.phase),
                "data": session.data,
                "skill_dir": target.display().to_string(),
            })
        );
    } else if !mode.quiet {
        println!(
            "knack interview: resumed session {} (phase: {})",
            session.id,
            phase_str(session.phase)
        );
    }
    Ok(())
}

fn save(args: SaveArgs, mode: OutputMode) -> CliResult<()> {
    let mut session = load_session(&args.session)?;
    let value: serde_json::Value =
        serde_json::from_str(&args.data).map_err(|e| CliError::User {
            code: "INVALID_JSON".into(),
            message: format!("--data is not valid JSON: {e}"),
            hint: None,
        })?;

    let key = phase_str(args.phase);
    let obj = session.data.as_object_mut().ok_or_else(|| CliError::User {
        code: "STATE_CORRUPT".into(),
        message: "session data is not a JSON object".into(),
        hint: Some("delete the session file and start a new interview".into()),
    })?;
    obj.insert(key.into(), value);
    save_session(&session)?;

    if mode.json {
        println!("{}", serde_json::json!({ "ok": true }));
    } else if !mode.quiet {
        println!("knack interview: saved phase {} for {}", key, session.id);
    }
    Ok(())
}

fn advance(args: AdvanceArgs, mode: OutputMode) -> CliResult<()> {
    let mut session = load_session(&args.session)?;
    let Some(next) = session.phase.next() else {
        return Err(CliError::User {
            code: "ALREADY_AT_LAST_PHASE".into(),
            message: "interview is already at the publish phase".into(),
            hint: Some("run `knack publish <slug>` to finish".into()),
        });
    };
    session.phase = next;
    save_session(&session)?;

    if mode.json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "phase": phase_str(next) })
        );
    } else if !mode.quiet {
        println!(
            "knack interview: advanced to {} for {}",
            phase_str(next),
            session.id
        );
    }
    Ok(())
}

fn status(args: StatusArgs, mode: OutputMode) -> CliResult<()> {
    let session = load_session(&args.session)?;
    if mode.json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "session": session.id,
                "phase": phase_str(session.phase),
                "data": session.data,
            })
        );
    } else if !mode.quiet {
        println!("session: {}", session.id);
        println!("phase:   {}", phase_str(session.phase));
        println!(
            "data:    {}",
            serde_json::to_string_pretty(&session.data).unwrap_or_default()
        );
    }
    Ok(())
}

fn phase_str(p: Phase) -> &'static str {
    match p {
        Phase::Genesis => "genesis",
        Phase::Artifacts => "artifacts",
        Phase::Intuition => "intuition",
        Phase::Compile => "compile",
        Phase::Refine => "refine",
        Phase::Publish => "publish",
    }
}

fn resolve_target(override_dir: Option<PathBuf>) -> CliResult<PathBuf> {
    let base = match override_dir {
        Some(p) => p,
        None => std::env::current_dir()?.join(".claude").join("skills"),
    };
    Ok(base.join("knack-interview"))
}

fn write_skill(target: &Path) -> CliResult<()> {
    std::fs::create_dir_all(target).map_err(CliError::from)?;
    for file in INTERVIEW_SKILL.files() {
        let dest = target.join(file.path().file_name().unwrap_or_default());
        std::fs::write(&dest, file.contents()).map_err(CliError::from)?;
    }
    Ok(())
}

fn sessions_dir() -> CliResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| CliError::User {
        code: "NO_HOME_DIR".into(),
        message: "could not resolve $HOME".into(),
        hint: None,
    })?;
    let dir = home.join(".knack").join("sessions");
    std::fs::create_dir_all(&dir).map_err(CliError::from)?;
    Ok(dir)
}

fn session_file(id: &str) -> CliResult<PathBuf> {
    Ok(sessions_dir()?.join(format!("{id}.json")))
}

fn save_session(session: &Session) -> CliResult<()> {
    let path = session_file(&session.id)?;
    let bytes = serde_json::to_vec_pretty(session).map_err(|e| CliError::User {
        code: "SERIALIZE".into(),
        message: format!("serialize session: {e}"),
        hint: None,
    })?;
    std::fs::write(&path, bytes).map_err(CliError::from)
}

fn load_session(id: &str) -> CliResult<Session> {
    let path = session_file(id)?;
    let bytes =
        std::fs::read(&path).map_err(|_| CliError::NotFound(format!("session {id} not found")))?;
    serde_json::from_slice(&bytes).map_err(|e| CliError::User {
        code: "STATE_CORRUPT".into(),
        message: format!("session {id} is corrupt: {e}"),
        hint: Some("delete the session file and start a new interview".into()),
    })
}
