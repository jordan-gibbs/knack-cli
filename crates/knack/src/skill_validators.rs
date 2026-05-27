//! Local pre-flight validators for skill folders.
//!
//! Rust port of the server-side validators in
//! `apps/api/knack_api/skill_format/{schema.py,validate.py}`. Catches the
//! common authoring mistakes (missing required `meta.knack.yaml` fields,
//! missing `SKILL.md` frontmatter, missing required frontmatter fields)
//! before the user pays a round-trip to the server. Keeping the schemas in
//! lockstep is a known maintenance cost; the server's
//! `RequestValidationError` envelope handler still catches anything we miss
//! here, so this is purely a UX layer.
//!
//! Output shape mirrors the server's `SKILL_FORMAT_INVALID` envelope:
//! `details: { issues: [{path, message, code}] }`.

use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct ValidationIssue {
    pub path: String,
    pub message: String,
    pub code: &'static str,
}

#[derive(Debug, Default)]
pub struct ValidationReport {
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn is_ok(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn into_details(self) -> Value {
        json!({
            "issues": self.issues.iter().map(|i| json!({
                "path": i.path,
                "message": i.message,
                "code": i.code,
            })).collect::<Vec<_>>(),
        })
    }

    pub fn summary(&self) -> String {
        if self.issues.is_empty() {
            return "ok".to_string();
        }
        self.issues
            .iter()
            .map(|i| {
                if i.path.is_empty() {
                    i.message.clone()
                } else {
                    format!("{}: {}", i.path, i.message)
                }
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn push(&mut self, path: &str, message: impl Into<String>, code: &'static str) {
        self.issues.push(ValidationIssue {
            path: path.into(),
            message: message.into(),
            code,
        });
    }
}

/// Validate a skill folder. Reads SKILL.md and meta.knack.yaml off disk
/// and runs them through the same shape checks the server applies.
pub fn validate_skill_folder(dir: &Path) -> ValidationReport {
    let mut report = ValidationReport::default();

    if !dir.is_dir() {
        report.push("", format!("{} is not a directory", dir.display()), "not_a_dir");
        return report;
    }

    // SKILL.md (required).
    let skill_md_path = dir.join("SKILL.md");
    match std::fs::read_to_string(&skill_md_path) {
        Ok(text) => validate_skill_md(&text, &mut report),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.push("SKILL.md", "missing", "missing_file");
        }
        Err(e) => {
            report.push("SKILL.md", format!("read failed: {e}"), "io_error");
        }
    }

    // meta.knack.yaml (required).
    let meta_path = dir.join("meta.knack.yaml");
    match std::fs::read_to_string(&meta_path) {
        Ok(text) => validate_meta_yaml(&text, &mut report),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.push("meta.knack.yaml", "missing", "missing_file");
        }
        Err(e) => {
            report.push("meta.knack.yaml", format!("read failed: {e}"), "io_error");
        }
    }

    report
}

/// Inspect the YAML frontmatter at the top of SKILL.md. Mirrors the
/// server's `SkillFrontmatter` Pydantic schema:
/// `name: str (1..=200)`, `description: str (1..=2000)`.
pub fn validate_skill_md(text: &str, report: &mut ValidationReport) {
    let frontmatter = match extract_frontmatter(text) {
        Some(fm) => fm,
        None => {
            report.push(
                "SKILL.md",
                "missing YAML frontmatter (--- fences)",
                "missing_frontmatter",
            );
            return;
        }
    };

    let parsed: serde_yaml::Value = match serde_yaml::from_str(&frontmatter) {
        Ok(v) => v,
        Err(e) => {
            report.push("SKILL.md", format!("invalid YAML frontmatter: {e}"), "yaml_parse_error");
            return;
        }
    };
    let map = match parsed.as_mapping() {
        Some(m) => m,
        None => {
            report.push(
                "SKILL.md",
                "frontmatter must be a mapping (key: value pairs)",
                "bad_type",
            );
            return;
        }
    };

    let name = map.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name.is_empty() {
        report.push("SKILL.md/name", "missing or empty", "missing_field");
    } else if name.len() > 200 {
        report.push(
            "SKILL.md/name",
            format!("{} chars exceeds 200 max", name.len()),
            "too_long",
        );
    }
    let description = map.get("description").and_then(|v| v.as_str()).unwrap_or("");
    if description.is_empty() {
        report.push("SKILL.md/description", "missing or empty", "missing_field");
    } else if description.len() > 2000 {
        report.push(
            "SKILL.md/description",
            format!("{} chars exceeds 2000 max", description.len()),
            "too_long",
        );
    }
}

/// Mirror of the server's `MetaKnack` schema. Required fields:
/// name, slug, author. Slug must match `^[a-z0-9][a-z0-9-]*$`. `id` is
/// server-managed: the identity is pinned by the URL path on publish, so
/// the file does not have to repeat it. If an `id` is present, we don't
/// touch it; if it's missing, we don't complain.
pub fn validate_meta_yaml(text: &str, report: &mut ValidationReport) {
    #[derive(Deserialize)]
    struct MetaShape {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        slug: Option<String>,
        #[serde(default)]
        author: Option<String>,
    }
    let meta: MetaShape = match serde_yaml::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            report.push("meta.knack.yaml", format!("invalid YAML: {e}"), "yaml_parse_error");
            return;
        }
    };

    require_non_empty(&meta.name, "meta.knack.yaml/name", report);
    require_non_empty(&meta.author, "meta.knack.yaml/author", report);

    match meta.slug.as_deref().map(str::trim).unwrap_or("") {
        "" => {
            report.push("meta.knack.yaml/slug", "missing or empty", "missing_field");
        }
        slug if !is_valid_slug(slug) => {
            report.push(
                "meta.knack.yaml/slug",
                format!("`{slug}` must match ^[a-z0-9][a-z0-9-]*$"),
                "bad_pattern",
            );
        }
        _ => {}
    }
}

fn require_non_empty(
    field: &Option<String>,
    path: &str,
    report: &mut ValidationReport,
) {
    let empty = match field {
        None => true,
        Some(s) => s.trim().is_empty(),
    };
    if empty {
        report.push(path, "missing or empty", "missing_field");
    }
}

fn is_valid_slug(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Extract the YAML body between the first two `---` fences. Returns
/// `None` if frontmatter is missing or unterminated.
fn extract_frontmatter(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix("---\n").or_else(|| trimmed.strip_prefix("---\r\n"))?;
    // End of frontmatter: a line containing only "---".
    let end = rest
        .split_inclusive('\n')
        .scan(0usize, |acc, line| {
            let start = *acc;
            *acc += line.len();
            Some((start, line))
        })
        .find_map(|(start, line)| {
            let stripped = line.trim_end_matches(&['\n', '\r'][..]);
            if stripped == "---" {
                Some(start)
            } else {
                None
            }
        })?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_yaml_three_required_pass() {
        // id is server-managed and optional in the file now; only name,
        // slug, author must be supplied.
        let yaml = "name: \"Foo\"\nslug: foo\nauthor: u@example.com\n";
        let mut r = ValidationReport::default();
        validate_meta_yaml(yaml, &mut r);
        assert!(r.is_ok(), "{}", r.summary());
    }

    #[test]
    fn meta_yaml_id_present_still_passes() {
        // Backward compat: existing files with `id:` keep working.
        let yaml = "id: skill-id-abc\nname: \"Foo\"\nslug: foo\nauthor: u@example.com\n";
        let mut r = ValidationReport::default();
        validate_meta_yaml(yaml, &mut r);
        assert!(r.is_ok(), "{}", r.summary());
    }

    #[test]
    fn meta_yaml_missing_fields_each_reported() {
        let yaml = "requires_tools:\n  - python\n";
        let mut r = ValidationReport::default();
        validate_meta_yaml(yaml, &mut r);
        let paths: Vec<&str> = r.issues.iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"meta.knack.yaml/name"));
        assert!(paths.contains(&"meta.knack.yaml/slug"));
        assert!(paths.contains(&"meta.knack.yaml/author"));
        // id should NOT be flagged any more.
        assert!(!paths.contains(&"meta.knack.yaml/id"));
    }

    #[test]
    fn meta_yaml_bad_slug_pattern() {
        let yaml = "id: abc\nname: x\nslug: Has-Caps\nauthor: u@x\n";
        let mut r = ValidationReport::default();
        validate_meta_yaml(yaml, &mut r);
        let paths: Vec<&str> = r.issues.iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"meta.knack.yaml/slug"));
    }

    #[test]
    fn meta_yaml_invalid_yaml_reports() {
        let yaml = "id: [unclosed\n";
        let mut r = ValidationReport::default();
        validate_meta_yaml(yaml, &mut r);
        assert!(r.issues.iter().any(|i| i.code == "yaml_parse_error"));
    }

    #[test]
    fn skill_md_missing_frontmatter() {
        let mut r = ValidationReport::default();
        validate_skill_md("# Just a body\n", &mut r);
        assert!(r.issues.iter().any(|i| i.code == "missing_frontmatter"));
    }

    #[test]
    fn skill_md_happy_path() {
        let md = "---\nname: Foo\ndescription: Bar\n---\n\n# Body\n";
        let mut r = ValidationReport::default();
        validate_skill_md(md, &mut r);
        assert!(r.is_ok(), "{}", r.summary());
    }

    #[test]
    fn skill_md_missing_description() {
        let md = "---\nname: Foo\n---\n\n# Body\n";
        let mut r = ValidationReport::default();
        validate_skill_md(md, &mut r);
        let paths: Vec<&str> = r.issues.iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"SKILL.md/description"));
    }

    #[test]
    fn folder_validation_end_to_end() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: T\ndescription: D\n---\n\n# Body\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("meta.knack.yaml"),
            "id: abc\nname: T\nslug: t\nauthor: u@x\n",
        )
        .unwrap();
        let report = validate_skill_folder(dir);
        assert!(report.is_ok(), "{}", report.summary());
    }

    #[test]
    fn folder_validation_misses_both_files() {
        let tmp = tempfile::tempdir().unwrap();
        let report = validate_skill_folder(tmp.path());
        let codes: Vec<&str> = report.issues.iter().map(|i| i.code).collect();
        assert!(codes.iter().filter(|c| **c == "missing_file").count() >= 2);
    }
}
