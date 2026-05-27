//! `knack fork @author/slug [--slug=NEW] [--name="..."]` — fork a public
//! marketplace skill into the caller's personal library and pull the
//! bundle into ``<workspace>/.knack/drafts/<slug>/`` for editing.
//!
//! Two server round-trips: resolve `@author/slug` via the anonymous
//! marketplace detail endpoint, then POST `/skills/{id}/fork` to create
//! the personal copy and copy the bundle bytes server-side. After the
//! fork lands we pull the new shell's bundle to disk under `drafts/` —
//! signalling that this is edit-intent, not a read-only consume.

use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::api::{skills as api_skills, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};
use crate::skill_pack::unpack_skill;

#[derive(Debug, Args)]
pub struct ForkArgs {
    /// Source skill, in the form `@author/slug`. Must be public.
    pub handle_slug: String,

    /// Override the slug of your new copy. Defaults to the original's
    /// slug; per-owner uniqueness means the same slug can live in your
    /// library and in the original author's at the same time.
    #[arg(long)]
    pub slug: Option<String>,

    /// Override the display name of your new copy. Defaults to the
    /// original's name.
    #[arg(long)]
    pub name: Option<String>,

    /// Write the unpacked bundle to a specific directory instead of the
    /// workspace's `drafts/<slug>/`. Useful for scripts and tests.
    #[arg(long)]
    pub target: Option<PathBuf>,

    /// Write into the HOME-shared `~/.knack/drafts/` pool instead of
    /// a workspace-local `drafts/`. Mirrors `--global` on `knack pull`.
    #[arg(long)]
    pub global: bool,
}

pub async fn run(args: ForkArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    // 1. Parse @author/slug; reject bare slugs (fork only makes sense
    //    for public skills, which must be addressed by handle).
    let (author, source_slug) = match api_skills::parse_handle_slug(&args.handle_slug) {
        Some(parts) => parts,
        None => {
            let err = CliError::User {
                code: "INVALID_HANDLE".to_string(),
                message: format!(
                    "fork target must be in the form `@author/slug`, got `{}`",
                    args.handle_slug
                ),
                hint: Some("example: `knack fork @knack/claude-api`".to_string()),
            };
            emit_err(mode, &err);
            return Err(err);
        }
    };

    // 2. Resolve the public skill_id via the anonymous marketplace
    //    detail endpoint. Works without sign-in for the lookup; the
    //    POST in step 3 then enforces auth.
    let (source_id, _source_semver) =
        match api_skills::resolve_public(&client, &author, &source_slug).await {
            Ok(t) => t,
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        };

    // 3. Create the server-side fork. Body fields are optional; the
    //    server defaults to the original's slug + name when omitted.
    let body = api_skills::SkillFork {
        slug: args.slug.clone(),
        name: args.name.clone(),
    };
    let new_skill = match api_skills::fork(&client, &source_id, &body).await {
        Ok(s) => s,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    // 4. Pull the new skill's bundle to drafts/<slug>/ for editing.
    //    The fork inherits the original's bundle bytes server-side, so
    //    this download mirrors what `knack pull` would write — just
    //    landing under drafts/ instead of skills/ to signal edit-intent.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let drafts_root = crate::workspace::resolve_drafts_root(
        &cwd,
        args.global,
        args.target.as_deref(),
        &client.config.skills_dir,
    );
    let dir = drafts_root.join(&new_skill.slug);
    std::fs::create_dir_all(&dir)?;

    let semver = new_skill
        .current_version_semver
        .clone()
        .unwrap_or_else(|| "0.1.0".to_string());
    let version = match api_skills::get_version(&client, &new_skill.id, &semver).await {
        Ok(v) => v,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let mode_label: &str;
    let written: Vec<PathBuf>;
    if version.packed_s3_key.is_some() {
        let dl = api_skills::bundle_download(&client, &new_skill.id, &version.version).await?;
        let resp = reqwest::Client::new().get(&dl.url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(CliError::Server {
                status: status.as_u16(),
                code: "BUNDLE_DOWNLOAD_FAILED".into(),
                message: format!("R2 GET returned {status}"),
            });
        }
        let bytes = resp.bytes().await?;
        let manifest = unpack_skill(&bytes, &dir)?;
        written = manifest
            .files
            .keys()
            .map(|arcname| dir.join(arcname.replace('/', std::path::MAIN_SEPARATOR_STR)))
            .collect();
        mode_label = "bundle";
    } else {
        // No packed bundle on the source (legacy pre-V2a row). Fall back
        // to writing the three canonical text files — same shape as the
        // legacy branch in `knack pull`.
        let mut acc = Vec::new();
        acc.extend(write_if_changed(&dir.join("SKILL.md"), &version.skill_md)?);
        acc.extend(write_if_changed(
            &dir.join("intuition.md"),
            &version.intuition_md,
        )?);
        acc.extend(write_if_changed(
            &dir.join("meta.knack.yaml"),
            &version.meta_yaml,
        )?);
        written = acc;
        mode_label = "legacy";
    }

    let owner_handle = new_skill
        .owner_username
        .clone()
        .unwrap_or_else(|| "me".to_string());

    emit_ok(
        mode,
        json!({
            "skill_id": new_skill.id,
            "slug": new_skill.slug,
            "version": version.version,
            "forked_from": {
                "author": author,
                "slug": source_slug,
            },
            "path": dir,
            "files_written": written,
            "mode": mode_label,
        }),
        || {
            println!(
                "✓ forked @{}/{} → @{}/{}  drafts/{}/",
                author, source_slug, owner_handle, new_skill.slug, new_skill.slug,
            );
            println!("  edit then `knack publish {}` when ready", new_skill.slug);
        },
    );
    Ok(())
}

/// Idempotent write — returns the path iff bytes changed (or file was new).
fn write_if_changed(path: &std::path::Path, content: &str) -> std::io::Result<Vec<PathBuf>> {
    if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        if existing == content {
            return Ok(Vec::new());
        }
    }
    std::fs::write(path, content)?;
    Ok(vec![path.to_path_buf()])
}
