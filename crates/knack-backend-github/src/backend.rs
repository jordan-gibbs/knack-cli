use async_trait::async_trait;
use chrono::{DateTime, Utc};
use git2::{IndexAddOption, Repository, Signature};
use knack_types::{
    Backend, BackendError, BackendResult, PublishReceipt, RunLog, SkillFile, SkillManifest,
    SkillPackage, SkillSource, SkillSummary,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runs::append_run;

/// GitHub-backed Backend. Reads and writes a user-owned repo on disk that
/// mirrors a remote GitHub repository.
#[derive(Debug, Clone)]
pub struct GithubBackend {
    pub owner: String,
    pub repo: String,
    pub local_path: PathBuf,
}

impl GithubBackend {
    pub fn new(owner: impl Into<String>, repo: impl Into<String>, local_path: PathBuf) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            local_path,
        }
    }

    fn skills_root(&self) -> PathBuf {
        self.local_path.join("skills")
    }
}

#[derive(Debug, Deserialize)]
struct MetaYaml {
    slug: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

#[async_trait]
impl Backend for GithubBackend {
    async fn pull(&self, slug: &str, version: Option<&str>) -> BackendResult<SkillPackage> {
        let local_path = self.local_path.clone();
        let slug = slug.to_string();
        let version = version.map(|s| s.to_string());

        tokio::task::spawn_blocking(move || pull_blocking(&local_path, &slug, version.as_deref()))
            .await
            .map_err(|e| BackendError::Other(format!("join: {e}")))?
    }

    async fn publish(&self, package: SkillPackage) -> BackendResult<PublishReceipt> {
        let local_path = self.local_path.clone();
        let owner = self.owner.clone();
        let repo = self.repo.clone();
        tokio::task::spawn_blocking(move || publish_blocking(&local_path, &owner, &repo, package))
            .await
            .map_err(|e| BackendError::Other(format!("join: {e}")))?
    }

    async fn list(&self) -> BackendResult<Vec<SkillSummary>> {
        let root = self.skills_root();
        tokio::task::spawn_blocking(move || list_blocking(&root))
            .await
            .map_err(|e| BackendError::Other(format!("join: {e}")))?
    }

    async fn search(&self, query: &str) -> BackendResult<Vec<SkillSummary>> {
        let root = self.skills_root();
        let q = query.to_lowercase();
        tokio::task::spawn_blocking(move || search_blocking(&root, &q))
            .await
            .map_err(|e| BackendError::Other(format!("join: {e}")))?
    }

    async fn record_run(&self, log: RunLog) -> BackendResult<()> {
        append_run(&self.local_path, &log)
            .map_err(|e| BackendError::Other(format!("write run log: {e}")))
    }
}

// === publish ===

fn publish_blocking(
    local_path: &Path,
    owner: &str,
    repo: &str,
    package: SkillPackage,
) -> BackendResult<PublishReceipt> {
    if !local_path.join(".git").exists() {
        return Err(BackendError::Other(format!(
            "no local clone at {} — run `knack init --self-host` first",
            local_path.display()
        )));
    }
    let repository =
        Repository::open(local_path).map_err(|e| BackendError::Other(format!("open repo: {e}")))?;

    // Refuse if working tree is dirty in unrelated areas — staging the
    // user's incidental edits with a publish commit is a footgun.
    if has_unrelated_dirty(&repository, &package.slug)? {
        return Err(BackendError::Invalid(format!(
            "working tree has uncommitted changes outside skills/{}. commit or stash them first",
            package.slug
        )));
    }

    // Materialize files from the package into <local>/skills/<slug>/. If
    // package.files is empty, assume the user already has them on disk and
    // we're publishing what's there.
    let skill_dir = local_path.join("skills").join(&package.slug);
    if !package.files.is_empty() {
        if skill_dir.exists() {
            // Wipe and rewrite to ensure we publish exactly the package
            // contents (not stale files from a previous version).
            fs::remove_dir_all(&skill_dir)
                .map_err(|e| BackendError::Other(format!("clear skill dir: {e}")))?;
        }
        fs::create_dir_all(&skill_dir)
            .map_err(|e| BackendError::Other(format!("create skill dir: {e}")))?;
        for file in &package.files {
            let dest = skill_dir.join(&file.path);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| BackendError::Other(format!("mkdir: {e}")))?;
            }
            fs::write(&dest, &file.bytes)
                .map_err(|e| BackendError::Other(format!("write {}: {e}", dest.display())))?;
        }
    } else if !skill_dir.exists() {
        return Err(BackendError::NotFound(format!(
            "no skill at {} and no files in the package",
            skill_dir.display()
        )));
    }

    // git add skills/<slug>
    let mut index = repository
        .index()
        .map_err(|e| BackendError::Other(format!("index: {e}")))?;
    let rel = PathBuf::from("skills").join(&package.slug);
    index
        .add_all([rel.as_path()].iter(), IndexAddOption::DEFAULT, None)
        .map_err(|e| BackendError::Other(format!("git add: {e}")))?;
    index
        .write()
        .map_err(|e| BackendError::Other(format!("write index: {e}")))?;

    let tree_oid = index
        .write_tree()
        .map_err(|e| BackendError::Other(format!("write tree: {e}")))?;
    let tree = repository
        .find_tree(tree_oid)
        .map_err(|e| BackendError::Other(format!("find tree: {e}")))?;

    let sig = git_signature(&repository, owner)?;

    let parent_commit = repository
        .head()
        .ok()
        .and_then(|h| h.target())
        .and_then(|oid| repository.find_commit(oid).ok());
    let parents: Vec<&git2::Commit> = parent_commit.as_ref().into_iter().collect();

    let commit_msg = format!("publish {}@v{}", package.slug, package.version);
    let commit_oid = repository
        .commit(Some("HEAD"), &sig, &sig, &commit_msg, &tree, &parents)
        .map_err(|e| BackendError::Other(format!("git commit: {e}")))?;

    let commit_obj = repository
        .find_object(commit_oid, None)
        .map_err(|e| BackendError::Other(format!("find commit: {e}")))?;

    let tag_name = format!("{}/v{}", package.slug, package.version);
    repository
        .tag(&tag_name, &commit_obj, &sig, &commit_msg, false)
        .map_err(|e| {
            // Tag already exists -> Conflict, anything else -> Other
            if e.code() == git2::ErrorCode::Exists {
                BackendError::Conflict(format!("tag {} already exists", tag_name))
            } else {
                BackendError::Other(format!("git tag: {e}"))
            }
        })?;

    push_via_git_cli(local_path)?;

    Ok(PublishReceipt {
        slug: package.slug.clone(),
        version: package.version.clone(),
        url: format!("https://github.com/{}/{}/tree/{}", owner, repo, tag_name),
    })
}

fn has_unrelated_dirty(repo: &Repository, current_slug: &str) -> BackendResult<bool> {
    let statuses = repo
        .statuses(None)
        .map_err(|e| BackendError::Other(format!("git status: {e}")))?;
    let prefix = format!("skills/{}", current_slug);
    for entry in statuses.iter() {
        if let Some(path) = entry.path() {
            // Treat anything under our own skills/<slug>/ as "this publish's
            // expected churn"; treat anything else as dirty.
            if !path.starts_with(&prefix) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn git_signature(repo: &Repository, fallback_owner: &str) -> BackendResult<Signature<'static>> {
    let cfg = repo
        .config()
        .map_err(|e| BackendError::Other(format!("git config: {e}")))?;
    let name = cfg
        .get_string("user.name")
        .unwrap_or_else(|_| fallback_owner.to_string());
    let email = cfg
        .get_string("user.email")
        .unwrap_or_else(|_| format!("{}@users.noreply.github.com", fallback_owner));
    Signature::now(&name, &email).map_err(|e| BackendError::Other(format!("signature: {e}")))
}

fn push_via_git_cli(local_path: &Path) -> BackendResult<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(local_path)
        .args(["push", "--follow-tags", "origin", "main"])
        .output()
        .map_err(|e| BackendError::Network(format!("invoke git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BackendError::Network(format!(
            "git push: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

// === pull ===

fn pull_blocking(
    local_path: &Path,
    slug: &str,
    version: Option<&str>,
) -> BackendResult<SkillPackage> {
    if !local_path.join(".git").exists() {
        return Err(BackendError::Other(format!(
            "no local clone at {}",
            local_path.display()
        )));
    }
    let repo =
        Repository::open(local_path).map_err(|e| BackendError::Other(format!("open repo: {e}")))?;

    let tag_name = match version {
        Some(v) => format!("{}/v{}", slug, v),
        None => find_latest_tag(&repo, slug)?
            .ok_or_else(|| BackendError::NotFound(format!("no versions of {}", slug)))?,
    };

    let tag_obj = repo
        .revparse_single(&format!("refs/tags/{}", tag_name))
        .map_err(|_| BackendError::NotFound(format!("tag {} not found", tag_name)))?;

    let commit = tag_obj
        .peel_to_commit()
        .map_err(|e| BackendError::Other(format!("peel to commit: {e}")))?;
    let tree = commit
        .tree()
        .map_err(|e| BackendError::Other(format!("commit tree: {e}")))?;

    let subdir_rel = format!("skills/{}", slug);
    let entry = tree
        .get_path(Path::new(&subdir_rel))
        .map_err(|_| BackendError::NotFound(format!("{} not in tag {}", subdir_rel, tag_name)))?;
    let subtree = repo
        .find_tree(entry.id())
        .map_err(|e| BackendError::Other(format!("find subtree: {e}")))?;

    let mut files: Vec<SkillFile> = Vec::new();
    walk_tree(&repo, &subtree, PathBuf::new(), &mut files)?;

    let version_str = version.map(|s| s.to_string()).unwrap_or_else(|| {
        tag_name
            .split('/')
            .last()
            .unwrap_or("")
            .trim_start_matches('v')
            .to_string()
    });

    let manifest = SkillManifest {
        slug: slug.to_string(),
        version: version_str.clone(),
        description: None,
        entry: PathBuf::from("SKILL.md"),
        assets: files.iter().map(|f| f.path.clone()).collect(),
        created_at: commit_time(&commit),
    };

    Ok(SkillPackage {
        slug: slug.to_string(),
        version: version_str,
        manifest,
        files,
    })
}

fn walk_tree(
    repo: &Repository,
    tree: &git2::Tree,
    rel: PathBuf,
    out: &mut Vec<SkillFile>,
) -> BackendResult<()> {
    for entry in tree.iter() {
        let name = entry.name().unwrap_or("").to_string();
        let entry_rel = rel.join(&name);
        match entry.kind() {
            Some(git2::ObjectType::Blob) => {
                let blob = repo
                    .find_blob(entry.id())
                    .map_err(|e| BackendError::Other(format!("find blob: {e}")))?;
                out.push(SkillFile {
                    path: entry_rel,
                    bytes: blob.content().to_vec(),
                });
            }
            Some(git2::ObjectType::Tree) => {
                let child = repo
                    .find_tree(entry.id())
                    .map_err(|e| BackendError::Other(format!("find subtree: {e}")))?;
                walk_tree(repo, &child, entry_rel, out)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn find_latest_tag(repo: &Repository, slug: &str) -> BackendResult<Option<String>> {
    let prefix = format!("{}/v", slug);
    let tag_names = repo
        .tag_names(Some(&format!("{}*", prefix)))
        .map_err(|e| BackendError::Other(format!("tag list: {e}")))?;

    let mut best: Option<(semver_tuple::SemVerTuple, String)> = None;
    for name in tag_names.iter().flatten() {
        if let Some(rest) = name.strip_prefix(&prefix) {
            if let Some(parsed) = semver_tuple::parse(rest) {
                if best.as_ref().is_none_or(|(b, _)| parsed > *b) {
                    best = Some((parsed, name.to_string()));
                }
            }
        }
    }
    Ok(best.map(|(_, name)| name))
}

fn commit_time(commit: &git2::Commit) -> DateTime<Utc> {
    let seconds = commit.time().seconds();
    DateTime::<Utc>::from_timestamp(seconds, 0).unwrap_or_else(Utc::now)
}

// === list / search ===

fn list_blocking(skills_root: &Path) -> BackendResult<Vec<SkillSummary>> {
    if !skills_root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let entries = fs::read_dir(skills_root)
        .map_err(|e| BackendError::Other(format!("read skills dir: {e}")))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(summary) = read_summary(&path)? {
            out.push(summary);
        }
    }
    Ok(out)
}

fn search_blocking(skills_root: &Path, query_lc: &str) -> BackendResult<Vec<SkillSummary>> {
    let all = list_blocking(skills_root)?;
    let filtered: Vec<SkillSummary> = all
        .into_iter()
        .filter(|s| {
            s.slug.to_lowercase().contains(query_lc)
                || s.description
                    .as_deref()
                    .map(|d| d.to_lowercase().contains(query_lc))
                    .unwrap_or(false)
        })
        .collect();
    Ok(filtered)
}

fn read_summary(skill_dir: &Path) -> BackendResult<Option<SkillSummary>> {
    let meta_path = skill_dir.join("meta.knack.yaml");
    if !meta_path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&meta_path).map_err(|e| BackendError::Other(format!("read meta: {e}")))?;
    let meta: MetaYaml = serde_yaml::from_slice(&bytes)
        .map_err(|e| BackendError::Other(format!("parse meta.knack.yaml: {e}")))?;

    let updated_at = fs::metadata(&meta_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| DateTime::<Utc>::from_timestamp(d.as_secs() as i64, 0).unwrap_or_else(Utc::now))
        .unwrap_or_else(Utc::now);

    Ok(Some(SkillSummary {
        slug: meta.slug,
        author: meta.author.unwrap_or_else(|| "unknown".to_string()),
        version: meta.version.unwrap_or_else(|| "0.0.0".to_string()),
        description: meta.description,
        updated_at,
        source: SkillSource::Github,
    }))
}

// === semver tuple ===
mod semver_tuple {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub struct SemVerTuple(pub u32, pub u32, pub u32);

    pub fn parse(s: &str) -> Option<SemVerTuple> {
        let mut parts = s.split('.');
        let major: u32 = parts.next()?.parse().ok()?;
        let minor: u32 = parts.next()?.parse().ok()?;
        let patch: u32 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(SemVerTuple(major, minor, patch))
    }
}

// Keep the unused-import warning at bay; we don't `use BTreeMap` anywhere
// but earlier drafts did. Strip if it lingers.
#[allow(dead_code)]
fn _retain_btreemap_import(_: BTreeMap<String, ()>) {}
