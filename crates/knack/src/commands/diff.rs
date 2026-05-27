//! `knack diff <slug>@<a> <slug>@<b>` — diff two versions of a skill.
//!
//! Comparison surface: SKILL.md, intuition.md, meta.knack.yaml. Output formats:
//!  - human (ANSI line diff via `similar`)
//!  - json (structured per-file unified diff string)

use clap::Args;
use serde_json::json;
use similar::{ChangeTag, TextDiff};

use crate::api::{skills as api_skills, ApiClient};
use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Left side, e.g. `monthly-close@1.0.0`
    pub left: String,
    /// Right side, e.g. `monthly-close@1.1.0`
    pub right: String,
}

pub async fn run(args: DiffArgs, client: ApiClient, mode: OutputMode) -> CliResult<()> {
    let (lslug, lver) = split(&args.left)?;
    let (rslug, rver) = split(&args.right)?;
    if lslug != rslug {
        let err = CliError::User {
            code: "DIFF_DIFFERENT_SKILLS".into(),
            message: "left and right must be the same skill slug".into(),
            hint: Some("e.g. `knack diff foo@1.0.0 foo@1.1.0`".into()),
        };
        emit_err(mode, &err);
        return Err(err);
    }

    let skill = match api_skills::find_by_slug(&client, lslug).await? {
        Some(s) => s,
        None => {
            let err = CliError::NotFound(format!("skill `{lslug}` not found"));
            emit_err(mode, &err);
            return Err(err);
        }
    };

    let left = api_skills::get_version(&client, &skill.id, lver).await?;
    let right = api_skills::get_version(&client, &skill.id, rver).await?;

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
            "skill": lslug,
            "left": left.version,
            "right": right.version,
            "files_changed": unified.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            "diffs": unified.iter().map(|(n, d)| json!({"file": n, "unified": d})).collect::<Vec<_>>(),
        }),
        || {
            if unified.is_empty() {
                println!("(identical)");
                return;
            }
            for (name, diff) in &unified {
                println!("--- {name} @ {}", left.version);
                println!("+++ {name} @ {}", right.version);
                print!("{diff}");
            }
        },
    );
    Ok(())
}

fn split(s: &str) -> CliResult<(&str, &str)> {
    s.split_once('@').ok_or_else(|| CliError::User {
        code: "DIFF_USAGE".into(),
        message: format!("expected `<slug>@<semver>`, got `{s}`"),
        hint: Some("e.g. `monthly-close@1.0.0`".into()),
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
    fn split_parses_slug_at_version() {
        assert_eq!(split("foo@1.0.0").unwrap(), ("foo", "1.0.0"));
    }

    #[test]
    fn split_rejects_missing_at() {
        let err = split("foo").unwrap_err();
        assert_eq!(err.code(), "USER_ERROR");
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
