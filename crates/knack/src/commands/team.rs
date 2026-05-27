//! `knack team` subcommands.
//!
//!   knack team create <slug> --name X      → POST /teams
//!   knack team list                         → GET /teams
//!   knack team show <team-id>               → GET /teams/{id}
//!   knack team invite <team-id> <email>     → POST /teams/{id}/invites
//!   knack team accept <invite-token>        → POST /teams/invites/accept
//!   knack team role <team-id> <user-id> <r> → PATCH /teams/{id}/memberships/{user_id}

use clap::{Args, Subcommand};
use serde_json::json;

use crate::api::{teams as api_teams, ApiClient};
use crate::errors::CliResult;
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Subcommand)]
pub enum TeamCmd {
    /// Create a new team. You become its owner.
    Create(CreateArgs),
    /// List the teams you're a member of
    List,
    /// Show one team's metadata
    Show(ShowArgs),
    /// Invite someone by email
    Invite(InviteArgs),
    /// Accept an invite token (received by email)
    Accept(AcceptArgs),
    /// Change a member's role (owner only)
    Role(RoleArgs),
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Slug for the team. Lowercase, hyphens (`^[a-z0-9][a-z0-9-]*$`).
    pub slug: String,
    /// Display name (1-200 chars).
    #[arg(long)]
    pub name: String,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Team id (UUID).
    pub team_id: String,
}

#[derive(Debug, Args)]
pub struct InviteArgs {
    /// Team id (UUID).
    pub team_id: String,
    /// Invitee email.
    pub email: String,
    /// Role to grant on accept. Defaults to `collaborator`.
    #[arg(long, value_parser = ["owner", "collaborator", "viewer"], default_value = "collaborator")]
    pub role: String,
}

#[derive(Debug, Args)]
pub struct AcceptArgs {
    /// Invite token from the invitation email.
    pub invite_token: String,
}

#[derive(Debug, Args)]
pub struct RoleArgs {
    /// Team id.
    pub team_id: String,
    /// Target user id (UUID).
    pub user_id: String,
    /// New role.
    #[arg(value_parser = ["owner", "collaborator", "viewer"])]
    pub role: String,
}

pub async fn run(cmd: TeamCmd, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match cmd {
        TeamCmd::Create(a) => create(a, client, mode).await,
        TeamCmd::List => list_my(client, mode).await,
        TeamCmd::Show(a) => show(a, client, mode).await,
        TeamCmd::Invite(a) => invite(a, client, mode).await,
        TeamCmd::Accept(a) => accept(a, client, mode).await,
        TeamCmd::Role(a) => role(a, client, mode).await,
    }
}

async fn create(args: CreateArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match api_teams::create(&client, &args.name, &args.slug).await {
        Ok(t) => {
            emit_ok(
                mode,
                json!({
                    "id": t.id,
                    "slug": t.slug,
                    "name": t.name,
                    "plan": t.plan,
                }),
                || println!("✓ created team {} (id: {})", t.slug, t.id),
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

async fn list_my(client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match api_teams::list_my(&client).await {
        Ok(teams) => {
            emit_ok(
                mode,
                json!({ "teams": teams.iter().map(|t| json!({
                    "id": t.id,
                    "slug": t.slug,
                    "name": t.name,
                    "plan": t.plan,
                })).collect::<Vec<_>>() }),
                || {
                    if teams.is_empty() {
                        println!("(no teams)");
                        return;
                    }
                    for t in &teams {
                        println!("  {} ({}) — {}  plan={}", t.slug, t.name, t.id, t.plan);
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
    match api_teams::get(&client, &args.team_id).await {
        Ok(t) => {
            emit_ok(
                mode,
                json!({
                    "id": t.id,
                    "slug": t.slug,
                    "name": t.name,
                    "plan": t.plan,
                }),
                || println!("{} ({})\nid: {}\nplan: {}", t.name, t.slug, t.id, t.plan),
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

async fn invite(args: InviteArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match api_teams::invite(&client, &args.team_id, &args.email, &args.role).await {
        Ok(inv) => {
            emit_ok(
                mode,
                json!({
                    "invite_id": inv.id,
                    "email": inv.email,
                    "role": inv.role,
                    "status": inv.status,
                    "invite_token": inv.invite_token,
                }),
                || {
                    println!(
                        "✓ invited {} as {} (status: {}). Pass invite_token via email \
                         (server already does this in prod).",
                        inv.email, inv.role, inv.status,
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

async fn accept(args: AcceptArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match api_teams::accept(&client, &args.invite_token).await {
        Ok(t) => {
            emit_ok(
                mode,
                json!({
                    "id": t.id,
                    "slug": t.slug,
                    "name": t.name,
                }),
                || println!("✓ joined team {} ({})", t.slug, t.name),
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

async fn role(args: RoleArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match api_teams::set_role(&client, &args.team_id, &args.user_id, &args.role).await {
        Ok(t) => {
            emit_ok(
                mode,
                json!({
                    "team_id": t.id,
                    "user_id": args.user_id,
                    "role": args.role,
                }),
                || println!("✓ set {} → {} on {}", args.user_id, args.role, t.slug),
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}
