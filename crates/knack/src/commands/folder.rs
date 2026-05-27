//! `knack folder` subcommands.
//!
//!   knack folder create <name> [--team-id <uuid>] [--parent <id-or-name>]
//!                                                    → POST   /folders
//!   knack folder list   [--scope <s>] [--team-id <t>] → GET    /folders
//!   knack folder rename <id-or-name> <new-name>      → PATCH  /folders/{id}
//!   knack folder reparent <id-or-name> <parent-id-or-name|--root>
//!                                                    → PATCH  /folders/{id}
//!   knack folder delete <id-or-name>                  → DELETE /folders/{id}
//!   knack folder mv <skill-slug> <folder-name>       → PATCH  /skills/{id}
//!   knack folder mv <skill-slug> --unfiled           → PATCH  /skills/{id}
//!
//! Folders are per-owner: a personal folder belongs to the calling user,
//! a team folder belongs to the team (created via ``--team-id``). They
//! organize personal and team skills only — public/marketplace skills
//! are never foldered.
//!
//! Workspace side-effect: every command that changes folder assignment
//! also updates the local ``.knack/folders.json`` so subsequent
//! ``knack list --folder=X`` calls don't need a server round-trip just
//! to know which folder this workspace's skills sit in.

use clap::{Args, Subcommand};
use serde_json::json;

use crate::api::{folders as api_folders, skills as api_skills, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{chatter, emit_err, emit_ok, OutputMode};
use crate::workspace::{
    assign_to_folder, discover_workspace_root, read_folders_index, remove_from_folder,
    write_folders_index, FoldersIndex,
};

#[derive(Debug, Subcommand)]
pub enum FolderCmd {
    /// Create a new folder (personal by default, or team if --team-id is set)
    Create(CreateArgs),
    /// List your folders
    List(ListArgs),
    /// Rename a folder
    Rename(RenameArgs),
    /// Move a folder under a new parent (or to the root with --root)
    Reparent(ReparentArgs),
    /// Delete a folder. Skills inside become unfiled.
    Delete(DeleteArgs),
    /// Move a skill into or out of a folder
    Mv(MvArgs),
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Folder name (1-80 chars).
    pub name: String,
    /// Team id. When set, creates a team folder. Caller must be a
    /// collaborator or owner of the team.
    #[arg(long)]
    pub team_id: Option<String>,
    /// Parent folder id or name. When set, the new folder is nested
    /// under that parent. The parent must have the same owner as the
    /// new folder (a personal folder can't parent a team folder and
    /// vice versa). Omit for a root-level folder.
    #[arg(long)]
    pub parent: Option<String>,
}

#[derive(Debug, Args)]
pub struct ReparentArgs {
    /// Folder to move. Either its UUID or its current name.
    pub id_or_name: String,
    /// New parent folder (UUID or name). Conflicts with --root.
    #[arg(conflicts_with = "root")]
    pub new_parent: Option<String>,
    /// Move the folder back to root (no parent). Conflicts with new_parent.
    #[arg(long, conflicts_with = "new_parent")]
    pub root: bool,
    /// Restrict the by-name lookup to a scope.
    #[arg(long, value_parser = ["personal", "team"])]
    pub scope: Option<String>,
    #[arg(long)]
    pub team_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Limit to a scope.
    #[arg(long, value_parser = ["personal", "team"])]
    pub scope: Option<String>,
    /// Limit to one team (implies --scope team).
    #[arg(long)]
    pub team_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct RenameArgs {
    /// Folder id (UUID) or current name.
    pub id_or_name: String,
    /// New name (1-80 chars).
    pub new_name: String,
    /// Restrict the by-name lookup to a scope.
    #[arg(long, value_parser = ["personal", "team"])]
    pub scope: Option<String>,
    #[arg(long)]
    pub team_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// Folder id (UUID) or name.
    pub id_or_name: String,
    #[arg(long, value_parser = ["personal", "team"])]
    pub scope: Option<String>,
    #[arg(long)]
    pub team_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct MvArgs {
    /// Skill slug to move.
    pub slug: String,
    /// Target folder name. Omit and pass --unfiled to clear the
    /// assignment.
    pub folder_name: Option<String>,
    /// Clear the folder assignment for the skill.
    #[arg(long, conflicts_with = "folder_name")]
    pub unfiled: bool,
    /// Restrict the folder lookup to a scope.
    #[arg(long, value_parser = ["personal", "team"])]
    pub scope: Option<String>,
    #[arg(long)]
    pub team_id: Option<String>,
}

pub async fn run(cmd: FolderCmd, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match cmd {
        FolderCmd::Create(a) => create(a, client, mode).await,
        FolderCmd::List(a) => list(a, client, mode).await,
        FolderCmd::Rename(a) => rename(a, client, mode).await,
        FolderCmd::Reparent(a) => reparent(a, client, mode).await,
        FolderCmd::Delete(a) => delete(a, client, mode).await,
        FolderCmd::Mv(a) => mv(a, client, mode).await,
    }
}

async fn create(args: CreateArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    // Resolve --parent (UUID or name → folder id) before creating so the
    // server gets a concrete UUID. Lookup is scope-aware: a team folder's
    // parent must also be a team folder, so we pass --team-id through.
    let parent_id: Option<String> = match &args.parent {
        None => None,
        Some(p) => {
            let scope_hint: Option<&str> = if args.team_id.is_some() {
                Some("team")
            } else {
                Some("personal")
            };
            match api_folders::resolve(&client, p, scope_hint, args.team_id.as_deref()).await {
                Ok(f) => Some(f.id),
                Err(e) => {
                    emit_err(mode, &e);
                    return Err(e);
                }
            }
        }
    };
    match api_folders::create(
        &client,
        &args.name,
        args.team_id.as_deref(),
        parent_id.as_deref(),
    )
    .await
    {
        Ok(f) => {
            // Stamp an empty entry into folders.json so the local index
            // reflects the cloud state without waiting for the next pull.
            if let Some(ws) = discover_workspace_root(&std::env::current_dir().unwrap_or_default())
            {
                let mut idx = read_folders_index(&ws).unwrap_or_default();
                let _ = assign_to_folder_stub(&mut idx, &f);
                let _ = write_folders_index(&ws, &idx);
            }
            emit_ok(
                mode,
                json!({
                    "id": f.id,
                    "name": f.name,
                    "scope": f.scope,
                    "owner_team_id": f.owner_team_id,
                }),
                || {
                    println!(
                        "✓ created folder {} (id: {}, scope: {})",
                        f.name, f.id, f.scope
                    )
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
    match api_folders::list(&client, args.scope.as_deref(), args.team_id.as_deref()).await {
        Ok(folders) => {
            emit_ok(
                mode,
                json!({
                    "folders": folders.iter().map(|f| json!({
                        "id": f.id,
                        "name": f.name,
                        "scope": f.scope,
                        "owner_team_id": f.owner_team_id,
                        "skill_count": f.skill_count,
                    })).collect::<Vec<_>>(),
                }),
                || {
                    if folders.is_empty() {
                        println!("(no folders)");
                        return;
                    }
                    for f in &folders {
                        let suffix = if f.skill_count == 1 {
                            "1 skill".to_string()
                        } else {
                            format!("{} skills", f.skill_count)
                        };
                        println!("  {:<28} {:<8} {}", f.name, f.scope, suffix);
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

async fn reparent(args: ReparentArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    // Validate that the caller asked for *something*: either a new
    // parent, or --root to promote to top-level. The clap conflict
    // guard handles the both-set case; we still need to catch the
    // neither-set case.
    if !args.root && args.new_parent.is_none() {
        let err = CliError::User {
            code: "REPARENT_NO_TARGET".into(),
            message: "pass a new parent folder id/name, or --root to promote to the top level"
                .into(),
            hint: Some("knack folder reparent <id-or-name> <parent-id-or-name> | --root".into()),
        };
        emit_err(mode, &err);
        return Err(err);
    }

    // Resolve the folder being moved.
    let folder = match api_folders::resolve(
        &client,
        &args.id_or_name,
        args.scope.as_deref(),
        args.team_id.as_deref(),
    )
    .await
    {
        Ok(f) => f,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    // Resolve the target parent (when not --root). Use the moved
    // folder's scope so we don't accidentally try to nest a personal
    // folder under a team folder; the server would 422 but the local
    // check gives a faster error.
    let new_parent_id: Option<String> = if args.root {
        None
    } else {
        let p = args.new_parent.as_ref().expect("guarded above");
        let scope_hint = Some(folder.scope.as_str());
        let team_hint = folder.owner_team_id.as_deref();
        match api_folders::resolve(&client, p, scope_hint, team_hint).await {
            Ok(parent) => Some(parent.id),
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        }
    };

    match api_folders::reparent(&client, &folder.id, new_parent_id.as_deref()).await {
        Ok(updated) => {
            emit_ok(
                mode,
                json!({
                    "id": updated.id,
                    "name": updated.name,
                    "parent_folder_id": updated.parent_folder_id,
                }),
                || {
                    if let Some(p) = updated.parent_folder_id.as_deref() {
                        println!("✓ moved {} under parent {}", updated.name, p);
                    } else {
                        println!("✓ promoted {} to root", updated.name);
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

async fn rename(args: RenameArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let folder = match api_folders::resolve(
        &client,
        &args.id_or_name,
        args.scope.as_deref(),
        args.team_id.as_deref(),
    )
    .await
    {
        Ok(f) => f,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };
    match api_folders::rename(&client, &folder.id, &args.new_name).await {
        Ok(f) => {
            sync_folder_rename_local(&f.id, &f.name);
            emit_ok(mode, json!({ "id": f.id, "name": f.name }), || {
                println!("✓ renamed → {}", f.name)
            });
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

async fn delete(args: DeleteArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let folder = match api_folders::resolve(
        &client,
        &args.id_or_name,
        args.scope.as_deref(),
        args.team_id.as_deref(),
    )
    .await
    {
        Ok(f) => f,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };
    match api_folders::delete(&client, &folder.id).await {
        Ok(()) => {
            sync_folder_delete_local(&folder.id);
            emit_ok(
                mode,
                json!({ "id": folder.id, "name": folder.name, "status": "deleted" }),
                || {
                    println!(
                        "✓ deleted folder {} (contained skills are now unfiled)",
                        folder.name
                    )
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

async fn mv(args: MvArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    // Resolve the skill first — we need its id (the server addresses
    // PATCH /skills/{id} by id, not slug) and its scope to verify the
    // folder lookup matches.
    let skill = match api_skills::find_by_slug(&client, &args.slug).await? {
        Some(s) => s,
        None => {
            let err = CliError::NotFound(format!("skill `{}` not found", args.slug));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    if args.unfiled {
        let body = api_skills::SkillUpdate {
            folder_id: Some(None),
            ..Default::default()
        };
        match api_skills::update(&client, &skill.id, &body).await {
            Ok(updated) => {
                sync_skill_folder_local(&updated.slug, None, None, None, None);
                emit_ok(
                    mode,
                    json!({ "slug": updated.slug, "folder_id": null }),
                    || println!("✓ {} is now unfiled", updated.slug),
                );
                return Ok(());
            }
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        }
    }

    let Some(folder_name) = args.folder_name else {
        let err = CliError::User {
            code: "MISSING_FOLDER".into(),
            message: "pass a folder name or --unfiled".into(),
            hint: Some("knack folder mv <skill-slug> <folder-name> | --unfiled".into()),
        };
        emit_err(mode, &err);
        return Err(err);
    };

    // Pick the folder scope based on the skill's scope unless the
    // caller overrode it. Personal skills go in personal folders; team
    // skills go in the team's folders.
    let derived_scope: Option<&str> = match args.scope.as_deref() {
        Some(s) => Some(s),
        None => match skill.scope.as_str() {
            "personal" => Some("personal"),
            "team" => Some("team"),
            _ => None,
        },
    };
    let team_id = args.team_id.clone().or_else(|| skill.owner_team_id.clone());

    let folder = match api_folders::resolve(
        &client,
        &folder_name,
        derived_scope,
        team_id.as_deref(),
    )
    .await
    {
        Ok(f) => f,
        Err(CliError::NotFound(_)) => {
            // Friendly path: create on the fly when the user typed a
            // name that doesn't exist yet. Same semantics as
            // ``knack folder create <name> && knack folder mv …``.
            chatter(mode, format!("creating folder `{}`…", folder_name));
            api_folders::create(
                &client,
                &folder_name,
                if derived_scope == Some("team") {
                    team_id.as_deref()
                } else {
                    None
                },
                None, // root-level when auto-creating from `mv`
            )
            .await?
        }
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let body = api_skills::SkillUpdate {
        folder_id: Some(Some(folder.id.clone())),
        ..Default::default()
    };
    match api_skills::update(&client, &skill.id, &body).await {
        Ok(updated) => {
            sync_skill_folder_local(
                &updated.slug,
                Some(&folder.id),
                Some(&folder.name),
                Some(&folder.scope),
                folder.owner_team_id.as_deref(),
            );
            emit_ok(
                mode,
                json!({
                    "slug": updated.slug,
                    "folder_id": folder.id,
                    "folder_name": folder.name,
                }),
                || println!("✓ {} → {}", updated.slug, folder.name),
            );
            Ok(())
        }
        Err(e) => {
            emit_err(mode, &e);
            Err(e)
        }
    }
}

// ─── workspace-side index helpers ──────────────────────────────────────────

/// Stamp an empty folder entry into folders.json. Used by ``create`` so
/// the new folder is visible to ``knack folder list`` immediately.
fn assign_to_folder_stub(idx: &mut FoldersIndex, f: &api_folders::Folder) -> bool {
    if idx.folders.iter().any(|e| e.id == f.id) {
        return false;
    }
    idx.folders.push(crate::workspace::FolderIndexEntry {
        id: f.id.clone(),
        name: f.name.clone(),
        scope: f.scope.clone(),
        owner_team_id: f.owner_team_id.clone(),
        slugs: Vec::new(),
    });
    idx.folders.sort_by(|a, b| a.name.cmp(&b.name));
    true
}

/// Rename a folder in the local index (after a server-side rename).
fn sync_folder_rename_local(folder_id: &str, new_name: &str) {
    let Some(ws) = discover_workspace_root(&std::env::current_dir().unwrap_or_default()) else {
        return;
    };
    let mut idx = match read_folders_index(&ws) {
        Ok(i) => i,
        Err(_) => return,
    };
    let mut changed = false;
    for entry in &mut idx.folders {
        if entry.id == folder_id && entry.name != new_name {
            entry.name = new_name.to_string();
            changed = true;
        }
    }
    if changed {
        idx.folders.sort_by(|a, b| a.name.cmp(&b.name));
        let _ = write_folders_index(&ws, &idx);
    }
}

/// Drop a folder entry from the local index (after a server-side delete).
fn sync_folder_delete_local(folder_id: &str) {
    let Some(ws) = discover_workspace_root(&std::env::current_dir().unwrap_or_default()) else {
        return;
    };
    let mut idx = match read_folders_index(&ws) {
        Ok(i) => i,
        Err(_) => return,
    };
    let before = idx.folders.len();
    idx.folders.retain(|e| e.id != folder_id);
    if idx.folders.len() != before {
        let _ = write_folders_index(&ws, &idx);
    }
}

/// Stamp a skill's new folder assignment into the local index. Pass
/// ``folder_id = None`` to unfile.
fn sync_skill_folder_local(
    slug: &str,
    folder_id: Option<&str>,
    folder_name: Option<&str>,
    scope: Option<&str>,
    owner_team_id: Option<&str>,
) {
    let Some(ws) = discover_workspace_root(&std::env::current_dir().unwrap_or_default()) else {
        return;
    };
    let mut idx = match read_folders_index(&ws) {
        Ok(i) => i,
        Err(_) => return,
    };
    let changed = match (folder_id, folder_name, scope) {
        (Some(id), Some(name), Some(sc)) => {
            assign_to_folder(&mut idx, slug, id, name, sc, owner_team_id)
        }
        _ => remove_from_folder(&mut idx, slug),
    };
    if changed {
        let _ = write_folders_index(&ws, &idx);
    }
}
