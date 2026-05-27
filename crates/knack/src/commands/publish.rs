//! `knack publish <slug> [--major|--minor|--patch]` — push the local skill
//! folder as a new immutable version.
//!
//! V2a default flow: pack the entire folder (SKILL.md + meta.knack.yaml +
//! optional scripts/ / assets/ / references/ / examples/ / tests/ /
//! intuition.md) into a deterministic gzip tarball, presign-upload to R2,
//! PUT the bytes, then POST the version with packed_s3_key set. The server
//! derives skill_md / intuition_md / meta_yaml from the bundle so the text
//! columns and the tarball stay in lockstep.
//!
//! Legacy fallback: if the server doesn't support the bundle endpoints (a
//! pre-V2a deployment), we send the three text fields as before.

use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::json;

use crate::api::{skills as api_skills, ApiClient};
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{chatter, emit_err, emit_ok, OutputMode};
use crate::skill_pack::pack_skill;

#[derive(Debug, Args)]
pub struct PublishArgs {
    pub slug: String,

    /// Folder containing SKILL.md. Default resolution order, first hit:
    ///   1. nearest workspace's ``.knack/drafts/<slug>/``
    ///   2. nearest workspace's ``.knack/skills/<slug>/``
    ///   3. legacy HOME pool ``~/.knack/skills/<slug>/``
    /// Pass ``--from <path>`` to override.
    #[arg(long)]
    pub from: Option<PathBuf>,

    /// Override version. Otherwise inferred from --major/--minor/--patch.
    #[arg(long, conflicts_with_all = ["major", "minor", "patch"])]
    pub as_version: Option<String>,

    #[arg(long)]
    pub major: bool,
    #[arg(long)]
    pub minor: bool,
    #[arg(long)]
    pub patch: bool,

    /// Pack + validate locally and print the file manifest. Skips the API
    /// upload entirely. Useful for verifying what would be sent before
    /// burning a version number.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: PublishArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    if let BackendMode::Github {
        owner,
        repo,
        local_path,
    } = &client.config.backend
    {
        return github_publish(&args, owner, repo, local_path, mode).await;
    }

    let dir = match args.from.clone() {
        Some(p) => p,
        None => {
            // Walk drafts/ → skills/ → HOME. Drafts wins because it's
            // where `knack create` parks scaffolded works-in-progress;
            // re-publishing a pulled skill (fork-style) is the rarer
            // second path; the legacy HOME pool only matters for
            // ~/.knack/skills/ users who haven't migrated.
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            crate::workspace::resolve_existing_skill_dir(
                &args.slug,
                &cwd,
                &client.config.skills_dir,
            )
            .unwrap_or_else(|| client.config.skills_dir.join(&args.slug))
        }
    };

    if !dir.is_dir() {
        let err = CliError::User {
            code: "PUBLISH_NO_FOLDER".into(),
            message: format!("not a directory: {}", dir.display()),
            hint: Some("pass --from <dir> with a folder containing SKILL.md".into()),
        };
        emit_err(mode, &err);
        return Err(err);
    }

    if args.dry_run {
        return dry_run(&args, &dir, mode);
    }

    let skill = match api_skills::find_by_slug(&client, &args.slug).await? {
        Some(s) => s,
        None => {
            let err = CliError::NotFound(format!("skill `{}` not found", args.slug));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    let current_semver = skill
        .current_version_semver
        .clone()
        .unwrap_or_else(|| "0.0.0".into());

    // Default bump is patch (--patch flag is documented but not load-bearing —
    // its absence still picks the patch path).
    let next = match (args.as_version.clone(), args.major, args.minor) {
        (Some(v), _, _) => v,
        (_, true, _) => bump(&current_semver, BumpKind::Major)?,
        (_, _, true) => bump(&current_semver, BumpKind::Minor)?,
        _ => bump_patch(&current_semver)?,
    };

    let parent_id = skill.current_version_id.clone();

    // V2a bundle path: pack the folder, presign-upload, PUT, POST with
    // packed_s3_key. If the server is pre-V2a (404 on presign-bundle), fall
    // back to the legacy three-text-field path so an old API still works.
    let bundle_outcome = match try_publish_with_bundle(
        &client,
        &skill.id,
        &next,
        parent_id.as_deref(),
        &dir,
    )
    .await
    {
        Ok(v) => PublishOutcome::Bundle(v),
        Err(CliError::NotFound(msg)) if msg.contains("presign-bundle") => {
            chatter(
                mode,
                "Server lacks bundle endpoints; falling back to legacy text fields.",
            );
            PublishOutcome::Legacy
        }
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let new_version = match bundle_outcome {
        PublishOutcome::Bundle(v) => v,
        PublishOutcome::Legacy => {
            let skill_md = read_required(&dir.join("SKILL.md"))?;
            let intuition_md = read_optional(&dir.join("intuition.md"));
            let meta_yaml = read_optional(&dir.join("meta.knack.yaml"));
            match api_skills::create_version(
                &client,
                &skill.id,
                &api_skills::SkillVersionCreate {
                    version: next.clone(),
                    skill_md,
                    intuition_md,
                    meta_yaml,
                    parent_version_id: parent_id.clone(),
                    artifact_ids: vec![],
                    packed_s3_key: None,
                },
            )
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    emit_err(mode, &e);
                    return Err(e);
                }
            }
        }
    };

    // After publishing, also make sure the local agent shims know about
    // this skill. A brand-new publish typically means the user just
    // authored it in `.knack/drafts/<slug>/` — the corresponding
    // canonical `.knack/skills/<slug>/` may not even exist yet. We pass
    // the draft dir directly to the shim renderer so the local agent
    // can discover the skill immediately in the current session.
    let shim_report = crate::commands::sync::sync_one_skill(
        &args.slug,
        crate::commands::install::installed::Scope::Project,
        &client.config,
    );

    emit_ok(
        mode,
        json!({
            "slug": args.slug,
            "skill_id": skill.id,
            "version": new_version.version,
            "parent_version_id": parent_id,
            "packed_s3_key": new_version.packed_s3_key,
            "from": dir,
            "shims": {
                "written": shim_report.written,
                "up_to_date": shim_report.up_to_date,
                "removed": shim_report.removed,
                "skipped": shim_report.skipped,
            },
        }),
        || {
            println!("✓ {}@{} published", args.slug, new_version.version);
            for r in &shim_report.written {
                println!("  ↪ {} shim → {}", r.agent, r.path);
            }
        },
    );
    Ok(())
}

enum PublishOutcome {
    Bundle(api_skills::SkillVersion),
    Legacy,
}

/// `--dry-run` path: validate the folder locally, pack the tarball, print the
/// manifest, return success. Never touches the network. Lets agents verify
/// what would be sent before burning a server-side version number.
fn dry_run(args: &PublishArgs, dir: &Path, mode: OutputMode) -> CliResult<()> {
    let report = crate::skill_validators::validate_skill_folder(dir);
    if !report.is_ok() {
        let summary = report.summary();
        let err = CliError::User {
            code: "SKILL_FORMAT_INVALID".into(),
            message: format!("skill validation failed. issues: {summary}"),
            hint: Some("fix the listed fields in meta.knack.yaml / SKILL.md and re-run".into()),
        };
        if mode.json {
            let env = serde_json::json!({
                "$schema": "knack://cli/v1",
                "ok": false,
                "error": {
                    "code": "SKILL_FORMAT_INVALID",
                    "message": err.to_string(),
                    "details": report.into_details(),
                    "hint": "fix the listed fields in meta.knack.yaml / SKILL.md and re-run",
                },
            });
            println!("{env}");
        } else {
            emit_err(mode, &err);
        }
        return Err(err);
    }

    let packed = match pack_skill(dir) {
        Ok(p) => p,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let target_semver = args
        .as_version
        .clone()
        .unwrap_or_else(|| "(server-assigned)".into());

    emit_ok(
        mode,
        json!({
            "dry_run": true,
            "slug": args.slug,
            "from": dir.display().to_string(),
            "target_version": target_semver,
            "tarball_sha256": packed.sha256,
            "tarball_size": packed.bytes.len(),
            "files": packed.manifest.files.iter()
                .map(|(p, sha)| json!({ "path": p, "sha256": sha }))
                .collect::<Vec<_>>(),
        }),
        || {
            println!(
                "✓ dry-run ok — {} files, {} bytes, sha256 {}",
                packed.manifest.files.len(),
                packed.bytes.len(),
                &packed.sha256,
            );
            println!("would publish {} → version {}", args.slug, target_semver);
            for (p, sha) in &packed.manifest.files {
                println!("  {}  {p}", &sha[..12]);
            }
        },
    );
    Ok(())
}

async fn try_publish_with_bundle(
    client: &ApiClient,
    skill_id: &str,
    next_semver: &str,
    parent_version_id: Option<&str>,
    dir: &Path,
) -> Result<api_skills::SkillVersion, CliError> {
    let packed = pack_skill(dir)?;
    let presign = api_skills::presign_bundle(client, skill_id).await?;

    // PUT the tarball bytes to the presigned R2 URL. No auth header — the
    // signature in the URL is the credential.
    let put_status = reqwest::Client::new()
        .put(&presign.upload_url)
        .header("Content-Type", "application/gzip")
        .body(packed.bytes)
        .send()
        .await?
        .status();
    if !put_status.is_success() {
        return Err(CliError::Server {
            status: put_status.as_u16(),
            code: "BUNDLE_UPLOAD_FAILED".into(),
            message: format!("R2 PUT returned {put_status}"),
        });
    }

    let version = api_skills::create_version(
        client,
        skill_id,
        &api_skills::SkillVersionCreate {
            version: next_semver.to_string(),
            skill_md: String::new(),
            intuition_md: String::new(),
            meta_yaml: String::new(),
            parent_version_id: parent_version_id.map(str::to_string),
            artifact_ids: vec![],
            packed_s3_key: Some(presign.s3_key),
        },
    )
    .await?;
    Ok(version)
}

// Compatibility shim: pre-V2a publish helpers (used only by the legacy
// fallback above and the older tests until they catch up).

#[derive(Copy, Clone)]
enum BumpKind {
    Major,
    Minor,
}

/// `1.0.0` → `1.0.1`. Mirrors the Python helper at
/// `apps/api/knack_api/services/skills.py:bump_patch`.
async fn github_publish(
    args: &PublishArgs,
    owner: &str,
    repo: &str,
    local_path: &Path,
    mode: OutputMode,
) -> CliResult<()> {
    use knack_backend_github::GithubBackend;
    use knack_types::{Backend, SkillManifest, SkillPackage};

    let skill_dir = local_path.join("skills").join(&args.slug);
    if !skill_dir.is_dir() {
        let err = CliError::User {
            code: "PUBLISH_NO_FOLDER".into(),
            message: format!("no skill at {}", skill_dir.display()),
            hint: Some(format!("run `knack create {}` first", args.slug)),
        };
        emit_err(mode, &err);
        return Err(err);
    }

    // Resolve next version. Priority: --as-version, then --major/--minor/--patch
    // bump from meta.knack.yaml's current version, then default to 0.1.0.
    let version = resolve_github_version(args, &skill_dir)?;

    // Write the bumped version back to meta.knack.yaml on disk BEFORE the
    // publish commit. Otherwise the committed file (and any subsequent
    // `knack run`) would still report the pre-bump value, putting the
    // telemetry log out of sync with the git tag.
    if !args.dry_run {
        if let Err(e) = update_meta_version(&skill_dir, &version) {
            let err = CliError::Internal(format!("update meta.knack.yaml: {e}"));
            emit_err(mode, &err);
            return Err(err);
        }
    }

    if args.dry_run {
        emit_ok(
            mode,
            json!({
                "slug": &args.slug,
                "version": &version,
                "dry_run": true,
                "owner": owner,
                "repo": repo,
            }),
            || {
                println!("✓ dry-run: would publish {}@v{}", args.slug, version);
                println!(
                    "  → github.com/{}/{} (tag {}/v{})",
                    owner, repo, args.slug, version
                );
            },
        );
        return Ok(());
    }

    // Empty files signals "use what's on disk at skills/<slug>/" to the
    // GithubBackend, so we don't have to round-trip through tarball + extract.
    let package = SkillPackage {
        slug: args.slug.clone(),
        version: version.clone(),
        manifest: SkillManifest {
            slug: args.slug.clone(),
            version: version.clone(),
            description: None,
            entry: PathBuf::from("SKILL.md"),
            assets: Vec::new(),
            created_at: chrono::Utc::now(),
        },
        files: Vec::new(),
    };

    let backend = GithubBackend::new(
        owner.to_string(),
        repo.to_string(),
        local_path.to_path_buf(),
    );
    let receipt = backend.publish(package).await.map_err(|e| {
        let err = CliError::User {
            code: "GH_PUBLISH_FAILED".into(),
            message: format!("github publish failed: {e}"),
            hint: None,
        };
        emit_err(mode, &err);
        err
    })?;

    emit_ok(
        mode,
        json!({
            "slug": receipt.slug,
            "version": receipt.version,
            "url": receipt.url,
            "backend": "github",
        }),
        || {
            println!("✓ {}@v{}", receipt.slug, receipt.version);
            println!("  → {}", receipt.url);
        },
    );
    Ok(())
}

/// Rewrite the `version:` field in `meta.knack.yaml`, preserving every
/// other line (and ordering, and comments) untouched. Textual line-level
/// replacement on purpose — `serde_yaml` would round-trip-drop comments.
fn update_meta_version(skill_dir: &Path, new_version: &str) -> std::io::Result<()> {
    let meta_path = skill_dir.join("meta.knack.yaml");
    let raw = std::fs::read_to_string(&meta_path)?;
    let mut replaced = false;
    let mut out_lines: Vec<String> = Vec::with_capacity(raw.lines().count() + 1);
    for line in raw.lines() {
        let trimmed = line.trim_start();
        if !replaced && trimmed.starts_with("version:") {
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            out_lines.push(format!("{indent}version: {new_version}"));
            replaced = true;
        } else {
            out_lines.push(line.to_string());
        }
    }
    if !replaced {
        // No prior version key — append one.
        out_lines.push(format!("version: {new_version}"));
    }
    let mut content = out_lines.join("\n");
    if raw.ends_with('\n') {
        content.push('\n');
    }
    std::fs::write(&meta_path, content)
}

fn resolve_github_version(args: &PublishArgs, skill_dir: &Path) -> CliResult<String> {
    if let Some(v) = &args.as_version {
        return Ok(v.trim_start_matches('v').to_string());
    }
    // Read current version from meta.knack.yaml. Default 0.1.0 if missing.
    let meta_path = skill_dir.join("meta.knack.yaml");
    let current: String = if meta_path.exists() {
        let bytes = std::fs::read(&meta_path).map_err(CliError::from)?;
        let parsed: serde_yaml::Value =
            serde_yaml::from_slice(&bytes).map_err(|e| CliError::User {
                code: "META_INVALID".into(),
                message: format!("could not parse meta.knack.yaml: {e}"),
                hint: None,
            })?;
        parsed
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("0.0.0")
            .to_string()
    } else {
        "0.0.0".to_string()
    };

    if args.major {
        bump(&current, BumpKind::Major)
    } else if args.minor {
        bump(&current, BumpKind::Minor)
    } else if args.patch {
        bump_patch(&current)
    } else if current == "0.0.0" {
        Ok("0.1.0".to_string())
    } else {
        bump_patch(&current)
    }
}

fn bump_patch(semver: &str) -> CliResult<String> {
    let s = semver.strip_prefix('v').unwrap_or(semver);
    let parts: Vec<&str> = s.split('.').collect();
    let (a, b, c) = match parts.as_slice() {
        [a, b] => (a, b, "0"),
        [a, b, c] => (a, b, *c),
        _ => {
            return Err(CliError::User {
                code: "PUBLISH_BAD_SEMVER".into(),
                message: format!("can't parse semver `{semver}`"),
                hint: None,
            });
        }
    };
    let parse = |x: &str| {
        x.parse::<u64>().map_err(|_| CliError::User {
            code: "PUBLISH_BAD_SEMVER".into(),
            message: format!("non-numeric component in `{semver}`"),
            hint: None,
        })
    };
    let (a, b, c) = (parse(a)?, parse(b)?, parse(c)?);
    Ok(format!("{}.{}.{}", a, b, c + 1))
}

fn bump(semver: &str, kind: BumpKind) -> CliResult<String> {
    let s = semver.strip_prefix('v').unwrap_or(semver);
    let parts: Vec<&str> = s.split('.').collect();
    let parse = |x: &str| {
        x.parse::<u64>().map_err(|_| CliError::User {
            code: "PUBLISH_BAD_SEMVER".into(),
            message: format!("non-numeric component in `{semver}`"),
            hint: None,
        })
    };
    let (a, b, _c) = match parts.as_slice() {
        [a, b] => (parse(a)?, parse(b)?, 0_u64),
        [a, b, c] => (parse(a)?, parse(b)?, parse(c)?),
        _ => {
            return Err(CliError::User {
                code: "PUBLISH_BAD_SEMVER".into(),
                message: format!("can't parse `{semver}`"),
                hint: None,
            });
        }
    };
    let bumped = match kind {
        BumpKind::Major => format!("{}.0.0", a + 1),
        BumpKind::Minor => format!("{}.{}.0", a, b + 1),
    };
    Ok(bumped)
}

fn read_required(path: &Path) -> CliResult<String> {
    std::fs::read_to_string(path).map_err(|e| CliError::User {
        code: "PUBLISH_MISSING_FILE".into(),
        message: format!("can't read {}: {e}", path.display()),
        hint: Some("did you `knack pull` first, or write SKILL.md to the folder?".into()),
    })
}

fn read_optional(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_major() {
        assert_eq!(bump("1.2.3", BumpKind::Major).unwrap(), "2.0.0");
    }

    #[test]
    fn bump_minor() {
        assert_eq!(bump("1.2.3", BumpKind::Minor).unwrap(), "1.3.0");
        assert_eq!(bump("1.2", BumpKind::Minor).unwrap(), "1.3.0");
    }

    #[test]
    fn bump_rejects_garbage() {
        assert!(bump("nope", BumpKind::Major).is_err());
    }

    #[test]
    fn bump_patch_basic() {
        assert_eq!(bump_patch("1.0.0").unwrap(), "1.0.1");
        assert_eq!(bump_patch("0.1").unwrap(), "0.1.1");
        assert_eq!(bump_patch("v2.3.4").unwrap(), "2.3.5");
    }

    #[test]
    fn bump_patch_rejects_garbage() {
        assert!(bump_patch("not-a-version").is_err());
        assert!(bump_patch("1.x.0").is_err());
    }
}
