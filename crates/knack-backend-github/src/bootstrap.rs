//! Eager bootstrap for `knack init --self-host`.
//!
//! Creates the GitHub repo (if missing), clones it to disk, scaffolds the
//! skills/runs/README layout, makes the initial commit, pushes to origin.
//! Idempotent: if the local clone already exists at `local_path`, just
//! re-scaffolds missing pieces without overwriting.

use anyhow::{anyhow, Context, Result};
use git2::{IndexAddOption, Repository, Signature};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::auth::resolve_token;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
}

impl Visibility {
    fn gh_flag(self) -> &'static str {
        match self {
            Visibility::Public => "--public",
            Visibility::Private => "--private",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BootstrapOpts {
    pub owner: String,
    pub repo: String,
    pub visibility: Visibility,
    pub local_path: PathBuf,
    pub author_name: String,
    pub author_email: String,
}

#[derive(Debug)]
pub struct BootstrapResult {
    pub created_repo: bool,
    pub local_path: PathBuf,
    pub https_url: String,
}

/// Run the full bootstrap. Returns the repo URL on success.
pub fn bootstrap_repo(opts: &BootstrapOpts) -> Result<BootstrapResult> {
    let auth = resolve_token().context("github token")?;
    let https_url = format!("https://github.com/{}/{}.git", opts.owner, opts.repo);

    let exists = repo_exists(&opts.owner, &opts.repo)?;
    if exists && local_path_has_content(&opts.local_path) {
        return Err(anyhow!(
            "{}/{} already exists on GitHub and {} is non-empty. point --github-repo at a different name or delete the local path",
            opts.owner,
            opts.repo,
            opts.local_path.display(),
        ));
    }

    let created = if !exists {
        gh_repo_create(&opts.owner, &opts.repo, opts.visibility)?;
        true
    } else {
        false
    };

    let repo = clone_or_init(&opts.local_path, &https_url)?;

    scaffold(&opts.local_path, &opts.owner, &opts.repo)?;

    commit_initial(&repo, opts)?;

    // Network ops go through the system `git` so we inherit the user's
    // existing credential helper (gh sets it up automatically). libgit2's
    // bundled HTTPS support requires extra TLS features that are painful
    // to enable on Windows.
    push_main_via_git_cli(&opts.local_path)?;
    let _ = auth; // token is consumed by the credential helper, not us

    Ok(BootstrapResult {
        created_repo: created,
        local_path: opts.local_path.clone(),
        https_url: format!("https://github.com/{}/{}", opts.owner, opts.repo),
    })
}

fn repo_exists(owner: &str, repo: &str) -> Result<bool> {
    let output = Command::new("gh")
        .args(["repo", "view", &format!("{owner}/{repo}"), "--json", "name"])
        .output()
        .context("invoke gh repo view")?;
    Ok(output.status.success())
}

fn gh_repo_create(owner: &str, repo: &str, vis: Visibility) -> Result<()> {
    let output = Command::new("gh")
        .args([
            "repo",
            "create",
            &format!("{owner}/{repo}"),
            vis.gh_flag(),
            "--description",
            "My Knack skills.",
        ])
        .output()
        .context("invoke gh repo create")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("gh repo create failed: {}", stderr.trim()));
    }
    Ok(())
}

fn local_path_has_content(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// Clone an existing remote, or init locally and add origin if the remote
/// is brand new (no commits yet, no default branch on GitHub side).
///
/// We always set the origin URL to the plain HTTPS URL with NO embedded
/// credentials. Auth happens at push time via the libgit2 credentials
/// callback so the token never lands in `.git/config`.
fn clone_or_init(local_path: &Path, https_url: &str) -> Result<Repository> {
    if local_path.exists() && local_path.join(".git").exists() {
        return Repository::open(local_path).context("open existing local clone");
    }
    fs::create_dir_all(local_path).context("create local_path")?;

    // GitHub repos created via `gh repo create` (without --clone) start empty
    // with no default branch. We can't clone an empty repo; instead init
    // locally and set the remote.
    let repo = Repository::init_opts(
        local_path,
        git2::RepositoryInitOptions::new()
            .initial_head("main")
            .external_template(false),
    )
    .context("git init")?;

    let _ = repo.remote("origin", https_url);
    Ok(repo)
}

fn scaffold(local_path: &Path, owner: &str, repo: &str) -> Result<()> {
    fs::create_dir_all(local_path.join("skills"))?;
    fs::create_dir_all(local_path.join("runs"))?;

    write_if_missing(&local_path.join("skills").join(".gitkeep"), "")?;
    write_if_missing(&local_path.join("runs").join(".gitkeep"), "")?;
    write_if_missing(
        &local_path.join(".gitignore"),
        ".knack/local-state.json\n.DS_Store\n",
    )?;
    write_if_missing(
        &local_path.join("knack.yaml"),
        &format!(
            "# Knack self-host config. Edited by the Knack CLI.\nowner: {owner}\nrepo: {repo}\nformat: knack-skills/v1\n"
        ),
    )?;
    write_if_missing(
        &local_path.join("README.md"),
        &format!(
            "# {repo}\n\nThis is a Knack skills repository. It is managed by the [Knack CLI](https://knack.ai).\n\n## Layout\n\n- `skills/<slug>/` — authored skills\n- `runs/<yyyy-mm>/<yyyy-mm-dd>.jsonl` — run telemetry\n- `knack.yaml` — repo metadata\n\n## Add a skill\n\n```\nknack create my-first-skill\nknack publish my-first-skill\n```\n\nVersions are per-skill git tags: `my-first-skill/v0.1.0`.\n"
        ),
    )?;
    Ok(())
}

fn write_if_missing(path: &Path, content: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn commit_initial(repo: &Repository, opts: &BootstrapOpts) -> Result<()> {
    // Skip if the repo already has commits and the working tree is unchanged.
    let head = repo.head();
    if head.is_ok() && repo.statuses(None)?.is_empty() {
        return Ok(());
    }

    let mut index = repo.index().context("repo index")?;
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .context("git add")?;
    index.write().context("write index")?;
    let tree_oid = index.write_tree().context("write tree")?;
    let tree = repo.find_tree(tree_oid)?;

    let sig = Signature::now(&opts.author_name, &opts.author_email).context("build signature")?;

    let parent = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .and_then(|oid| repo.find_commit(oid).ok());
    let parents: Vec<&git2::Commit> = match &parent {
        Some(c) => vec![c],
        None => vec![],
    };

    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "Initial Knack skills repo (via `knack init --self-host`)",
        &tree,
        &parents,
    )
    .context("git commit")?;

    Ok(())
}

/// Push via the system `git` binary, not libgit2.
///
/// Why: libgit2's HTTPS support depends on the build-time TLS features,
/// which are painful to enable on Windows (needs schannel or vendored
/// OpenSSL with a working C toolchain). The system `git` is already a
/// pre-requisite for any contributor flow and is automatically credential-
/// helped by `gh auth login`, so we just shell out.
fn push_main_via_git_cli(local_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(local_path)
        .args(["push", "-u", "origin", "main"])
        .output()
        .context("invoke git push")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git push origin main failed: {}", stderr.trim()));
    }
    Ok(())
}
