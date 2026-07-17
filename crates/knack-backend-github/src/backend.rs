use async_trait::async_trait;
use chrono::{DateTime, Utc};
use git2::{IndexAddOption, Repository, Signature, StatusOptions};
use knack_types::{
    Backend, BackendError, BackendResult, PublishReceipt, RunLog, SkillFile, SkillManifest,
    SkillPackage, SkillSource, SkillSummary,
};
use serde::Deserialize;
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

    // The version number asserts content. Read the tagged tree back and
    // byte-compare it against what we were asked to publish; refuse to push
    // or report success on any mismatch. This is the invariant that would
    // have caught the "meta-only publish commit" bug the first time it
    // shipped a stale release.
    verify_tagged_content(&repository, &tag_name, &package, &skill_dir)?;

    push_via_git_cli(local_path)?;

    Ok(PublishReceipt {
        slug: package.slug.clone(),
        version: package.version.clone(),
        url: format!("https://github.com/{}/{}/tree/{}", owner, repo, tag_name),
    })
}

/// Post-commit, pre-push integrity check: every file the publish was asked
/// to ship must exist in the tagged tree with identical bytes. When the
/// package carried explicit files (the `--from` sync path) we check all of
/// them; for publish-in-place (empty `files`) we check the two required
/// files on disk. The main way this trips in practice is a registry-repo
/// `.gitignore` rule silently excluding skill files from the commit.
fn verify_tagged_content(
    repository: &Repository,
    tag_name: &str,
    package: &SkillPackage,
    skill_dir: &Path,
) -> BackendResult<()> {
    let obj = repository
        .revparse_single(&format!("refs/tags/{}", tag_name))
        .map_err(|e| BackendError::Other(format!("verify tag: {e}")))?;
    let commit = obj
        .peel_to_commit()
        .map_err(|e| BackendError::Other(format!("verify: peel tag: {e}")))?;
    let tree = commit
        .tree()
        .map_err(|e| BackendError::Other(format!("verify: commit tree: {e}")))?;
    let subdir_rel = format!("skills/{}", package.slug);
    let entry = tree.get_path(Path::new(&subdir_rel)).map_err(|_| {
        BackendError::Other(format!(
            "verify: tag {} does not contain {}",
            tag_name, subdir_rel
        ))
    })?;
    let subtree = repository
        .find_tree(entry.id())
        .map_err(|e| BackendError::Other(format!("verify: find subtree: {e}")))?;
    let mut tagged: Vec<SkillFile> = Vec::new();
    walk_tree(repository, &subtree, PathBuf::new(), &mut tagged)?;
    let tagged_map: std::collections::HashMap<&Path, &[u8]> = tagged
        .iter()
        .map(|f| (f.path.as_path(), f.bytes.as_slice()))
        .collect();

    let expected: Vec<(PathBuf, Vec<u8>)> = if package.files.is_empty() {
        // Publish-in-place: the on-disk dir is the source of truth; the
        // required files are the load-bearing minimum to assert.
        let mut out = Vec::new();
        for name in ["SKILL.md", "meta.knack.yaml"] {
            let p = skill_dir.join(name);
            if p.is_file() {
                let bytes = fs::read(&p)
                    .map_err(|e| BackendError::Other(format!("verify: read {name}: {e}")))?;
                out.push((PathBuf::from(name), bytes));
            }
        }
        out
    } else {
        package
            .files
            .iter()
            .map(|f| (f.path.clone(), f.bytes.clone()))
            .collect()
    };

    let mut bad: Vec<String> = Vec::new();
    for (path, bytes) in &expected {
        match tagged_map.get(path.as_path()) {
            Some(t) if *t == bytes.as_slice() => {}
            Some(_) => bad.push(format!("{} (content differs)", path.display())),
            None => bad.push(format!("{} (missing from tag)", path.display())),
        }
    }
    if !bad.is_empty() {
        return Err(BackendError::Invalid(format!(
            "tag {tag_name} does not match the content being published: {}. \
             nothing was pushed. check the registry repo's .gitignore for rules \
             that exclude skill files, then delete the local tag and re-publish",
            bad.join(", ")
        )));
    }
    Ok(())
}

fn has_unrelated_dirty(repo: &Repository, current_slug: &str) -> BackendResult<bool> {
    // `Repository::statuses(None)` defaults to including ignored entries,
    // which flags the `.knack/` workspace that `knack init --self-host`
    // plants (and that the planted `.knack/.gitignore` then ignores) as a
    // dirty path. Result: every freshly-init'd self-host user's first
    // publish aborts with GH_PUBLISH_FAILED. Explicitly opt out of
    // ignored entries; we only care about real working-tree changes.
    let mut opts = StatusOptions::new();
    opts.include_ignored(false)
        .include_untracked(true)
        .recurse_untracked_dirs(true);
    let statuses = repo
        .statuses(Some(&mut opts))
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
    let target = crate::git::resolve_remote(local_path);
    let output = Command::new("git")
        .arg("-C")
        .arg(local_path)
        .args(["push", "--follow-tags", &target.remote, &target.branch])
        .output()
        .map_err(|e| BackendError::Network(format!("invoke git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BackendError::Network(format!(
            "git push {} {}: {}",
            target.remote,
            target.branch,
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
            if s.slug.to_lowercase().contains(query_lc) {
                return true;
            }
            if s.description
                .as_deref()
                .map(|d| d.to_lowercase().contains(query_lc))
                .unwrap_or(false)
            {
                return true;
            }
            // Fall through to grepping SKILL.md. The README documents
            // this surface; without it `knack search "rule about X"`
            // misses skills whose slug and description don't mention
            // X but whose body does. Read errors are non-fatal — a
            // skill folder missing SKILL.md just doesn't match here.
            let skill_md_path = skills_root.join(&s.slug).join("SKILL.md");
            fs::read_to_string(&skill_md_path)
                .map(|body| body.to_lowercase().contains(query_lc))
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
    // Tolerate a UTF-8 BOM — PowerShell 5.1's `Set-Content -Encoding utf8`
    // writes one, and a single BOM'd meta used to break `knack list` and
    // `publish` for every skill in the registry.
    let meta: MetaYaml = serde_yaml::from_slice(knack_types::strip_utf8_bom(&bytes))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_skill(root: &Path, slug: &str, description: &str, skill_md_body: &str) {
        let dir = root.join(slug);
        fs::create_dir_all(&dir).unwrap();
        // Minimum-viable `meta.knack.yaml` — `read_summary` only requires
        // slug (others have sensible fallbacks).
        let meta = format!(
            "slug: {slug}\nname: {slug}\nauthor: t@example.com\ndescription: {description}\n"
        );
        fs::write(dir.join("meta.knack.yaml"), meta).unwrap();
        fs::write(dir.join("SKILL.md"), skill_md_body).unwrap();
    }

    #[test]
    fn search_matches_skill_md_body_when_slug_and_description_miss() {
        // The README advertises grep over slug + description + SKILL.md.
        // Before this fix the implementation only checked the first two,
        // so a unique phrase in the body was invisible to `knack search`.
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "alpha",
            "totally unrelated",
            "# Alpha\n\nA particular needle phrase lives here in the body.\n",
        );
        write_skill(
            dir.path(),
            "beta",
            "also unrelated",
            "# Beta\n\nNothing of note.\n",
        );
        let results = search_blocking(dir.path(), "needle").unwrap();
        let slugs: Vec<_> = results.iter().map(|s| s.slug.as_str()).collect();
        assert_eq!(slugs, vec!["alpha"], "SKILL.md body match should surface alpha");
    }

    #[test]
    fn list_tolerates_utf8_bom_in_meta() {
        // PowerShell 5.1's `Set-Content -Encoding utf8` writes a BOM. One
        // BOM'd meta anywhere in the registry used to break `knack list`
        // and `knack publish` for EVERY skill with the baffling
        // "missing field `slug` at line 1 column 2".
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("bommed");
        fs::create_dir_all(&skill_dir).unwrap();
        let mut bytes = b"\xef\xbb\xbf".to_vec();
        bytes.extend_from_slice(
            b"slug: bommed\nname: bommed\nauthor: t@example.com\ndescription: d\nversion: 1.2.3\n",
        );
        fs::write(skill_dir.join("meta.knack.yaml"), bytes).unwrap();
        let results = list_blocking(dir.path()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].slug, "bommed");
        assert_eq!(results[0].version, "1.2.3");
    }

    #[test]
    fn search_still_matches_slug() {
        // Don't regress the pre-existing slug match.
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "needle-skill", "x", "body");
        let results = search_blocking(dir.path(), "needle").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_still_matches_description() {
        // Don't regress the pre-existing description match.
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "x",
            "this needle is in the description",
            "body",
        );
        let results = search_blocking(dir.path(), "needle").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_tolerates_missing_skill_md() {
        // A skill folder with no SKILL.md just doesn't match on body —
        // not a fatal error.
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("orphan");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("meta.knack.yaml"),
            "slug: orphan\nname: orphan\nauthor: t@example.com\ndescription: anything\n",
        )
        .unwrap();
        let results = search_blocking(dir.path(), "needle").unwrap();
        assert!(results.is_empty());
    }

    fn init_repo_with_root_gitignore(root: &Path, ignore_body: &str) -> Repository {
        let repo = Repository::init(root).unwrap();
        {
            // libgit2 needs a user identity to even produce status. Set
            // local config so the test doesn't depend on the runner's
            // global git config.
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "test").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        fs::write(root.join(".gitignore"), ignore_body).unwrap();
        // Commit the .gitignore so it isn't itself a dirty entry that
        // pollutes the status check below. A real self-host repo always
        // has its .gitignore committed before the first publish.
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new(".gitignore")).unwrap();
            index.write().unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();
            let sig = Signature::now("test", "test@example.com").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        repo
    }

    /// Registry repo on `main` with a local bare repo as `origin`, so the
    /// full publish path (commit → tag → verify → push) runs offline.
    fn init_registry_with_bare_origin(root: &Path) -> (Repository, PathBuf) {
        let bare_path = root.join("origin.git");
        Repository::init_bare(&bare_path).unwrap();

        let registry = root.join("registry");
        let repo = Repository::init_opts(
            &registry,
            git2::RepositoryInitOptions::new()
                .initial_head("main")
                .external_template(false),
        )
        .unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "test").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        repo.remote("origin", bare_path.to_str().unwrap()).unwrap();
        (repo, registry)
    }

    fn commit_all(repo: &Repository, msg: &str) {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("test", "test@example.com").unwrap();
        let parent = repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .and_then(|oid| repo.find_commit(oid).ok());
        let parents: Vec<&git2::Commit> = parent.as_ref().into_iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
            .unwrap();
    }

    fn make_package(slug: &str, version: &str, files: Vec<(&str, &str)>) -> SkillPackage {
        SkillPackage {
            slug: slug.into(),
            version: version.into(),
            manifest: SkillManifest {
                slug: slug.into(),
                version: version.into(),
                description: None,
                entry: PathBuf::from("SKILL.md"),
                assets: Vec::new(),
                created_at: Utc::now(),
            },
            files: files
                .into_iter()
                .map(|(p, body)| SkillFile {
                    path: PathBuf::from(p),
                    bytes: body.as_bytes().to_vec(),
                })
                .collect(),
        }
    }

    #[test]
    fn publish_with_files_syncs_content_including_deletions() {
        // THE bug from the July 2026 field report: publish with explicit
        // package files used to commit only a meta version bump — the
        // actual content never reached the repo, so every tag shipped
        // whatever was already sitting there. Pin the fix end to end:
        // files land, stale files are deleted, the tag matches.
        let dir = tempdir().unwrap();
        let (repo, registry) = init_registry_with_bare_origin(dir.path());

        // Pre-existing (stale) skill content, committed.
        let stale_dir = registry.join("skills/alpha");
        fs::create_dir_all(&stale_dir).unwrap();
        fs::write(stale_dir.join("SKILL.md"), "# Old\n").unwrap();
        fs::write(stale_dir.join("stale-file.md"), "delete me\n").unwrap();
        fs::write(
            stale_dir.join("meta.knack.yaml"),
            "slug: alpha\nversion: 0.1.0\n",
        )
        .unwrap();
        commit_all(&repo, "init");

        let package = make_package(
            "alpha",
            "0.2.0",
            vec![
                ("SKILL.md", "# New body\n"),
                ("meta.knack.yaml", "slug: alpha\nversion: 0.2.0\n"),
                ("examples/README.md", "fresh example\n"),
            ],
        );
        let receipt = publish_blocking(&registry, "o", "r", package).unwrap();
        assert_eq!(receipt.version, "0.2.0");

        // The tag must contain exactly the new content.
        let pulled = pull_blocking(&registry, "alpha", Some("0.2.0")).unwrap();
        let by_path: std::collections::HashMap<String, &[u8]> = pulled
            .files
            .iter()
            .map(|f| (f.path.to_string_lossy().replace('\\', "/"), f.bytes.as_slice()))
            .collect();
        assert_eq!(by_path.get("SKILL.md").copied(), Some("# New body\n".as_bytes()));
        assert_eq!(
            by_path.get("examples/README.md").copied(),
            Some("fresh example\n".as_bytes())
        );
        assert!(
            !by_path.contains_key("stale-file.md"),
            "files absent from the package must be deleted from the published tree"
        );
    }

    #[test]
    fn publish_refuses_success_when_gitignore_swallows_content() {
        // The post-commit verify: if a registry .gitignore rule excludes a
        // skill file from the commit, the tag silently misses content —
        // exactly the class of loss the version number is supposed to rule
        // out. Publish must fail loudly and must not push.
        let dir = tempdir().unwrap();
        let (repo, registry) = init_registry_with_bare_origin(dir.path());
        fs::write(registry.join(".gitignore"), "*.secret\n").unwrap();
        commit_all(&repo, "init");

        let package = make_package(
            "beta",
            "0.1.0",
            vec![
                ("SKILL.md", "# Beta\n"),
                ("meta.knack.yaml", "slug: beta\nversion: 0.1.0\n"),
                ("notes.secret", "will be gitignored\n"),
            ],
        );
        let err = publish_blocking(&registry, "o", "r", package).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("does not match") && msg.contains("notes.secret"),
            "expected a content-mismatch error naming the missing file, got: {msg}"
        );
    }

    #[test]
    fn dirty_check_ignores_gitignored_knack_workspace() {
        // Layer 3 smoke surfaced this: `knack init --self-host` plants
        // a `.knack/` workspace dir + a `.knack/.gitignore`, and the
        // working-tree-dirty check was including IGNORED entries by
        // default. Result: every fresh self-host user's first publish
        // aborted with GH_PUBLISH_FAILED. Pin the fix.
        let dir = tempdir().unwrap();
        let root = dir.path();
        let repo = init_repo_with_root_gitignore(root, ".knack/\n");
        // Materialize the planted workspace dir + a stub file so it
        // shows up in `git status --ignored`.
        fs::create_dir_all(root.join(".knack/skills")).unwrap();
        fs::write(root.join(".knack/config.yaml"), "mode: github\n").unwrap();

        assert_eq!(
            has_unrelated_dirty(&repo, "any-slug").unwrap(),
            false,
            "ignored .knack/ workspace must not count as unrelated dirt"
        );
    }

    #[test]
    fn dirty_check_flags_real_unrelated_changes() {
        // Negative-space pair for the test above: an actual untracked
        // file outside skills/<slug>/ must still trip the check.
        let dir = tempdir().unwrap();
        let root = dir.path();
        let repo = init_repo_with_root_gitignore(root, "");
        fs::write(root.join("stray.txt"), "untracked\n").unwrap();
        assert_eq!(has_unrelated_dirty(&repo, "any-slug").unwrap(), true);
    }

    #[test]
    fn dirty_check_allows_changes_inside_current_skill_prefix() {
        // Don't regress the original intent: in-skill churn is the
        // publish's own work and must be allowed.
        let dir = tempdir().unwrap();
        let root = dir.path();
        let repo = init_repo_with_root_gitignore(root, "");
        let skill_dir = root.join("skills/alpha");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "# Alpha\n").unwrap();
        // Recurse untracked dirs so leaf paths are reported instead of
        // the collapsed `skills/` parent — matches what we need to do
        // in production too. We also rebuild the StatusOptions here to
        // sanity-check the production fix when the only dirt is leaf
        // files under the current skill's prefix.
        let mut opts = StatusOptions::new();
        opts.include_ignored(false)
            .include_untracked(true)
            .recurse_untracked_dirs(true);
        let statuses = repo.statuses(Some(&mut opts)).unwrap();
        let paths: Vec<_> = statuses
            .iter()
            .filter_map(|e| e.path().map(str::to_string))
            .collect();
        assert!(
            paths.iter().all(|p| p.starts_with("skills/alpha")),
            "all dirt should be inside skills/alpha, got: {paths:?}"
        );
        assert_eq!(has_unrelated_dirty(&repo, "alpha").unwrap(), false);
    }
}
