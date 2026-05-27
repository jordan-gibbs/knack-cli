//! External skill pulls via the GitHub Contents API.
//!
//! Lets self-host users pull skills from any public GitHub knack-skills repo
//! without first git-cloning it. Three accepted spec shapes:
//!
//!   `@owner/slug`          -> github.com/<owner>/knack-skills, path `skills/<slug>`
//!   `@owner/repo:slug`     -> github.com/<owner>/<repo>, path `skills/<slug>`
//!   `@owner/repo:slug@ver` -> same, at tag `<slug>/v<ver>` (else default branch)
//!
//! Uses the user's gh-resolved token via [`crate::auth::resolve_token`] so
//! private repos work too if the user has access. The repo's default branch
//! is resolved at lookup time so we don't hard-code `main`.

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use knack_types::{SkillFile, SkillManifest, SkillPackage};
use serde::Deserialize;
use std::path::PathBuf;

use crate::auth::resolve_token;

const USER_AGENT: &str = concat!("knack-cli/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone)]
pub struct ExternalSpec {
    pub owner: String,
    pub repo: String,
    pub slug: String,
    pub version: Option<String>,
}

/// Parse `@owner/slug`, `@owner/repo:slug`, optionally `@...@<ver>`.
pub fn parse_spec(s: &str) -> Result<ExternalSpec> {
    let stripped = s
        .strip_prefix('@')
        .ok_or_else(|| anyhow!("external spec must start with '@', got '{s}'"))?;

    // Split off the optional `@<ver>` suffix once, from the right, so paths
    // and slugs containing '@' don't trip us up (they shouldn't, but defensive).
    let (head, version) = match stripped.rsplit_once("@v") {
        Some((h, v)) => (h.to_string(), Some(v.to_string())),
        None => match stripped.rsplit_once('@') {
            Some((h, v)) => (h.to_string(), Some(v.to_string())),
            None => (stripped.to_string(), None),
        },
    };

    // Now head is `owner/slug` or `owner/repo:slug`.
    let (owner_repo, slug) = match head.split_once(':') {
        Some((or, sl)) => (or.to_string(), sl.to_string()),
        None => {
            let (owner, slug) = head
                .split_once('/')
                .ok_or_else(|| anyhow!("expected `@owner/slug`, got '{s}'"))?;
            return Ok(ExternalSpec {
                owner: owner.to_string(),
                repo: "knack-skills".to_string(),
                slug: slug.to_string(),
                version,
            });
        }
    };
    let (owner, repo) = owner_repo
        .split_once('/')
        .ok_or_else(|| anyhow!("expected `@owner/repo:slug`, got '{s}'"))?;
    Ok(ExternalSpec {
        owner: owner.to_string(),
        repo: repo.to_string(),
        slug,
        version,
    })
}

/// Fetch a skill subtree from an external GitHub repo and return a
/// `SkillPackage` ready to be materialized to disk.
pub async fn pull_external(spec: &ExternalSpec) -> Result<SkillPackage> {
    let auth = resolve_token().context("resolve github token")?;
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("build reqwest client")?;

    let r#ref = match &spec.version {
        Some(v) => format!("{}/v{}", spec.slug, v),
        None => resolve_default_branch(&client, &auth.token, &spec.owner, &spec.repo).await?,
    };

    let path = format!("skills/{}", spec.slug);
    let mut files: Vec<SkillFile> = Vec::new();
    walk_contents(
        &client,
        &auth.token,
        spec,
        &path,
        &r#ref,
        &PathBuf::new(),
        &mut files,
    )
    .await?;

    if files.is_empty() {
        return Err(anyhow!(
            "no files found at {}/{} {} ({})",
            spec.owner,
            spec.repo,
            path,
            r#ref
        ));
    }

    // Resolve the displayed version. Explicit `@vX.Y.Z` wins. Otherwise
    // parse the fetched meta.knack.yaml; falls back to "0.0.0" only if the
    // file is missing or unparseable.
    let version = spec
        .version
        .clone()
        .or_else(|| read_version_from_meta(&files))
        .unwrap_or_else(|| "0.0.0".to_string());
    Ok(SkillPackage {
        slug: spec.slug.clone(),
        version: version.clone(),
        manifest: SkillManifest {
            slug: spec.slug.clone(),
            version,
            description: None,
            entry: PathBuf::from("SKILL.md"),
            assets: files.iter().map(|f| f.path.clone()).collect(),
            created_at: Utc::now(),
        },
        files,
    })
}

fn read_version_from_meta(files: &[SkillFile]) -> Option<String> {
    let meta = files
        .iter()
        .find(|f| f.path == std::path::Path::new("meta.knack.yaml"))?;
    let text = std::str::from_utf8(&meta.bytes).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(text).ok()?;
    value
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[derive(Debug, Deserialize)]
struct RepoInfo {
    default_branch: String,
}

async fn resolve_default_branch(
    client: &reqwest::Client,
    token: &str,
    owner: &str,
    repo: &str,
) -> Result<String> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}");
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("GET repo info")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "github repos/{}/{} returned {}: {}",
            owner,
            repo,
            status,
            text.trim()
        ));
    }
    let info: RepoInfo = resp.json().await.context("parse repo info")?;
    Ok(info.default_branch)
}

#[derive(Debug, Deserialize)]
struct ContentsEntry {
    name: String,
    path: String,
    #[serde(rename = "type")]
    kind: String, // "file" | "dir" | "symlink" | "submodule"
    download_url: Option<String>,
}

/// Walk a directory recursively via the Contents API. For each file, fetch
/// the bytes via `download_url`. For each subdirectory, recurse.
async fn walk_contents(
    client: &reqwest::Client,
    token: &str,
    spec: &ExternalSpec,
    api_path: &str,
    r#ref: &str,
    rel_so_far: &std::path::Path,
    out: &mut Vec<SkillFile>,
) -> Result<()> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
        spec.owner, spec.repo, api_path, r#ref,
    );
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("GET contents")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "github contents {} returned {}: {}",
            api_path,
            status,
            text.trim()
        ));
    }

    // The API returns an array for a directory, an object for a single file.
    // We requested a directory, but defensive parsing handles both.
    let body: serde_json::Value = resp.json().await.context("parse contents")?;
    let entries: Vec<ContentsEntry> = match body {
        serde_json::Value::Array(_) => serde_json::from_value(body)?,
        v => vec![serde_json::from_value(v)?],
    };

    for entry in entries {
        let entry_rel = rel_so_far.join(&entry.name);
        match entry.kind.as_str() {
            "file" => {
                let Some(dl) = entry.download_url else {
                    continue;
                };
                let bytes = client
                    .get(&dl)
                    .bearer_auth(token)
                    .send()
                    .await
                    .with_context(|| format!("GET {dl}"))?
                    .bytes()
                    .await
                    .with_context(|| format!("read {}", entry.path))?;
                out.push(SkillFile {
                    path: entry_rel,
                    bytes: bytes.to_vec(),
                });
            }
            "dir" => {
                Box::pin(walk_contents(
                    client,
                    token,
                    spec,
                    &entry.path,
                    r#ref,
                    &entry_rel,
                    out,
                ))
                .await?;
            }
            _ => {
                // Skip symlinks and submodules.
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default_repo() {
        let s = parse_spec("@jordan-gibbs/email-triage").unwrap();
        assert_eq!(s.owner, "jordan-gibbs");
        assert_eq!(s.repo, "knack-skills");
        assert_eq!(s.slug, "email-triage");
        assert!(s.version.is_none());
    }

    #[test]
    fn parse_custom_repo() {
        let s = parse_spec("@jordan-gibbs/my-skills:email-triage").unwrap();
        assert_eq!(s.owner, "jordan-gibbs");
        assert_eq!(s.repo, "my-skills");
        assert_eq!(s.slug, "email-triage");
    }

    #[test]
    fn parse_with_version() {
        let s = parse_spec("@jordan-gibbs/email-triage@v0.2.1").unwrap();
        assert_eq!(s.repo, "knack-skills");
        assert_eq!(s.slug, "email-triage");
        assert_eq!(s.version.as_deref(), Some("0.2.1"));
    }

    #[test]
    fn rejects_no_at_prefix() {
        assert!(parse_spec("jordan-gibbs/email-triage").is_err());
    }
}
