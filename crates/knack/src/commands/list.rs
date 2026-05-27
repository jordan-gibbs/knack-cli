//! `knack list [--scope=public] [--folder=<name>]` — list skills.

use clap::Args;
use console::style;
use serde_json::json;

use crate::api::{folders as api_folders, skills as api_skills, ApiClient};
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Filter by scope: personal, team, or public.
    #[arg(long, value_parser = ["personal", "team", "public"])]
    pub scope: Option<String>,

    /// Filter to skills inside a specific folder (name lookup). Resolves
    /// the folder server-side via `GET /folders`; pass with `--scope` to
    /// disambiguate when the same name exists across personal and team
    /// scopes.
    #[arg(long)]
    pub folder: Option<String>,

    /// Show only unfiled skills (skills with no folder).
    #[arg(long, conflicts_with = "folder")]
    pub unfiled: bool,

    /// Page size cap. Defaults to 50, max 200.
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
}

pub async fn run(args: ListArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if let BackendMode::Github {
        owner,
        repo,
        local_path,
    } = &client.config.backend
    {
        return github_list(owner, repo, local_path, mode).await;
    }

    // Resolve --folder to a folder_id up front. If the name doesn't
    // exist we fail loud — silently returning an empty page would look
    // like "you have no matching skills" which is a lie.
    let folder_id: Option<String> = if let Some(name) = &args.folder {
        match api_folders::resolve(&client, name, args.scope.as_deref(), None).await {
            Ok(f) => Some(f.id),
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        }
    } else {
        None
    };

    let page = match api_skills::list_with_folder(
        &client,
        args.scope.as_deref(),
        folder_id.as_deref(),
        args.unfiled,
        args.limit,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            emit_err(mode, &e);
            return match e {
                CliError::AuthFailed(_) => Err(CliError::AuthRequired),
                other => Err(other),
            };
        }
    };

    emit_ok(
        mode,
        json!({
            "items": page.items,
            "next_cursor": page.next_cursor,
        }),
        || {
            if page.items.is_empty() {
                println!("(no skills yet. run `knack create <slug> --name \"...\"` after authoring SKILL.md)");
                return;
            }
            // Show a "Folder" column only when the caller didn't already
            // filter to a single folder (in that case it would be the
            // same value on every row — visual noise).
            let show_folder = folder_id.is_none() && !args.unfiled;
            for s in &page.items {
                let semver = s.current_version_semver.as_deref().unwrap_or("—");
                let folder_label = s
                    .folder_name
                    .as_deref()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".to_string());
                if show_folder {
                    println!(
                        "{:<28} {:<8} {:<8} {}",
                        s.slug,
                        style(semver).cyan(),
                        style(&s.scope).dim(),
                        style(folder_label).dim()
                    );
                } else {
                    println!(
                        "{:<28} {:<8} {}",
                        s.slug,
                        style(semver).cyan(),
                        style(&s.scope).dim()
                    );
                }
            }
        },
    );
    Ok(())
}

async fn github_list(
    owner: &str,
    repo: &str,
    local_path: &std::path::Path,
    mode: OutputMode,
) -> CliResult<()> {
    use knack_backend_github::GithubBackend;
    use knack_types::Backend;

    let backend = GithubBackend::new(
        owner.to_string(),
        repo.to_string(),
        local_path.to_path_buf(),
    );
    let skills = backend.list().await.map_err(|e| {
        let err = CliError::User {
            code: "GH_LIST_FAILED".into(),
            message: format!("github list failed: {e}"),
            hint: None,
        };
        emit_err(mode, &err);
        err
    })?;

    emit_ok(
        mode,
        json!({
            "items": skills.iter().map(|s| json!({
                "slug": s.slug,
                "version": s.version,
                "author": s.author,
                "description": s.description,
                "source": "github",
            })).collect::<Vec<_>>(),
            "backend": "github",
            "owner": owner,
            "repo": repo,
        }),
        || {
            if skills.is_empty() {
                println!("no skills yet. run `knack create <slug>` to author one.");
                return;
            }
            println!(
                "{:<32} {:<10} {}",
                style("slug").bold(),
                style("version").bold(),
                style("description").bold()
            );
            for s in &skills {
                let desc = s.description.as_deref().unwrap_or("");
                println!("{:<32} {:<10} {}", s.slug, s.version, desc);
            }
        },
    );
    Ok(())
}
