//! `knack edit <slug> [--name X] [--description Y] [--scope personal|team|public] [--team <slug>]`
//!
//! Wraps PATCH /skills/{id}. Slug is immutable server-side by design — keep
//! it stable so share links and agent context files never break.
//!
//! Scope transitions:
//!   * personal ↔ public — the publish gate
//!   * personal → team — transfer into a team's library (requires --team)
//!   * team → team — reassign to a different team (requires --team)
//!   * team → personal — pull back into the actor's personal library

use clap::Args;
use serde_json::json;

use crate::api::{skills as api_skills, teams as api_teams, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct EditArgs {
    /// Slug of the skill to edit (your own personal or team skills only).
    pub slug: String,

    /// New display name (1-200 chars).
    #[arg(long)]
    pub name: Option<String>,

    /// New one-line description (0-280 chars).
    #[arg(long)]
    pub description: Option<String>,

    /// New visibility scope. `public` requires a claimed username and counts
    /// against the `max_public_skills` plan quota. `team` transfers the
    /// skill into a team's library — pair with --team.
    #[arg(long, value_parser = ["personal", "team", "public"])]
    pub scope: Option<String>,

    /// Target team slug (or id) when --scope team. Resolved against the
    /// caller's team memberships; pass either the human slug (e.g.
    /// `bookkeeping-crew`) or the UUID directly.
    #[arg(long)]
    pub team: Option<String>,
}

pub async fn run(args: EditArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if args.name.is_none()
        && args.description.is_none()
        && args.scope.is_none()
        && args.team.is_none()
    {
        let err = CliError::User {
            code: "EDIT_NO_CHANGES".into(),
            message:
                "no fields supplied. pass at least one of --name / --description / --scope / --team"
                    .into(),
            hint: Some("slug is immutable; if you want a different slug, create a new skill".into()),
        };
        emit_err(mode, &err);
        return Err(err);
    }

    // --team without --scope team is almost certainly a user mistake. Fail
    // fast with a clear message instead of silently sending the team_id
    // and watching the backend ignore it.
    if args.team.is_some() && args.scope.as_deref() != Some("team") {
        let err = CliError::User {
            code: "EDIT_TEAM_WITHOUT_SCOPE".into(),
            message: "--team only makes sense with --scope team".into(),
            hint: Some("did you mean: --scope team --team <slug>".into()),
        };
        emit_err(mode, &err);
        return Err(err);
    }

    let skill = match api_skills::find_by_slug(&client, &args.slug).await? {
        Some(s) => s,
        None => {
            let err = CliError::NotFound(format!("skill `{}` not found", args.slug));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    // Resolve target team if needed. We accept either the slug or the UUID.
    // For UX, look up by slug first; if it doesn't match, send the input
    // through as-is and let the backend reject if it's neither a slug nor a
    // valid id (clean 404 from the server).
    let owner_team_id = if args.scope.as_deref() == Some("team") {
        match args.team.as_deref() {
            Some(s) => Some(resolve_team(&client, s, mode).await?),
            None => {
                let err = CliError::User {
                    code: "EDIT_MISSING_TEAM".into(),
                    message: "--scope team requires --team <slug>".into(),
                    hint: Some(
                        "list your teams: `knack teams list` (returns slugs you can pass to --team)"
                            .into(),
                    ),
                };
                emit_err(mode, &err);
                return Err(err);
            }
        }
    } else {
        None
    };

    let body = api_skills::SkillUpdate {
        name: args.name.clone(),
        description: args.description.clone(),
        scope: args.scope.clone(),
        owner_team_id,
        folder_id: None,
    };

    let updated = match api_skills::update(&client, &skill.id, &body).await {
        Ok(s) => s,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    emit_ok(
        mode,
        json!({
            "slug": updated.slug,
            "skill_id": updated.id,
            "name": updated.name,
            "description": updated.description,
            "scope": updated.scope,
            "owner_team_id": updated.owner_team_id,
        }),
        || {
            let mut changes: Vec<String> = Vec::new();
            if args.name.is_some() {
                changes.push(format!("name → {:?}", updated.name));
            }
            if args.description.is_some() {
                changes.push(format!("description → {:?}", updated.description));
            }
            if args.scope.is_some() {
                changes.push(format!("scope → {}", updated.scope));
            }
            if let Some(team_id) = &updated.owner_team_id {
                if args.scope.as_deref() == Some("team") {
                    changes.push(format!("team → {}", team_id));
                }
            }
            println!("✓ {} updated: {}", updated.slug, changes.join(", "));
        },
    );
    Ok(())
}

/// Resolve a user-supplied `--team <input>` to a team id. Accepts both
/// slugs (`bookkeeping-crew`) and raw UUIDs. If the input doesn't match
/// any of the caller's memberships by slug, we return it verbatim — the
/// PATCH route will 404 if it's not a real team id either, which is the
/// clean failure mode.
async fn resolve_team(
    client: &ApiClient,
    input: &str,
    mode: OutputMode,
) -> Result<String, CliError> {
    let teams = api_teams::list_my(client).await?;
    if let Some(t) = teams.iter().find(|t| t.slug == input || t.id == input) {
        return Ok(t.id.clone());
    }
    // Helpful message before falling through — the agent gets a list of
    // valid slugs instead of just a 404 from the server.
    let slugs: Vec<&str> = teams.iter().map(|t| t.slug.as_str()).collect();
    let err = CliError::User {
        code: "TEAM_NOT_FOUND".into(),
        message: format!("you're not on a team called `{}`", input),
        hint: if slugs.is_empty() {
            Some("you're not on any teams; create one in the workspace or accept an invite first".into())
        } else {
            Some(format!("your teams: {}", slugs.join(", ")))
        },
    };
    emit_err(mode, &err);
    Err(err)
}
