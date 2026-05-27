//! `knack create <slug> --name "..."` — bootstrap a new skill shell.
//!
//! Hits `POST /skills` to register a new slug + name + scope. Returns the
//! generated skill id.
//!
//! With `--scaffold <dir>` (the default if the flag is omitted but `--name` is
//! present? No — explicit opt-in to avoid surprising existing scripts), also
//! writes a complete starter folder: SKILL.md with frontmatter, meta.knack.yaml
//! with the four required MetaKnack fields (id, name, slug, author), an
//! intuition.md stub, and an empty examples/ dir. The agent can immediately
//! `knack validate <dir>` + `knack publish <slug> --from <dir>` without
//! having to know the meta schema.

use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::json;

use crate::api::{auth as api_auth, skills as api_skills, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Slug for the new skill. Lowercase, hyphens, no leading hyphen
    /// (matches `^[a-z0-9][a-z0-9-]*$`).
    pub slug: String,

    /// Display name (1-200 chars).
    #[arg(long)]
    pub name: String,

    /// One-line description (1-280 chars). Optional; defaults to a stub.
    #[arg(long)]
    pub description: Option<String>,

    /// Visibility scope. Defaults to `personal`. `team` requires --team-id.
    #[arg(long, default_value = "personal")]
    pub scope: String,

    /// Team UUID (required when --scope team, forbidden otherwise).
    #[arg(long)]
    pub team_id: Option<String>,

    /// Override the scaffold target. Default: nearest workspace's
    /// ``.knack/drafts/<slug>/``, falling back to ``./.knack/drafts/<slug>/``
    /// when no workspace exists in the ancestor chain.
    #[arg(long)]
    pub scaffold: Option<PathBuf>,

    /// Skip the local scaffold and only register the slug API-side. The
    /// inverse of the old opt-in behavior — by default we always write
    /// a starter folder because the four required ``meta.knack.yaml``
    /// fields (id, name, slug, author) are easier to fill from a
    /// template than from memory.
    #[arg(long)]
    pub no_scaffold: bool,

    /// Scaffold into ``~/.knack/drafts/<slug>/`` (HOME-shared) instead
    /// of the workspace-local default. Pairs with ``knack pull --global``
    /// for users who prefer one global pool over per-project layouts.
    #[arg(long)]
    pub global: bool,
}

pub async fn run(args: CreateArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    validate_slug(&args.slug)?;
    validate_scope(&args.scope, args.team_id.as_deref())?;

    let body = api_skills::SkillCreate {
        slug: args.slug.clone(),
        name: args.name.clone(),
        scope: Some(args.scope.clone()),
        owner_team_id: args.team_id.clone(),
    };

    let skill = match api_skills::create(&client, &body).await {
        Ok(s) => s,
        Err(e) => {
            emit_err(mode, &e);
            return Err(e);
        }
    };

    let mut scaffolded_path: Option<PathBuf> = None;
    if !args.no_scaffold {
        // Default target: <workspace>/.knack/drafts/<slug>/. `--scaffold`
        // overrides; `--global` flips to ~/.knack/drafts/<slug>/. The
        // workspace gets created lazily on first write so users don't
        // have to remember `knack init` first.
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let drafts_root = crate::workspace::resolve_drafts_root(
            &cwd,
            args.global,
            args.scaffold.as_deref(),
            &client.config.skills_dir,
        );
        // If --scaffold gave a literal target we treat it as the FINAL
        // dir; otherwise we append <slug> so multiple drafts can
        // coexist under one drafts/ root.
        let dir = if args.scaffold.is_some() {
            drafts_root
        } else {
            drafts_root.join(&args.slug)
        };

        // Fetch the caller's email for the `author` field. We only block
        // on this when scaffolding so the bare-create path stays one
        // round-trip.
        let me = match api_auth::me(&client).await {
            Ok(m) => m,
            Err(e) => {
                emit_err(mode, &e);
                return Err(e);
            }
        };
        let desc = args
            .description
            .clone()
            .unwrap_or_else(|| format!("{} — describe what it does in one line.", args.name));
        if let Err(e) = write_scaffold(&dir, &skill.id, &args.slug, &args.name, &desc, &me.email) {
            emit_err(mode, &e);
            return Err(e);
        }
        scaffolded_path = Some(dir);
    }

    emit_ok(
        mode,
        json!({
            "slug": skill.slug,
            "skill_id": skill.id,
            "scope": skill.scope,
            "name": skill.name,
            "scaffold": scaffolded_path.as_ref().map(|p| p.display().to_string()),
        }),
        || {
            println!("✓ created {} (id: {})", skill.slug, skill.id);
            match &scaffolded_path {
                Some(p) => {
                    println!("  scaffolded → {}", p.display());
                    println!(
                        "next: edit SKILL.md (the body of your skill), then \
                         `knack validate {}` + `knack publish {} --from {}`",
                        p.display(),
                        skill.slug,
                        p.display(),
                    );
                }
                None => println!(
                    "next: write SKILL.md (rules go in its ## Intuition section), \
                     then `knack publish {} --from <dir>`. \
                     Tip: pass --scaffold ./<dir> next time to skip the boilerplate.",
                    skill.slug,
                ),
            }
        },
    );
    Ok(())
}

/// Write a complete starter folder. Files mirror the canonical Knack skill
/// layout so `knack validate` + `knack publish --from` work immediately.
fn write_scaffold(
    dir: &Path,
    skill_id: &str,
    slug: &str,
    name: &str,
    description: &str,
    author_email: &str,
) -> Result<(), CliError> {
    if dir.exists() && dir.read_dir().map(|mut i| i.next().is_some()).unwrap_or(false) {
        return Err(CliError::User {
            code: "CREATE_SCAFFOLD_DIR_NOT_EMPTY".into(),
            message: format!("scaffold target {} is not empty", dir.display()),
            hint: Some("pass a fresh path or remove the existing contents first".into()),
        });
    }
    std::fs::create_dir_all(dir).map_err(io)?;
    std::fs::create_dir_all(dir.join("examples")).map_err(io)?;

    let skill_md = render_skill_md(name, description);
    let meta_yaml = render_meta_yaml(skill_id, name, slug, author_email);
    let intuition_md = render_intuition_md();
    let examples_readme = render_examples_readme();

    std::fs::write(dir.join("SKILL.md"), skill_md).map_err(io)?;
    std::fs::write(dir.join("meta.knack.yaml"), meta_yaml).map_err(io)?;
    std::fs::write(dir.join("intuition.md"), intuition_md).map_err(io)?;
    std::fs::write(dir.join("examples").join("README.md"), examples_readme).map_err(io)?;
    Ok(())
}

fn render_skill_md(name: &str, description: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: {description}\n---\n\n\
         # How to do it\n\n\
         Replace this with the step-by-step procedure. Keep it concrete.\n\n\
         # Intuition\n\n\
         Replace this with the judgment calls and edge cases that make this skill \
         non-obvious. See intuition.md for the long-form scenario list.\n\n\
         # Definition of done\n\n\
         Replace this with the criteria that say the work is finished.\n"
    )
}

fn render_meta_yaml(skill_id: &str, name: &str, slug: &str, author: &str) -> String {
    // Hand-written — meta.knack.yaml is short (~4 lines) so a yaml library
    // would be overkill here. Quote the name to be safe against colons.
    format!(
        "id: {skill_id}\n\
         name: \"{name}\"\n\
         slug: {slug}\n\
         author: {author}\n"
    )
}

fn render_intuition_md() -> String {
    "## Scenarios\n\n\
     - Replace each bullet with a realistic case the skill needs to handle.\n\
     - The more specific and unusual, the better — these become the test cases.\n"
        .to_string()
}

fn render_examples_readme() -> String {
    "# Examples\n\n\
     Drop input/output pairs here. They get bundled with the skill and the \
     conductor uses them as few-shot anchors.\n"
        .to_string()
}

fn io(e: std::io::Error) -> CliError {
    CliError::User {
        code: "CREATE_SCAFFOLD_IO".into(),
        message: format!("scaffold write failed: {e}"),
        hint: None,
    }
}

fn validate_slug(slug: &str) -> CliResult<()> {
    if slug.is_empty() || slug.len() > 100 {
        return Err(CliError::User {
            code: "CREATE_BAD_SLUG".into(),
            message: format!("slug `{slug}` must be 1-100 chars"),
            hint: None,
        });
    }
    let mut chars = slug.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(CliError::User {
            code: "CREATE_BAD_SLUG".into(),
            message: format!("slug must start with [a-z0-9], got `{slug}`"),
            hint: Some("use lowercase letters, numbers, and hyphens".into()),
        });
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(CliError::User {
                code: "CREATE_BAD_SLUG".into(),
                message: format!("slug contains invalid character `{c}` in `{slug}`"),
                hint: Some("only [a-z0-9-] allowed after the first character".into()),
            });
        }
    }
    Ok(())
}

fn validate_scope(scope: &str, team_id: Option<&str>) -> CliResult<()> {
    match scope {
        "personal" | "public" => {
            if team_id.is_some() {
                return Err(CliError::User {
                    code: "CREATE_BAD_SCOPE".into(),
                    message: format!("--team-id is forbidden when scope=`{scope}`"),
                    hint: None,
                });
            }
            Ok(())
        }
        "team" => {
            if team_id.is_none() {
                return Err(CliError::User {
                    code: "CREATE_BAD_SCOPE".into(),
                    message: "--team-id is required when scope=`team`".into(),
                    hint: Some("pass --team-id <uuid>".into()),
                });
            }
            Ok(())
        }
        other => Err(CliError::User {
            code: "CREATE_BAD_SCOPE".into(),
            message: format!("scope must be one of personal | team | public, got `{other}`"),
            hint: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_accepts_canonical() {
        validate_slug("intake-cleanup").unwrap();
        validate_slug("a").unwrap();
        validate_slug("9-lives").unwrap();
        validate_slug("month-end-close-2026").unwrap();
    }

    #[test]
    fn slug_rejects_uppercase() {
        assert!(validate_slug("Intake-Cleanup").is_err());
    }

    #[test]
    fn slug_rejects_leading_hyphen() {
        assert!(validate_slug("-foo").is_err());
    }

    #[test]
    fn slug_rejects_underscore() {
        assert!(validate_slug("intake_cleanup").is_err());
    }

    #[test]
    fn slug_rejects_empty() {
        assert!(validate_slug("").is_err());
    }

    #[test]
    fn scope_personal_no_team() {
        validate_scope("personal", None).unwrap();
        assert!(validate_scope("personal", Some("abc")).is_err());
    }

    #[test]
    fn scope_team_requires_id() {
        assert!(validate_scope("team", None).is_err());
        validate_scope("team", Some("uuid")).unwrap();
    }

    #[test]
    fn scope_public_no_team() {
        validate_scope("public", None).unwrap();
        assert!(validate_scope("public", Some("abc")).is_err());
    }

    #[test]
    fn scope_rejects_unknown() {
        assert!(validate_scope("private", None).is_err());
    }

    #[test]
    fn scaffold_writes_all_four_required_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skill");
        write_scaffold(
            &dir,
            "skill-id-abc",
            "humanizetext",
            "Humanize Text",
            "Rewrite AI prose",
            "user@example.com",
        )
        .unwrap();
        assert!(dir.join("SKILL.md").exists());
        assert!(dir.join("meta.knack.yaml").exists());
        assert!(dir.join("intuition.md").exists());
        assert!(dir.join("examples").join("README.md").exists());

        let meta = std::fs::read_to_string(dir.join("meta.knack.yaml")).unwrap();
        // Four required MetaKnack fields all present.
        assert!(meta.contains("id: skill-id-abc"));
        assert!(meta.contains("name: \"Humanize Text\""));
        assert!(meta.contains("slug: humanizetext"));
        assert!(meta.contains("author: user@example.com"));

        let skill_md = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        // Frontmatter present with name + description (server requires these).
        assert!(skill_md.starts_with("---\n"));
        assert!(skill_md.contains("name: Humanize Text"));
        assert!(skill_md.contains("description: Rewrite AI prose"));
    }

    #[test]
    fn scaffold_rejects_non_empty_target() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skill");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("existing.txt"), "stuff").unwrap();
        let err = write_scaffold(
            &dir,
            "id",
            "slug",
            "Name",
            "desc",
            "u@example.com",
        )
        .unwrap_err();
        match err {
            CliError::User { code, .. } => assert_eq!(code, "CREATE_SCAFFOLD_DIR_NOT_EMPTY"),
            other => panic!("expected User error, got {other:?}"),
        }
    }
}
