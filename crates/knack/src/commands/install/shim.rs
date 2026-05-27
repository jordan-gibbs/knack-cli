//! Per-skill shim renderers + writers.
//!
//! A "shim" makes a pulled skill *natively discoverable* by an agent
//! runtime. Three shapes, dispatched by [`crate::commands::install::targets::ShimStyle`]:
//!
//! * [`ShimStyle::NativeSkill`] — Claude Code / Cowork. One folder per
//!   skill at `<root>/<slug>/SKILL.md` with YAML frontmatter and a body
//!   pointer at the canonical `.knack/skills/<slug>/SKILL.md`. Frontmatter
//!   is what triggers progressive disclosure on session start.
//! * [`ShimStyle::NativeRule`] — Cursor. One `.mdc` file per skill at
//!   `<root>/knack-<slug>.mdc`. Cursor's rule loader keys on the
//!   frontmatter `description`.
//! * [`ShimStyle::TextBlock`] — every other agent. A sentinel-bracketed
//!   block inserted into the same context file `knack install` already
//!   manages (AGENTS.md, CONVENTIONS.md, …). Inserted *after* the install
//!   block so the two never overlap.
//!
//! Every shim we author carries a first-line sigil (or sentinel-pair
//! delimiter for TextBlock). Removal refuses to delete files / blocks
//! that lack the sigil — that protects user-authored Claude skills that
//! happen to live in `~/.claude/skills/` for other reasons.

use std::fs;
use std::io;
use std::path::Path;

use crate::skill_pack::SkillFrontmatter;

use super::block::{END_MARKER, START_MARKER};
use super::targets::{AgentTarget, ShimStyle};

/// First-line marker stamped onto every native-style shim (Claude
/// SKILL.md, Cursor .mdc). Removal refuses to delete shim files whose
/// first line doesn't match this exact string, which is what keeps us
/// from nuking user-authored skills sharing `~/.claude/skills/`.
pub const SHIM_SIGIL: &str = "<!-- knack:shim — managed; do not edit -->";

/// Per-skill sentinel pair for TextBlock shims. Each pair is keyed by
/// the skill slug so individual skills can be added/removed without
/// rewriting the whole context file.
pub fn text_block_start(slug: &str) -> String {
    format!("<!-- knack:skill:{slug}:start -->")
}
pub fn text_block_end(slug: &str) -> String {
    format!("<!-- knack:skill:{slug}:end -->")
}

// ─── render ─────────────────────────────────────────────────────────────────

/// Render the body of a Claude Code shim SKILL.md.
///
/// `canonical_md_path` is the absolute on-disk location of the source
/// `.knack/skills/<slug>/SKILL.md` (the body the agent should load when
/// the description triggers disclosure).
pub fn render_native_skill(slug: &str, fm: &SkillFrontmatter, canonical_md_path: &Path) -> String {
    let name = fm.name.as_deref().unwrap_or(slug);
    let description = fm
        .description
        .as_deref()
        .unwrap_or("A skill managed by knack. Run `knack run <slug>` to invoke.");
    format!(
        "{SHIM_SIGIL}
---
name: {name}
description: {description}
---

This skill is managed by [knack](https://getknack.ai). Canonical body:

  {path}

Invoke with: `knack run {slug}`
",
        path = canonical_md_path.display(),
    )
}

/// Render the body of a Cursor `.mdc` rule.
pub fn render_native_rule(slug: &str, fm: &SkillFrontmatter, canonical_md_path: &Path) -> String {
    let description = fm
        .description
        .as_deref()
        .unwrap_or("A skill managed by knack. Run `knack run <slug>` to invoke.");
    format!(
        "{SHIM_SIGIL}
---
description: {description}
alwaysApply: false
---

When this rule matches, invoke `knack run {slug}`.
Canonical: {path}
",
        path = canonical_md_path.display(),
    )
}

/// Render the *body* of a TextBlock shim (no sentinels — those are added
/// by [`upsert_skill_block`]).
pub fn render_text_block(slug: &str, fm: &SkillFrontmatter) -> String {
    let description = fm.description.as_deref().unwrap_or("(no description)");
    format!("- **{slug}** — {description}. Run: `knack run {slug}`")
}

// ─── write ─────────────────────────────────────────────────────────────────

/// Write a Claude Code SKILL.md shim at `<root>/<slug>/SKILL.md`.
/// Idempotent — re-running with identical bytes is a no-op.
/// Returns `true` if the file's bytes changed.
pub fn write_native_skill(root: &Path, slug: &str, body: &str) -> io::Result<bool> {
    let dir = root.join(slug);
    fs::create_dir_all(&dir)?;
    let file = dir.join("SKILL.md");
    write_if_changed(&file, body)
}

/// Remove the Claude SKILL.md shim folder for `slug`. Returns `true`
/// when something was removed. **Refuses** to delete unless the file's
/// first line is [`SHIM_SIGIL`] — that protects user-authored skills
/// that happen to share the same `~/.claude/skills/` root.
pub fn remove_native_skill(root: &Path, slug: &str) -> io::Result<bool> {
    let dir = root.join(slug);
    let file = dir.join("SKILL.md");
    if !file.exists() {
        return Ok(false);
    }
    if !file_carries_sigil(&file) {
        // User-authored skill at this path; leave it alone.
        return Ok(false);
    }
    fs::remove_file(&file)?;
    // Best-effort dir cleanup. If the user dropped other files into the
    // shim folder we leave them; otherwise the empty dir is just
    // clutter.
    if fs::read_dir(&dir)
        .map(|mut it| it.next().is_none())
        .unwrap_or(false)
    {
        let _ = fs::remove_dir(&dir);
    }
    Ok(true)
}

/// Write a Cursor `.mdc` shim at `<root>/knack-<slug>.mdc`. Idempotent.
pub fn write_native_rule(root: &Path, slug: &str, body: &str) -> io::Result<bool> {
    fs::create_dir_all(root)?;
    let file = root.join(format!("knack-{slug}.mdc"));
    write_if_changed(&file, body)
}

/// Remove the Cursor `.mdc` shim for `slug`. Sigil-protected.
pub fn remove_native_rule(root: &Path, slug: &str) -> io::Result<bool> {
    let file = root.join(format!("knack-{slug}.mdc"));
    if !file.exists() {
        return Ok(false);
    }
    if !file_carries_sigil(&file) {
        return Ok(false);
    }
    fs::remove_file(&file)?;
    Ok(true)
}

/// Insert (or replace in place) a sentinel-bracketed per-skill block
/// inside an agent's context file. Idempotent. Returns `true` when the
/// file changed.
///
/// Placement when no existing block for this slug is found:
///   * If the file contains a [`super::block::END_MARKER`] (install
///     block), insert immediately after it, separated by a blank line.
///   * Otherwise, append at EOF.
///
/// The install block and shim blocks are siblings — they never nest.
pub fn upsert_skill_block(path: &Path, slug: &str, body: &str) -> io::Result<bool> {
    let existing = read_to_string_or_empty(path)?;
    let start = text_block_start(slug);
    let end = text_block_end(slug);
    let block = format!("{start}\n{body}\n{end}");

    let next = if let Some((s, e)) = find_block(&existing, &start, &end) {
        // In-place replacement.
        let mut new = String::with_capacity(existing.len() + 32);
        new.push_str(&existing[..s]);
        new.push_str(&block);
        new.push_str(&existing[e..]);
        new
    } else if let Some(idx) = existing.find(END_MARKER) {
        // Insert after the install block's END_MARKER + the newline after it.
        let after = idx + END_MARKER.len();
        let head = &existing[..after];
        let tail = existing[after..].trim_start_matches('\n');
        let trimmed_head = head.trim_end_matches('\n');
        if tail.is_empty() {
            format!("{trimmed_head}\n\n{block}\n")
        } else {
            format!("{trimmed_head}\n\n{block}\n\n{tail}")
        }
    } else if existing.trim().is_empty() {
        format!("{block}\n")
    } else {
        let trimmed = existing.trim_end_matches('\n');
        format!("{trimmed}\n\n{block}\n")
    };

    if next == existing {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, next)?;
    Ok(true)
}

/// Remove the sentinel-bracketed per-skill block for `slug` from
/// `path`. Returns `true` if anything changed. If removing the block
/// leaves the file empty (only whitespace), delete the file.
pub fn remove_skill_block(path: &Path, slug: &str) -> io::Result<bool> {
    let Ok(existing) = fs::read_to_string(path) else {
        return Ok(false);
    };
    let start = text_block_start(slug);
    let end = text_block_end(slug);
    let Some((s, e)) = find_block(&existing, &start, &end) else {
        return Ok(false);
    };
    let head = existing[..s].trim_end_matches('\n');
    let tail = existing[e..].trim_start_matches('\n');
    let next = match (head.is_empty(), tail.is_empty()) {
        (true, true) => String::new(),
        (true, false) => tail.to_string(),
        (false, true) => format!("{head}\n"),
        (false, false) => format!("{head}\n\n{tail}"),
    };
    if next.trim().is_empty() {
        let _ = fs::remove_file(path);
    } else {
        fs::write(path, next)?;
    }
    Ok(true)
}

/// Walk every shim file under a [`AgentTarget::shim_root`] and delete
/// the knack-authored ones. Returns the count removed. Used by
/// `knack sync --purge` and by `knack install --uninstall`.
pub fn remove_all_shims(target: &AgentTarget, root: &Path) -> io::Result<usize> {
    let mut removed = 0;
    match target.shim_style {
        ShimStyle::NativeSkill => {
            if !root.is_dir() {
                return Ok(0);
            }
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                let path = entry.path();
                let skill_md = path.join("SKILL.md");
                if !skill_md.is_file() || !file_carries_sigil(&skill_md) {
                    continue;
                }
                let _ = fs::remove_file(&skill_md);
                if fs::read_dir(&path)
                    .map(|mut it| it.next().is_none())
                    .unwrap_or(false)
                {
                    let _ = fs::remove_dir(&path);
                }
                removed += 1;
            }
        }
        ShimStyle::NativeRule => {
            if !root.is_dir() {
                return Ok(0);
            }
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                let path = entry.path();
                let fname = match path.file_name().and_then(|s| s.to_str()) {
                    Some(n) => n,
                    None => continue,
                };
                // Match both the new meta-skill (`knack.mdc`) and any
                // legacy per-skill shims (`knack-<slug>.mdc`).
                let matches_meta = fname == "knack.mdc";
                let matches_legacy = fname.starts_with("knack-") && fname.ends_with(".mdc");
                if !(matches_meta || matches_legacy) {
                    continue;
                }
                if !file_carries_sigil(&path) {
                    continue;
                }
                let _ = fs::remove_file(&path);
                removed += 1;
            }
        }
        ShimStyle::TextBlock => {
            // Sweep every `<!-- knack:skill:*:start -->` block in the
            // file. We don't know the slugs in advance; scan + extract.
            if !root.exists() {
                return Ok(0);
            }
            let body = fs::read_to_string(root)?;
            let mut next = body.clone();
            let prefix = "<!-- knack:skill:";
            while let Some(s) = next.find(prefix) {
                // Extract slug between prefix and ":start -->".
                let rest = &next[s + prefix.len()..];
                let Some(colon) = rest.find(':') else { break };
                let slug = &rest[..colon];
                let slug = slug.to_string();
                if !remove_skill_block_from_string(&mut next, &slug) {
                    break; // safety: avoid infinite loop on malformed input
                }
                removed += 1;
            }
            if next != body {
                if next.trim().is_empty() {
                    let _ = fs::remove_file(root);
                } else {
                    fs::write(root, next)?;
                }
            }
        }
        ShimStyle::None => {}
    }
    Ok(removed)
}

// ─── internals ─────────────────────────────────────────────────────────────

fn write_if_changed(path: &Path, body: &str) -> io::Result<bool> {
    if let Ok(existing) = fs::read_to_string(path) {
        if existing == body {
            return Ok(false);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, body)?;
    Ok(true)
}

fn read_to_string_or_empty(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

fn file_carries_sigil(path: &Path) -> bool {
    let Ok(s) = fs::read_to_string(path) else {
        return false;
    };
    s.lines()
        .next()
        .map(|l| l.trim() == SHIM_SIGIL)
        .unwrap_or(false)
}

/// Return (block-start byte offset, block-end byte offset, end exclusive)
/// for a `<!-- knack:skill:<slug>:start --> ... :end -->` pair in
/// `input`, or `None` when no such pair exists.
fn find_block(input: &str, start_marker: &str, end_marker: &str) -> Option<(usize, usize)> {
    let s = input.find(start_marker)?;
    let after = s + start_marker.len();
    let rel_end = input[after..].find(end_marker)?;
    let e = after + rel_end + end_marker.len();
    Some((s, e))
}

/// In-string variant for [`remove_all_shims`]. Mutates `s` in place.
fn remove_skill_block_from_string(s: &mut String, slug: &str) -> bool {
    let start = text_block_start(slug);
    let end = text_block_end(slug);
    let Some((bs, be)) = find_block(s, &start, &end) else {
        return false;
    };
    let head = s[..bs].trim_end_matches('\n').to_string();
    let tail = s[be..].trim_start_matches('\n').to_string();
    *s = match (head.is_empty(), tail.is_empty()) {
        (true, true) => String::new(),
        (true, false) => tail,
        (false, true) => format!("{head}\n"),
        (false, false) => format!("{head}\n\n{tail}"),
    };
    true
}

#[allow(dead_code)] // kept for symmetry with future test helpers
const _START_REF: &str = START_MARKER;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fm(name: &str, desc: &str) -> SkillFrontmatter {
        SkillFrontmatter {
            name: Some(name.into()),
            description: Some(desc.into()),
        }
    }

    #[test]
    fn render_native_skill_uses_frontmatter_description() {
        let body = render_native_skill(
            "monthly-close",
            &fm("monthly-close", "Reconciles Stripe payouts every Monday."),
            Path::new("/x/.knack/skills/monthly-close/SKILL.md"),
        );
        assert!(body.starts_with(SHIM_SIGIL));
        assert!(body.contains("name: monthly-close"));
        assert!(body.contains("description: Reconciles Stripe payouts every Monday."));
        assert!(body.contains("knack run monthly-close"));
    }

    #[test]
    fn render_native_rule_emits_mdc_with_alwaysapply_false() {
        let body = render_native_rule(
            "monthly-close",
            &fm("x", "Reconciles payouts."),
            Path::new("/repo/.knack/skills/monthly-close/SKILL.md"),
        );
        assert!(body.contains("alwaysApply: false"));
        assert!(body.contains("description: Reconciles payouts."));
        assert!(body.contains("knack run monthly-close"));
    }

    #[test]
    fn render_text_block_includes_slug_and_description() {
        let body = render_text_block(
            "weekly-digest",
            &fm("weekly-digest", "Top tickets, one fix."),
        );
        assert!(body.contains("- **weekly-digest**"));
        assert!(body.contains("Top tickets, one fix."));
        assert!(body.contains("`knack run weekly-digest`"));
    }

    #[test]
    fn write_native_skill_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let body = "abc";
        let first = write_native_skill(dir.path(), "foo", body).unwrap();
        let second = write_native_skill(dir.path(), "foo", body).unwrap();
        assert!(first);
        assert!(!second);
        let content = fs::read_to_string(dir.path().join("foo").join("SKILL.md")).unwrap();
        assert_eq!(content, body);
    }

    #[test]
    fn remove_native_skill_refuses_without_sigil() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("custom")).unwrap();
        fs::write(
            dir.path().join("custom").join("SKILL.md"),
            "---\nname: custom\n---\n\nUser-authored body.\n",
        )
        .unwrap();

        // Refuses to delete: returns false, file still exists.
        let removed = remove_native_skill(dir.path(), "custom").unwrap();
        assert!(!removed);
        assert!(dir.path().join("custom").join("SKILL.md").exists());
    }

    #[test]
    fn remove_native_skill_deletes_with_sigil() {
        let dir = TempDir::new().unwrap();
        let body = format!("{SHIM_SIGIL}\n---\nname: foo\n---\n\nbody\n");
        write_native_skill(dir.path(), "foo", &body).unwrap();
        let removed = remove_native_skill(dir.path(), "foo").unwrap();
        assert!(removed);
        assert!(!dir.path().join("foo").join("SKILL.md").exists());
    }

    #[test]
    fn upsert_skill_block_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("AGENTS.md");
        let body = "- **foo** — bar";
        assert!(upsert_skill_block(&path, "foo", body).unwrap());
        let after_first = fs::read_to_string(&path).unwrap();
        assert!(!upsert_skill_block(&path, "foo", body).unwrap());
        assert_eq!(after_first, fs::read_to_string(&path).unwrap());
    }

    #[test]
    fn upsert_skill_block_replaces_existing_same_slug() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("AGENTS.md");
        upsert_skill_block(&path, "foo", "- **foo** — v1").unwrap();
        upsert_skill_block(&path, "foo", "- **foo** — v2").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("v2"));
        assert!(!content.contains("v1"));
        assert_eq!(content.matches(&text_block_start("foo")).count(), 1);
    }

    #[test]
    fn upsert_skill_block_inserts_after_install_block_when_present() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("AGENTS.md");
        let initial = format!(
            "# pre\n\n{START_MARKER}\nknack install body\n{END_MARKER}\n\n# user content after\n"
        );
        fs::write(&path, &initial).unwrap();
        upsert_skill_block(&path, "foo", "- **foo** — bar").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        // Order: install block → shim block → user content.
        let install_idx = content.find(START_MARKER).unwrap();
        let shim_idx = content.find(&text_block_start("foo")).unwrap();
        let user_idx = content.find("# user content after").unwrap();
        assert!(install_idx < shim_idx);
        assert!(shim_idx < user_idx);
    }

    #[test]
    fn upsert_skill_block_appends_when_no_install_block_exists() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, "# user content\n").unwrap();
        upsert_skill_block(&path, "foo", "- **foo** — bar").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("# user content"));
        assert!(content.contains(&text_block_start("foo")));
    }

    #[test]
    fn remove_skill_block_only_removes_matching_slug() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("AGENTS.md");
        upsert_skill_block(&path, "foo", "- foo body").unwrap();
        upsert_skill_block(&path, "bar", "- bar body").unwrap();
        assert!(remove_skill_block(&path, "foo").unwrap());
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains(&text_block_start("foo")));
        assert!(content.contains(&text_block_start("bar")));
    }
}
