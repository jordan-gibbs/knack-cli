//! `knack diff <slug>@<a> <slug>@<b>` — diff two versions of a skill.
//! Either side may also be a local skill folder (`knack diff foo@1.0.0 ./foo`),
//! which is the "local vs published" mode the help text advertises.
//!
//! Works on both backends: cloud versions come from the API, self-host
//! versions from the registry repo's `<slug>/v<semver>` git tags.
//!
//! Comparison surface: SKILL.md and meta.knack.yaml are always compared.
//! intuition.md is back-compat only — diffed when either side has a
//! non-empty value (legacy skills authored before rules moved inside
//! SKILL.md's `## Intuition` section). Output formats:
//!  - human (ANSI line diff via `similar`)
//!  - json (structured per-file unified diff string)

use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::json;
use similar::{ChangeTag, TextDiff};

use crate::api::{skills as api_skills, ApiClient};
use crate::config::BackendMode;
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Left side: `monthly-close@1.0.0`, or a local skill folder path
    pub left: String,
    /// Right side: `monthly-close@1.1.0`, or a local skill folder path
    pub right: String,
}

/// One side of the diff, parsed from the CLI arg.
#[derive(Debug)]
enum SideSpec {
    Version { slug: String, ver: String },
    Local(PathBuf),
}

/// The three comparable text files of one side, plus its display label.
#[derive(Debug)]
struct SideTexts {
    label: String,
    skill_md: String,
    intuition_md: String,
    meta_yaml: String,
}

pub async fn run(args: DiffArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let left = parse_side(&args.left)?;
    let right = parse_side(&args.right)?;

    if let (SideSpec::Version { slug: l, .. }, SideSpec::Version { slug: r, .. }) = (&left, &right)
    {
        if l != r {
            let err = CliError::User {
                code: "DIFF_DIFFERENT_SKILLS".into(),
                message: "left and right must be the same skill slug".into(),
                hint: Some("e.g. `knack diff foo@1.0.0 foo@1.1.0`".into()),
            };
            emit_err(mode, &err);
            return Err(err);
        }
    }
    if let (SideSpec::Local(_), SideSpec::Local(_)) = (&left, &right) {
        let err = CliError::User {
            code: "DIFF_USAGE".into(),
            message: "at most one side can be a local folder".into(),
            hint: Some("compare a local folder against a published version: `knack diff foo@1.0.0 ./path/to/foo`".into()),
        };
        emit_err(mode, &err);
        return Err(err);
    }

    let left = load_side(&client, left).await?;
    let right = load_side(&client, right).await?;

    let files = [
        ("SKILL.md", &left.skill_md, &right.skill_md),
        ("intuition.md", &left.intuition_md, &right.intuition_md),
        ("meta.knack.yaml", &left.meta_yaml, &right.meta_yaml),
    ];

    let unified: Vec<(String, String)> = files
        .iter()
        .filter_map(|(name, a, b)| {
            if a == b {
                None
            } else {
                Some((name.to_string(), unified_diff(a, b)))
            }
        })
        .collect();

    emit_ok(
        mode,
        json!({
            "left": left.label,
            "right": right.label,
            "files_changed": unified.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            "diffs": unified.iter().map(|(n, d)| json!({"file": n, "unified": d})).collect::<Vec<_>>(),
        }),
        || {
            if unified.is_empty() {
                println!("(identical)");
                return;
            }
            for (name, diff) in &unified {
                println!("--- {name} @ {}", left.label);
                println!("+++ {name} @ {}", right.label);
                print!("{diff}");
            }
        },
    );
    Ok(())
}

fn parse_side(s: &str) -> CliResult<SideSpec> {
    // A real directory wins over the `@` parse so `./skills/@odd/name`
    // shapes still work; a literal folder is unambiguous.
    let p = Path::new(s);
    if p.is_dir() {
        return Ok(SideSpec::Local(p.to_path_buf()));
    }
    if let Some((slug, ver)) = s.split_once('@') {
        if !slug.is_empty() && !ver.is_empty() {
            return Ok(SideSpec::Version {
                slug: slug.to_string(),
                ver: ver.trim_start_matches('v').to_string(),
            });
        }
    }
    Err(CliError::User {
        code: "DIFF_USAGE".into(),
        message: format!("expected `<slug>@<semver>` or a local skill folder path, got `{s}`"),
        hint: Some("e.g. `knack diff monthly-close@1.0.0 monthly-close@1.1.0` or `knack diff monthly-close@1.0.0 ./monthly-close`".into()),
    })
}

async fn load_side(client: &ApiClient, spec: SideSpec) -> CliResult<SideTexts> {
    match spec {
        SideSpec::Local(dir) => load_local(&dir),
        SideSpec::Version { slug, ver } => {
            if let BackendMode::Github { local_path, .. } = &client.config.backend {
                load_github_version(local_path, &slug, &ver).await
            } else {
                load_cloud_version(client, &slug, &ver).await
            }
        }
    }
}

fn load_local(dir: &Path) -> CliResult<SideTexts> {
    let skill_md_path = dir.join("SKILL.md");
    if !skill_md_path.is_file() {
        return Err(CliError::User {
            code: "DIFF_LOCAL_INVALID".into(),
            message: format!("{} has no SKILL.md", dir.display()),
            hint: Some("point at a skill folder (the directory containing SKILL.md)".into()),
        });
    }
    let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
    Ok(SideTexts {
        label: format!("{} (local)", dir.display()),
        skill_md: read(skill_md_path),
        intuition_md: read(dir.join("intuition.md")),
        meta_yaml: read(dir.join("meta.knack.yaml")),
    })
}

/// Self-host: a version is the `<slug>/v<semver>` tag on the registry repo —
/// read the three text files straight out of the tagged tree.
async fn load_github_version(local_path: &Path, slug: &str, ver: &str) -> CliResult<SideTexts> {
    use knack_backend_github::GithubBackend;
    use knack_types::Backend;

    let backend = GithubBackend::new(String::new(), String::new(), local_path.to_path_buf());
    let package = backend
        .pull(slug, Some(ver))
        .await
        .map_err(|e| match e {
            knack_types::BackendError::NotFound(m) => CliError::NotFound(m),
            other => CliError::User {
                code: "GH_DIFF_FAILED".into(),
                message: format!("github diff failed: {other}"),
                hint: None,
            },
        })?;
    let text_of = |name: &str| {
        package
            .files
            .iter()
            .find(|f| f.path == Path::new(name))
            .map(|f| String::from_utf8_lossy(&f.bytes).into_owned())
            .unwrap_or_default()
    };
    Ok(SideTexts {
        label: format!("{slug}@{ver}"),
        skill_md: text_of("SKILL.md"),
        intuition_md: text_of("intuition.md"),
        meta_yaml: text_of("meta.knack.yaml"),
    })
}

async fn load_cloud_version(client: &ApiClient, slug: &str, ver: &str) -> CliResult<SideTexts> {
    let skill = match api_skills::find_by_slug(client, slug).await? {
        Some(s) => s,
        None => return Err(CliError::NotFound(format!("skill `{slug}` not found"))),
    };
    let version = api_skills::get_version(client, &skill.id, ver).await?;
    Ok(SideTexts {
        label: format!("{slug}@{}", version.version),
        skill_md: version.skill_md,
        intuition_md: version.intuition_md,
        meta_yaml: version.meta_yaml,
    })
}

fn unified_diff(a: &str, b: &str) -> String {
    let diff = TextDiff::from_lines(a, b);
    let mut out = String::new();
    for change in diff.iter_all_changes() {
        let prefix = match change.tag() {
            ChangeTag::Equal => " ",
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
        };
        out.push_str(prefix);
        out.push_str(change.value());
        if !change.value().ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_side_slug_at_version() {
        match parse_side("foo@1.0.0").unwrap() {
            SideSpec::Version { slug, ver } => {
                assert_eq!(slug, "foo");
                assert_eq!(ver, "1.0.0");
            }
            _ => panic!("expected version spec"),
        }
    }

    #[test]
    fn parse_side_strips_v_prefix() {
        match parse_side("foo@v1.0.0").unwrap() {
            SideSpec::Version { ver, .. } => assert_eq!(ver, "1.0.0"),
            _ => panic!("expected version spec"),
        }
    }

    #[test]
    fn parse_side_rejects_bare_slug() {
        let err = parse_side("foo").unwrap_err();
        assert_eq!(err.code(), "DIFF_USAGE");
    }

    #[test]
    fn parse_side_accepts_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        match parse_side(dir.path().to_str().unwrap()).unwrap() {
            SideSpec::Local(p) => assert_eq!(p, dir.path()),
            _ => panic!("expected local spec"),
        }
    }

    #[test]
    fn load_local_requires_skill_md() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_local(dir.path()).unwrap_err();
        assert_eq!(err.code(), "DIFF_LOCAL_INVALID");
    }

    #[test]
    fn load_local_reads_texts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SKILL.md"), "# S\n").unwrap();
        std::fs::write(dir.path().join("meta.knack.yaml"), "slug: s\n").unwrap();
        let side = load_local(dir.path()).unwrap();
        assert_eq!(side.skill_md, "# S\n");
        assert_eq!(side.meta_yaml, "slug: s\n");
        assert_eq!(side.intuition_md, "");
        assert!(side.label.ends_with("(local)"));
    }

    #[test]
    fn unified_diff_marks_changes() {
        let d = unified_diff("a\nb\nc\n", "a\nB\nc\n");
        assert!(d.contains("-b"));
        assert!(d.contains("+B"));
    }

    #[test]
    fn unified_diff_empty_when_identical() {
        let d = unified_diff("a\nb\n", "a\nb\n");
        assert!(!d.contains("-"));
        assert!(!d.contains("+"));
    }
}
