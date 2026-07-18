//! `knack status [<slug>]` — the truth command for "which copy is real?"
//!
//! A skill can exist in up to four places at once: a workspace draft
//! (`.knack/drafts/<slug>`), a workspace skill (`.knack/skills/<slug>`),
//! the HOME pool (`~/.knack/skills/<slug>`), and — self-host only — the
//! registry clone (`<repo>/skills/<slug>`). A July 2026 field report
//! documented two agents editing the same skill from two different
//! copies with only git archaeology catching it, and stale publishes
//! going undetected because nothing answered "is the local copy ahead
//! of the published version, and in which files?" This command answers
//! both, and shows which copy the current CWD would resolve for
//! `publish`/`run` so mismatches are visible before they bite.
//!
//! * `knack status` — offline overview: every locally present skill,
//!   its copies, and whether any two copies disagree. No network.
//! * `knack status <slug>` — deep view of one skill: adds the latest
//!   published version and the per-file changed list of the resolved
//!   copy against it (self-host compares the full tagged tree; cloud
//!   compares the three canonical text files the API serves).
//!
//! Comparisons are EOL-insensitive (CRLF↔LF), matching the publish
//! verify — otherwise every Windows clone with `core.autocrlf true`
//! would report permanent phantom drift.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::json;

use crate::api::{skills as api_skills, ApiClient};
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{display_path, emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Skill slug. Omit for the all-skills overview (offline).
    pub slug: Option<String>,
}

/// One locally present copy of a skill.
#[derive(Debug, Clone, serde::Serialize)]
struct Copy {
    /// `workspace-draft` | `workspace-skill` | `home-pool` | `registry`
    kind: &'static str,
    path: String,
    /// `version:` from the copy's meta.knack.yaml, when parseable.
    version: Option<String>,
    /// True when this is the copy `publish`/`run` would resolve from
    /// the current working directory.
    resolved: bool,
}

pub async fn run(args: StatusArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    match &args.slug {
        Some(slug) => skill_status(slug, &client, mode).await,
        None => overview(&client, mode),
    }
}

// ─── overview (no slug, no network) ────────────────────────────────────────

fn overview(client: &ApiClient, mode: OutputMode) -> CliResult<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let ws = crate::workspace::discover_workspace_root(&cwd);
    let registry_root = match &client.config.backend {
        BackendMode::Github { local_path, .. } => Some(local_path.clone()),
        _ => None,
    };

    let mut slugs: Vec<String> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(ws) = &ws {
        roots.push(ws.join("drafts"));
        roots.push(ws.join("skills"));
    }
    roots.push(client.config.skills_dir.clone());
    if let Some(r) = &registry_root {
        roots.push(r.join("skills"));
    }
    for root in &roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && p.join("SKILL.md").is_file() {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if seen.insert(name.to_string()) {
                        slugs.push(name.to_string());
                    }
                }
            }
        }
    }
    slugs.sort();

    let mut rows = Vec::new();
    for slug in &slugs {
        let copies = copies_of(slug, &cwd, client);
        let drift = copies_disagree(&copies);
        rows.push(json!({
            "slug": slug,
            "copies": copies,
            "copies_disagree": drift,
        }));
    }

    emit_ok(
        mode,
        json!({
            "backend": backend_tag(client),
            "workspace": ws.as_ref().map(|p| display_path(p)),
            "home_pool": display_path(&client.config.skills_dir),
            "registry": registry_root.as_ref().map(|p| display_path(p)),
            "skills": rows,
        }),
        || {
            if slugs.is_empty() {
                println!("no local skills found");
                return;
            }
            for slug in &slugs {
                let copies = copies_of(slug, &cwd, client);
                let drift = copies_disagree(&copies);
                let flag = if drift { "  ⚠ copies disagree" } else { "" };
                println!("{slug}{flag}");
                for c in &copies {
                    let mark = if c.resolved { "→" } else { " " };
                    println!(
                        "  {mark} {:<16} {}  ({})",
                        c.kind,
                        c.path,
                        c.version.as_deref().unwrap_or("?")
                    );
                }
            }
            println!();
            println!("→ marks the copy `publish`/`run` resolve from this directory.");
            println!("`knack status <slug>` compares against the published version.");
        },
    );
    Ok(())
}

// ─── single skill (adds upstream comparison) ───────────────────────────────

async fn skill_status(slug: &str, client: &ApiClient, mode: OutputMode) -> CliResult<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let copies = copies_of(slug, &cwd, client);
    if copies.is_empty() {
        let err = CliError::NotFound(format!("no local copy of `{slug}` found"));
        emit_err(mode, &err);
        return Err(err);
    }
    let resolved = copies.iter().find(|c| c.resolved).cloned();

    let (published, changed): (Option<String>, Option<Vec<String>>) =
        match &client.config.backend {
            BackendMode::Github { local_path, .. } => {
                let latest = knack_backend_github::latest_published_version(local_path, slug);
                // Compare the resolved copy when one exists; otherwise
                // fall back to the registry working copy — for a
                // self-host user whose only copy IS the clone, "does my
                // working tree match the last tag?" is precisely the
                // stale-publish question this command exists to answer.
                let compare = resolved
                    .clone()
                    .or_else(|| copies.iter().find(|c| c.kind == "registry").cloned());
                let changed = match (&compare, &latest) {
                    (Some(c), Some(_)) => {
                        Some(changed_vs_tag(local_path, slug, Path::new(&c.path)).await?)
                    }
                    _ => None,
                };
                (latest, changed)
            }
            _ => cloud_comparison(client, slug, resolved.as_ref()).await?,
        };

    let drift = copies_disagree(&copies);
    emit_ok(
        mode,
        json!({
            "slug": slug,
            "backend": backend_tag(client),
            "copies": copies,
            "copies_disagree": drift,
            "published_version": published,
            // Files where the resolved copy differs from the published
            // version. Cloud compares SKILL.md / meta.knack.yaml /
            // intuition.md (what the API serves); self-host compares the
            // full tagged tree.
            "changed_vs_published": changed,
        }),
        || {
            println!("skill: {slug}");
            for c in &copies {
                let mark = if c.resolved { "→" } else { " " };
                println!(
                    "  {mark} {:<16} {}  ({})",
                    c.kind,
                    c.path,
                    c.version.as_deref().unwrap_or("?")
                );
            }
            if drift {
                println!("  ⚠ local copies disagree with each other");
            }
            match &published {
                Some(v) => println!("  published: {v}"),
                None => println!("  published: (none)"),
            }
            match &changed {
                Some(files) if files.is_empty() => {
                    println!("  resolved copy matches the published version");
                }
                Some(files) => {
                    println!("  resolved copy differs from published in: {}", files.join(", "));
                }
                None => {}
            }
        },
    );
    Ok(())
}

/// Cloud: latest version + changed-file list over the three canonical
/// text files (`GET /skills/{id}/versions/{semver}` serves exactly
/// those; comparing bundle assets would require a full pull).
async fn cloud_comparison(
    client: &ApiClient,
    slug: &str,
    resolved: Option<&Copy>,
) -> CliResult<(Option<String>, Option<Vec<String>>)> {
    let Some(skill) = api_skills::find_by_slug(client, slug).await? else {
        return Ok((None, None));
    };
    let Some(latest) = skill.current_version_semver.clone() else {
        return Ok((None, None));
    };
    let Some(res) = resolved else {
        return Ok((Some(latest), None));
    };
    let version = api_skills::get_version(client, &skill.id, &latest).await?;
    let dir = Path::new(&res.path);
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    let mut changed = Vec::new();
    for (name, remote) in [
        ("SKILL.md", &version.skill_md),
        ("intuition.md", &version.intuition_md),
        ("meta.knack.yaml", &version.meta_yaml),
    ] {
        if !eol_eq(read(name).as_bytes(), remote.as_bytes()) {
            changed.push(name.to_string());
        }
    }
    Ok((Some(latest), Some(changed)))
}

/// Self-host: full-tree changed-file list of `local_dir` vs the latest
/// `<slug>/v*` tag.
async fn changed_vs_tag(registry: &Path, slug: &str, local_dir: &Path) -> CliResult<Vec<String>> {
    use knack_types::Backend;
    let backend = knack_backend_github::GithubBackend::new(
        String::new(),
        String::new(),
        registry.to_path_buf(),
    );
    let package = backend
        .pull(slug, None)
        .await
        .map_err(|e| CliError::Internal(format!("read published version: {e}")))?;

    let mut tagged: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for f in package.files {
        tagged.insert(f.path.to_string_lossy().replace('\\', "/"), f.bytes);
    }
    let local = crate::skill_pack::collect_skill_entries(local_dir)?;
    let mut local_map: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (arc, p) in local {
        local_map.insert(arc, p);
    }

    let mut changed = Vec::new();
    for (arc, p) in &local_map {
        match tagged.get(arc) {
            Some(remote) => {
                let bytes = std::fs::read(p).unwrap_or_default();
                if !eol_eq(&bytes, remote) {
                    changed.push(arc.clone());
                }
            }
            None => changed.push(format!("{arc} (new)")),
        }
    }
    for arc in tagged.keys() {
        // .knack/manifest.json is pack metadata, not authored content.
        if !local_map.contains_key(arc) && !arc.starts_with(".knack/") {
            changed.push(format!("{arc} (deleted locally)"));
        }
    }
    Ok(changed)
}

// ─── shared helpers ────────────────────────────────────────────────────────

fn backend_tag(client: &ApiClient) -> &'static str {
    match &client.config.backend {
        BackendMode::Github { .. } => "github",
        _ => "cloud",
    }
}

/// Every existing on-disk copy of `slug`, in resolution-priority order.
fn copies_of(slug: &str, cwd: &Path, client: &ApiClient) -> Vec<Copy> {
    let ws = crate::workspace::discover_workspace_root(cwd);
    let mut candidates: Vec<(&'static str, PathBuf)> = Vec::new();
    if let Some(ws) = &ws {
        candidates.push(("workspace-draft", ws.join("drafts").join(slug)));
        candidates.push(("workspace-skill", ws.join("skills").join(slug)));
    }
    candidates.push(("home-pool", client.config.skills_dir.join(slug)));
    if let BackendMode::Github { local_path, .. } = &client.config.backend {
        candidates.push(("registry", local_path.join("skills").join(slug)));
    }

    // What publish/run would use: mirror of
    // workspace::resolve_existing_skill_dir — first existing candidate
    // among draft → skill → pool (the registry clone is publish's
    // TARGET, never its source).
    let resolved_path = crate::workspace::resolve_existing_skill_dir(
        slug,
        cwd,
        &client.config.skills_dir,
    );

    candidates
        .into_iter()
        .filter(|(_, p)| p.is_dir() && p.join("SKILL.md").is_file())
        .map(|(kind, p)| Copy {
            kind,
            version: meta_version(&p),
            resolved: resolved_path.as_deref() == Some(p.as_path()),
            path: display_path(&p),
        })
        .collect()
}

/// True when any two copies' content differs (EOL-insensitive, full
/// canonical file set).
fn copies_disagree(copies: &[Copy]) -> bool {
    let mut digests: Vec<BTreeMap<String, Vec<u8>>> = Vec::new();
    for c in copies {
        let dir = PathBuf::from(&c.path);
        let Ok(entries) = crate::skill_pack::collect_skill_entries(&dir) else {
            continue;
        };
        let mut m = BTreeMap::new();
        for (arc, p) in entries {
            m.insert(arc, normalize_eol(&std::fs::read(&p).unwrap_or_default()));
        }
        digests.push(m);
    }
    digests.windows(2).any(|w| w[0] != w[1])
}

fn meta_version(dir: &Path) -> Option<String> {
    let bytes = std::fs::read(dir.join("meta.knack.yaml")).ok()?;
    let parsed: serde_yaml::Value =
        serde_yaml::from_slice(knack_types::strip_utf8_bom(&bytes)).ok()?;
    parsed.get("version")?.as_str().map(str::to_string)
}

fn normalize_eol(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\r' && b.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn eol_eq(a: &[u8], b: &[u8]) -> bool {
    normalize_eol(a) == normalize_eol(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_eol_strips_crlf_only() {
        assert_eq!(normalize_eol(b"a\r\nb"), b"a\nb");
        assert_eq!(normalize_eol(b"a\rb"), b"a\rb"); // bare CR is content
        assert!(eol_eq(b"x\r\n", b"x\n"));
        assert!(!eol_eq(b"x\r\n", b"y\n"));
    }

    #[test]
    fn meta_version_reads_bom_tolerant_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("meta.knack.yaml"),
            "\u{feff}slug: s\nversion: 1.2.3\n",
        )
        .unwrap();
        assert_eq!(meta_version(dir.path()).as_deref(), Some("1.2.3"));
    }
}
