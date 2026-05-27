//! `knack pull <slug>[@<semver>] [--target ./dir]` — write a skill to disk.
//!
//! V2a default: if the SkillVersion has a `packed_s3_key`, download the
//! tarball via the bundle endpoint and unpack the full Anthropic Agent
//! Skills folder (SKILL.md + meta.knack.yaml + intuition.md + scripts/ +
//! assets/ + references/ + examples/ + tests/) into the target directory.
//!
//! Legacy fallback: pre-V2a versions store only three text columns and have
//! no bundle. We write just `SKILL.md`, `intuition.md`, `meta.knack.yaml`
//! into the target — matching the original layout.
//!
//! Idempotent on the legacy path: re-pulling overwrites only when content
//! actually changed.

use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::api::{skills as api_skills, ApiClient};
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{chatter, display_path, emit_err, emit_ok, OutputMode};
use crate::skill_pack::unpack_skill;

#[derive(Debug, Args)]
pub struct PullArgs {
    /// Skill identifier — `<slug>` or `<slug>@<semver>`.
    pub slug_at_version: String,

    /// Override the output base directory. The skill is written into a
    /// ``<slug>/`` subdirectory underneath. Default: nearest workspace's
    /// ``.knack/skills/`` (walking up from cwd), or ``./.knack/skills/``
    /// if no workspace ancestor exists.
    #[arg(long)]
    pub target: Option<PathBuf>,

    /// Write into the HOME-shared ``~/.knack/skills/`` pool instead of
    /// a workspace-local ``.knack/skills/``. Use this when you want a
    /// single skills pool shared across every project on this machine.
    #[arg(long)]
    pub global: bool,
}

pub async fn run(args: PullArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        return github_pull(&args, local_path, &client.config.skills_dir, mode).await;
    }

    let (slug, version_filter) = crate::slug::parse_slug_at_version(&args.slug_at_version);

    let skill = match api_skills::find_by_slug(&client, slug).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            let err = CliError::NotFound(format!("skill `{slug}` not found"));
            emit_err(mode, &err);
            return Err(err);
        }
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let semver = match version_filter {
        Some(v) => v.to_string(),
        None => match skill.current_version_semver.clone() {
            Some(v) => v,
            None => {
                let err = CliError::NotFound(format!("skill `{slug}` has no published version"));
                emit_err(mode, &err);
                return Err(err);
            }
        },
    };

    let version = match api_skills::get_version(&client, &skill.id, &semver).await {
        Ok(v) => v,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    // Workspace-local default: walk up from cwd looking for a `.knack/`,
    // fall back to creating one in cwd. `--global` opts back into the
    // HOME-shared pool; `--target` overrides everything.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let target_root = crate::workspace::resolve_skills_root(
        &cwd,
        args.global,
        args.target.as_deref(),
        &client.config.skills_dir,
    );
    let dir = target_root.join(&skill.slug);
    std::fs::create_dir_all(&dir)?;

    // V2a: if the version was published with a packed bundle, download and
    // unpack the whole folder. Legacy versions (packed_s3_key=None) take
    // the three-text-field write path below.
    let mode_label: &str;
    let written: Vec<PathBuf>;
    if version.packed_s3_key.is_some() {
        let dl = api_skills::bundle_download(&client, &skill.id, &version.version).await?;
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

    chatter(
        mode,
        format!(
            "pulled {}@{} ({}) → {}",
            skill.slug,
            version.version,
            mode_label,
            dir.display()
        ),
    );

    // Mirror the skill's server-side folder assignment into the
    // workspace's `.knack/folders.json` so `knack list --folder=X` and
    // `knack folder list` reflect cloud state without a second round
    // trip. Best-effort: a failure here doesn't fail the pull (the
    // folder name is recoverable via `knack folder list` later).
    if let Some(ws) = crate::workspace::discover_workspace_root(&cwd) {
        let mut idx = crate::workspace::read_folders_index(&ws).unwrap_or_default();
        let changed = match (&skill.folder_id, &skill.folder_name) {
            (Some(fid), Some(fname)) => crate::workspace::assign_to_folder(
                &mut idx,
                &skill.slug,
                fid,
                fname,
                &skill.scope,
                skill.owner_team_id.as_deref(),
            ),
            _ => crate::workspace::remove_from_folder(&mut idx, &skill.slug),
        };
        if changed {
            let _ = crate::workspace::write_folders_index(&ws, &idx);
        }
    }

    // Best-effort: register this skill with whichever agent runtime(s)
    // are recorded as installed on this machine. Failures (Redis hiccup,
    // unwritable .claude/, malformed frontmatter) get folded into the
    // report under `shims[].status = "skipped"` rather than turning the
    // pull into a non-zero exit — the canonical file is already on disk
    // and `knack sync` can recover later.
    let scope = if args.global {
        crate::commands::install::installed::Scope::Home
    } else {
        crate::commands::install::installed::Scope::Project
    };
    let shim_report = crate::commands::sync::sync_one_skill(&skill.slug, scope, &client.config);

    emit_ok(
        mode,
        json!({
            "skill_id": skill.id,
            "slug": skill.slug,
            "version": version.version,
            "path": dir,
            "files_written": written,
            "mode": mode_label,
            "shims": {
                "written": shim_report.written,
                "up_to_date": shim_report.up_to_date,
                "removed": shim_report.removed,
                "skipped": shim_report.skipped,
            },
        }),
        || {
            println!("✓ {}@{} → {}", skill.slug, version.version, dir.display());
            for r in &shim_report.written {
                println!("  ↪ {} shim → {}", r.agent, r.path);
            }
            for r in &shim_report.skipped {
                let reason = r.reason.as_deref().unwrap_or("?");
                println!("  ↪ {} shim skipped ({reason})", r.agent);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_if_changed_skips_when_identical() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.md");
        std::fs::write(&p, "hello").unwrap();
        let written = write_if_changed(&p, "hello").unwrap();
        assert!(written.is_empty());
    }

    #[test]
    fn write_if_changed_writes_on_diff() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.md");
        std::fs::write(&p, "hello").unwrap();
        let written = write_if_changed(&p, "world").unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "world");
    }

    #[test]
    fn write_if_changed_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new.md");
        let written = write_if_changed(&p, "data").unwrap();
        assert_eq!(written.len(), 1);
        assert!(p.exists());
    }
}

async fn github_pull(
    args: &PullArgs,
    local_path: &std::path::Path,
    home_skills_dir: &std::path::Path,
    mode: OutputMode,
) -> CliResult<()> {
    use knack_backend_github::GithubBackend;
    use knack_types::Backend;

    let spec = args.slug_at_version.clone();

    // `@owner/slug` (or `@owner/repo:slug`) -> external pull via Contents API.
    let pkg = if spec.starts_with('@') {
        let parsed = match knack_backend_github::parse_spec(&spec) {
            Ok(p) => p,
            Err(e) => {
                let err = CliError::User {
                    code: "INVALID_EXTERNAL_SPEC".into(),
                    message: format!("{e}"),
                    hint: Some(
                        "valid forms: `@owner/slug`, `@owner/repo:slug`, `@owner/repo:slug@v0.1.0`"
                            .into(),
                    ),
                };
                emit_err(mode, &err);
                return Err(err);
            }
        };
        match knack_backend_github::pull_external(&parsed).await {
            Ok(p) => p,
            Err(e) => {
                let err = CliError::NotFound(format!("external pull failed: {e}"));
                emit_err(mode, &err);
                return Err(err);
            }
        }
    } else {
        let (slug, version_filter) = crate::slug::parse_slug_at_version(&spec);
        let backend = GithubBackend::new("".to_string(), "".to_string(), local_path.to_path_buf());
        match backend.pull(slug, version_filter).await {
            Ok(p) => p,
            Err(e) => {
                let err = CliError::NotFound(format!("github pull: {e}"));
                emit_err(mode, &err);
                return Err(err);
            }
        }
    };

    // Resolve the target directory the same way cloud-mode pull does: explicit
    // --target wins, then --global, then nearest workspace's .knack/skills/.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let root = crate::workspace::resolve_skills_root(
        &cwd,
        args.global,
        args.target.as_deref(),
        home_skills_dir,
    );
    let dir = root.join(&pkg.slug);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        let err = CliError::Internal(format!("create target dir: {e}"));
        emit_err(mode, &err);
        return Err(err);
    }

    let mut written: Vec<String> = Vec::new();
    for file in &pkg.files {
        let dest = dir.join(&file.path);
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                let err = CliError::Internal(format!("mkdir {}: {e}", parent.display()));
                emit_err(mode, &err);
                return Err(err);
            }
        }
        if let Err(e) = std::fs::write(&dest, &file.bytes) {
            let err = CliError::Internal(format!("write {}: {e}", dest.display()));
            emit_err(mode, &err);
            return Err(err);
        }
        written.push(file.path.display().to_string());
    }

    emit_ok(
        mode,
        json!({
            "slug": pkg.slug,
            "version": pkg.version,
            "target": dir.display().to_string(),
            "files": written,
            "backend": "github",
        }),
        || {
            println!("✓ {} v{} → {}", pkg.slug, pkg.version, display_path(&dir));
            println!("  ({} files)", pkg.files.len());
        },
    );
    Ok(())
}
