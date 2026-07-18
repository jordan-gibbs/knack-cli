//! Local pre-flight check for `knack validate` and `knack publish --dry-run`.
//!
//! Confirms the skill folder has the two files every Anthropic-format
//! skill needs (`SKILL.md` + `meta.knack.yaml`) and that they aren't
//! empty. Deeper schema validation runs server-side: `publish` round-
//! trips through `SKILL_FORMAT_INVALID`, which returns the same
//! `{path, message, code}` issue shape this module emits — so callers
//! handling one envelope handle both.
//!
//! Used to be a Rust port of the server's Python validators (~370
//! lines). Kept in lockstep was a maintenance cost without a real
//! win — the round-trip on `publish` already pays for full schema
//! checks. This is now just the offline existence gate.
//!
//! Output shape mirrors the server's `SKILL_FORMAT_INVALID` envelope:
//! `details: { issues: [{path, message, code}] }`.

use std::path::Path;

use serde_json::{json, Value};

use crate::errors::CliError;
use crate::output::{emit_err, OutputMode};

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

/// Emit the canonical `SKILL_FORMAT_INVALID` envelope for a failed
/// validation report and return the `CliError` the caller should
/// propagate. Three call sites need this exact shape (validate,
/// publish dry-run, publish main paths in both cloud + github), so
/// the helper saves the manual JSON construction from drifting
/// between them.
///
/// The envelope carries the structured `details.issues` payload the
/// agent-side `--json` consumer expects; `emit_err` alone doesn't
/// accept arbitrary `details`, so we hand-render the envelope in JSON
/// mode and fall back to `emit_err` in human mode.
pub fn emit_format_invalid(mode: OutputMode, report: ValidationReport) -> CliError {
    let err = CliError::User {
        code: "SKILL_FORMAT_INVALID".into(),
        message: format!("skill validation failed. issues: {}", report.summary()),
        hint: Some("fix the listed fields in meta.knack.yaml / SKILL.md and re-run".into()),
    };
    if mode.json {
        let env = json!({
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
    err
}

/// Existence-and-non-empty gate for a skill folder, plus the modes
/// manifest check. Real schema validation runs server-side on publish.
pub fn validate_skill_folder(dir: &Path) -> ValidationReport {
    let mut report = ValidationReport::default();

    if !dir.is_dir() {
        report.push("", "skill folder does not exist or is not a directory", "DIR_MISSING");
        return report;
    }

    for required in ["SKILL.md", "meta.knack.yaml"] {
        let path = dir.join(required);
        match std::fs::read(&path) {
            Err(_) => report.push(required, format!("missing required file `{required}`"), "FILE_MISSING"),
            Ok(bytes) if bytes.iter().all(|b| b.is_ascii_whitespace()) => {
                report.push(required, format!("`{required}` is empty"), "FILE_EMPTY")
            }
            Ok(_) => {}
        }
    }

    // Modes gate: publishing a mode whose `load` list points at files
    // that aren't in the folder would ship a partial-loading manifest
    // that silently loads nothing — the exact class of quiet breakage
    // partial loading is meant to prevent. This is offline and cheap,
    // so it runs here rather than only server-side.
    if let Ok(skill_md) = std::fs::read_to_string(dir.join("SKILL.md")) {
        if let Ok(Some(fm)) = crate::skill_pack::parse_skill_md_frontmatter(&skill_md) {
            if let Some(modes) = &fm.modes {
                let arcnames: Vec<String> = crate::skill_pack::collect_skill_entries(dir)
                    .map(|es| es.into_iter().map(|(a, _)| a).collect())
                    .unwrap_or_default();
                for (mode_name, spec) in modes {
                    if spec.load.is_empty() {
                        report.push(
                            &format!("modes/{mode_name}/load"),
                            "mode lists no files to load",
                            "MODE_EMPTY",
                        );
                        continue;
                    }
                    for pat in &spec.load {
                        if !arcnames.iter().any(|a| wildcard_matches(pat, a)) {
                            report.push(
                                &format!("modes/{mode_name}/load"),
                                format!("`{pat}` matches no file in the skill folder"),
                                "MODE_FILE_MISSING",
                            );
                        }
                    }
                }
            }
        }
    }

    report
}

/// Match a mode `load` pattern against a skill-relative posix path.
/// `*` matches within a single path segment (`examples/email-*.md`);
/// there is deliberately no `**` — skill folders are flat enough that
/// per-segment wildcards cover the real cases, and a smaller pattern
/// language means fewer surprising matches.
pub fn wildcard_matches(pattern: &str, path: &str) -> bool {
    let psegs: Vec<&str> = pattern.split('/').collect();
    let fsegs: Vec<&str> = path.split('/').collect();
    if psegs.len() != fsegs.len() {
        return false;
    }
    psegs
        .iter()
        .zip(fsegs.iter())
        .all(|(p, f)| segment_matches(p, f))
}

fn segment_matches(pattern: &str, segment: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == segment;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = segment;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match rest.find(part) {
            // The first part must anchor at the start (no implicit
            // leading `*`), the last must anchor at the end.
            Some(pos) if i == 0 && pos != 0 => return false,
            Some(pos) => rest = &rest[pos + part.len()..],
            None => return false,
        }
    }
    if let Some(last) = parts.last() {
        if !last.is_empty() && !segment.ends_with(last) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn ok_when_both_files_present_and_nonempty() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("SKILL.md"), "# x").unwrap();
        fs::write(dir.path().join("meta.knack.yaml"), "name: x").unwrap();
        assert!(validate_skill_folder(dir.path()).is_ok());
    }

    #[test]
    fn flags_missing_dir() {
        let report = validate_skill_folder(Path::new("/no/such/dir/anywhere"));
        assert!(!report.is_ok());
        assert_eq!(report.issues[0].code, "DIR_MISSING");
    }

    #[test]
    fn flags_missing_skill_md() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("meta.knack.yaml"), "name: x").unwrap();
        let report = validate_skill_folder(dir.path());
        assert!(!report.is_ok());
        assert!(report.issues.iter().any(|i| i.path == "SKILL.md" && i.code == "FILE_MISSING"));
    }

    #[test]
    fn flags_empty_meta_yaml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("SKILL.md"), "# x").unwrap();
        fs::write(dir.path().join("meta.knack.yaml"), "   \n\n  ").unwrap();
        let report = validate_skill_folder(dir.path());
        assert!(!report.is_ok());
        assert!(report.issues.iter().any(|i| i.path == "meta.knack.yaml" && i.code == "FILE_EMPTY"));
    }

    #[test]
    fn wildcard_matching_rules() {
        assert!(wildcard_matches("SKILL.md", "SKILL.md"));
        assert!(wildcard_matches("examples/email-*.md", "examples/email-negotiation.md"));
        assert!(wildcard_matches("examples/*.md", "examples/a.md"));
        assert!(!wildcard_matches("examples/*.md", "references/a.md"));
        assert!(!wildcard_matches("*.md", "examples/a.md")); // no ** semantics
        assert!(!wildcard_matches("examples/email-*.md", "examples/article-1.md"));
        assert!(!wildcard_matches("e*.md", "examples/a.md"));
        assert!(!wildcard_matches("*-email.md", "notemail.md"));
    }

    #[test]
    fn modes_gate_flags_unmatched_load_patterns() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("SKILL.md"),
            "---\nname: s\ndescription: d\nmodes:\n  email:\n    load:\n      - references/style-guide.md\n      - examples/email-*.md\n  broken:\n    load:\n      - examples/missing-*.md\n---\nbody\n",
        )
        .unwrap();
        fs::write(dir.path().join("meta.knack.yaml"), "name: s").unwrap();
        fs::create_dir_all(dir.path().join("references")).unwrap();
        fs::create_dir_all(dir.path().join("examples")).unwrap();
        fs::write(dir.path().join("references/style-guide.md"), "g").unwrap();
        fs::write(dir.path().join("examples/email-1.md"), "e").unwrap();

        let report = validate_skill_folder(dir.path());
        assert!(!report.is_ok());
        // Only the `broken` mode trips; `email` resolves both patterns.
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].code, "MODE_FILE_MISSING");
        assert!(report.issues[0].path.contains("broken"));
    }

    #[test]
    fn modes_gate_passes_folder_without_modes() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("SKILL.md"), "---\nname: s\ndescription: d\n---\nbody\n").unwrap();
        fs::write(dir.path().join("meta.knack.yaml"), "name: s").unwrap();
        assert!(validate_skill_folder(dir.path()).is_ok());
    }

    #[test]
    fn details_envelope_carries_issue_array() {
        let report = validate_skill_folder(Path::new("/no/such/dir"));
        let details = report.into_details();
        assert!(details["issues"].is_array());
        assert!(details["issues"][0]["code"].is_string());
    }
}
