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

// ─── linked-skill render (`knack link`) ──────────────────────────────────────
//
// Unlike the pointer-style shims above, `knack link` writes the *whole*
// skill into the agent's native skill directory so `/<slug>` works
// standalone. We keep the original SKILL.md frontmatter verbatim (so the
// agent parses `name` / `description` and both slash-invoke and
// description auto-invoke work) and inject a telemetry wrapper into the
// body. The managed-marker sigil goes on the first *body* line — never
// before the frontmatter — so frontmatter parsing is never disturbed.
// [`file_carries_sigil`] scans the head of the file, so the sigil is
// still found for sigil-protected removal.

/// Telemetry preamble injected at the top of a linked skill's body.
/// Best-effort by design: the agent records a run but proceeds even if
/// `knack run` fails (offline / signed out), so a linked skill is never
/// blocked by telemetry.
fn telemetry_preamble(slug: &str, runtime: &str) -> String {
    format!(
        "> **Managed by [knack](https://getknack.ai).** Before doing the work below, \
record a run so this skill's usage is tracked: run \
`knack run {slug} --runtime {runtime} --json` and keep the returned `run_id`. \
If that command fails (offline or not signed in), continue anyway. Telemetry is \
best-effort and must never block the task. If that command reports \
`update_available` (a newer version was published, e.g. by a teammate), tell the \
user a newer version exists and who published it, then ASK whether they want you \
to grab it for them (you would run `knack link {slug}`). Phrase it as offering to \
do it yourself, never as instructing the user to run a command. Do NOT pull the \
new version or change your behavior on your own — this pinned copy is the source \
of truth until the user says go."
    )
}

/// Telemetry footer injected at the end of a linked skill's body.
fn telemetry_footer() -> String {
    "> **When finished**, close the loop: `knack mark <run_id> succeeded` \
(or `knack mark <run_id> failed --note \"...\"`). Skip this if the run above \
was not recorded."
        .to_string()
}

/// Split a SKILL.md into `(frontmatter_head, body)`. `frontmatter_head`
/// is the verbatim `---`…`---` block (no trailing newline); empty when
/// the file has no parseable frontmatter. `body` is everything after,
/// with leading blank lines trimmed. A leading UTF-8 BOM is dropped.
fn split_frontmatter(skill_md: &str) -> (String, String) {
    let no_bom = skill_md.trim_start_matches('\u{feff}');
    if !no_bom.starts_with("---") {
        return (String::new(), no_bom.to_string());
    }
    let Some(first_nl) = no_bom.find('\n') else {
        return (String::new(), no_bom.to_string());
    };
    let after_open = &no_bom[first_nl + 1..];
    let mut byte = 0usize;
    for line in after_open.split_inclusive('\n') {
        let s = line.trim_end_matches(['\r', '\n']);
        if s == "---" || s == "..." {
            // Head spans from start of file through this closing fence line.
            let head_end = first_nl + 1 + byte + line.len();
            let head = no_bom[..head_end].trim_end_matches(['\r', '\n']).to_string();
            let body = no_bom[head_end..].trim_start_matches(['\r', '\n']).to_string();
            return (head, body);
        }
        byte += line.len();
    }
    // No closing fence — treat as no frontmatter.
    (String::new(), no_bom.to_string())
}

/// Render the full SKILL.md for a linked skill: original frontmatter
/// verbatim (kept as the literal file head), then the telemetry preamble,
/// the original body, and the telemetry footer. The [`SHIM_SIGIL`] is the
/// first body line so removal stays sigil-protected without breaking
/// frontmatter parsing.
pub fn render_linked_skill(slug: &str, runtime: &str, original_skill_md: &str) -> String {
    let (head, body) = split_frontmatter(original_skill_md);
    let preamble = telemetry_preamble(slug, runtime);
    let footer = telemetry_footer();
    let body = body.trim_end_matches('\n');
    if head.is_empty() {
        format!("{SHIM_SIGIL}\n\n{preamble}\n\n{body}\n\n{footer}\n")
    } else {
        format!("{head}\n\n{SHIM_SIGIL}\n\n{preamble}\n\n{body}\n\n{footer}\n")
    }
}

/// Render a Cursor `.mdc` rule for a linked skill: description-matched
/// (`alwaysApply: false`), wrapping the telemetry preamble + the skill's
/// body + footer. Sigil on line 1, matching [`render_native_rule`].
pub fn render_linked_rule(
    slug: &str,
    runtime: &str,
    fm: &SkillFrontmatter,
    original_skill_md: &str,
) -> String {
    let description = fm
        .description
        .as_deref()
        .unwrap_or("A skill managed by knack.");
    let (_head, body) = split_frontmatter(original_skill_md);
    let preamble = telemetry_preamble(slug, runtime);
    let footer = telemetry_footer();
    let body = body.trim_end_matches('\n');
    format!(
        "{SHIM_SIGIL}\n---\ndescription: {description}\nalwaysApply: false\n---\n\n{preamble}\n\n{body}\n\n{footer}\n"
    )
}

/// Render a TextBlock body for a linked skill. These runtimes have no
/// native slash-command surface, so this is a pointer entry that still
/// carries the telemetry instruction. No sentinels — [`upsert_skill_block`]
/// adds them.
pub fn render_linked_text_block(slug: &str, runtime: &str, fm: &SkillFrontmatter) -> String {
    let description = fm.description.as_deref().unwrap_or("(no description)");
    format!(
        "- **{slug}** — {description}. Run `knack run {slug} --runtime {runtime}`, \
do the work, then `knack mark <run_id> succeeded`."
    )
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

/// Write a full linked skill into `<root>/<slug>/`: the wrapped `SKILL.md`
/// plus every supporting file (`scripts/…`, `references/…`, `meta.knack.yaml`,
/// etc.) so the slash command is self-contained. Idempotent — unchanged
/// bytes are skipped. `support_files` are `(posix_relpath, bytes)` pairs
/// (the SKILL.md is supplied separately as `wrapped_md` and must not be in
/// the list). Returns `true` if anything changed on disk.
pub fn write_linked_skill(
    root: &Path,
    slug: &str,
    wrapped_md: &str,
    support_files: &[(String, Vec<u8>)],
) -> io::Result<bool> {
    let dir = root.join(slug);
    fs::create_dir_all(&dir)?;
    let mut changed = write_if_changed(&dir.join("SKILL.md"), wrapped_md)?;
    for (rel, bytes) in support_files {
        if rel == "SKILL.md" {
            continue; // never let a raw SKILL.md clobber the wrapped one
        }
        let dest = dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        changed |= write_bytes_if_changed(&dest, bytes)?;
    }
    Ok(changed)
}

/// Remove a linked skill folder `<root>/<slug>/`. Sigil-protected: refuses
/// unless `<root>/<slug>/SKILL.md` carries the knack [`SHIM_SIGIL`], so a
/// user-authored skill sharing the same root is never touched. Removes the
/// **whole** folder (skill + its supporting files), unlike
/// [`remove_native_skill`] which only drops the SKILL.md.
pub fn remove_linked_skill(root: &Path, slug: &str) -> io::Result<bool> {
    let dir = root.join(slug);
    let file = dir.join("SKILL.md");
    if !file.is_file() {
        return Ok(false);
    }
    if !file_carries_sigil(&file) {
        return Ok(false);
    }
    fs::remove_dir_all(&dir)?;
    Ok(true)
}

/// True when `<root>/<slug>/SKILL.md` exists but is NOT knack-managed — i.e.
/// a user-authored skill already occupies this slug. Callers use this to
/// refuse overwriting someone's own skill when linking; it's the write-side
/// counterpart to the sigil-protected removal above.
pub fn foreign_skill_present(root: &Path, slug: &str) -> bool {
    let file = root.join(slug).join("SKILL.md");
    file.is_file() && !file_carries_sigil(&file)
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
                if !path.is_dir() {
                    continue;
                }
                let skill_md = path.join("SKILL.md");
                if !skill_md.is_file() || !file_carries_sigil(&skill_md) {
                    continue;
                }
                // Remove the whole folder: the meta-skill `knack/` dir holds
                // only SKILL.md, but a linked skill also has supporting
                // files (scripts/, references/, …) we own end-to-end.
                let _ = fs::remove_dir_all(&path);
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

/// Byte-oriented sibling of [`write_if_changed`] for supporting files,
/// which may be binary (images under `assets/`, etc.).
fn write_bytes_if_changed(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if let Ok(existing) = fs::read(path) {
        if existing == bytes {
            return Ok(false);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(true)
}

fn read_to_string_or_empty(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// True when `path` is a knack-authored shim. We scan the head of the
/// file rather than only line 1: meta-skill / `.mdc` shims carry the
/// [`SHIM_SIGIL`] on line 1, but linked skills (`knack link`) put it on
/// the first *body* line so the YAML frontmatter stays the literal file
/// head (required for the agent to parse `name`/`description`). Scanning a
/// short head window finds both while still refusing user-authored files
/// that have no sigil near the top.
fn file_carries_sigil(path: &Path) -> bool {
    let Ok(s) = fs::read_to_string(path) else {
        return false;
    };
    s.lines().take(12).any(|l| l.trim() == SHIM_SIGIL)
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
            modes: None,
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

    const LINKED_SRC: &str =
        "---\nname: monthly-close\ndescription: Reconciles Stripe payouts.\n---\n\n# Monthly close\n\nDo the thing.\n";

    #[test]
    fn render_linked_skill_keeps_frontmatter_first_and_wraps_body() {
        let out = render_linked_skill("monthly-close", "claude", LINKED_SRC);
        // Frontmatter is the literal head so the agent parses name/description.
        assert!(out.starts_with("---\nname: monthly-close"));
        assert!(out.contains("description: Reconciles Stripe payouts."));
        // Sigil sits in the body, AFTER the closing fence — never before it.
        let sigil_at = out.find(SHIM_SIGIL).unwrap();
        let second_fence = out[4..].find("\n---").map(|i| i + 4).unwrap();
        assert!(sigil_at > second_fence, "sigil must follow the frontmatter");
        // Telemetry wrapper present with the right slug + runtime.
        assert!(out.contains("knack run monthly-close --runtime claude --json"));
        assert!(out.contains("knack mark <run_id> succeeded"));
        // Original body survives.
        assert!(out.contains("# Monthly close"));
        assert!(out.contains("Do the thing."));
    }

    #[test]
    fn render_linked_skill_without_frontmatter_puts_sigil_first() {
        let out = render_linked_skill("x", "codex", "# Just a body\n");
        assert!(out.starts_with(SHIM_SIGIL));
        assert!(out.contains("# Just a body"));
        assert!(out.contains("--runtime codex"));
    }

    #[test]
    fn split_frontmatter_handles_both_shapes() {
        let (head, body) = split_frontmatter(LINKED_SRC);
        assert!(head.starts_with("---\nname: monthly-close"));
        assert!(head.ends_with("---"));
        assert!(body.starts_with("# Monthly close"));

        let (head2, body2) = split_frontmatter("# no frontmatter\n");
        assert!(head2.is_empty());
        assert_eq!(body2, "# no frontmatter\n");
    }

    #[test]
    fn write_and_remove_linked_skill_round_trip() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let wrapped = render_linked_skill("demo", "claude", LINKED_SRC);
        let support = vec![
            ("scripts/run.py".to_string(), b"print('hi')\n".to_vec()),
            ("meta.knack.yaml".to_string(), b"slug: demo\n".to_vec()),
        ];
        let changed = write_linked_skill(root, "demo", &wrapped, &support).unwrap();
        assert!(changed);
        assert!(root.join("demo").join("SKILL.md").is_file());
        assert!(root.join("demo").join("scripts").join("run.py").is_file());
        // Idempotent second write.
        assert!(!write_linked_skill(root, "demo", &wrapped, &support).unwrap());

        // Removal takes the whole folder (sigil-protected).
        assert!(remove_linked_skill(root, "demo").unwrap());
        assert!(!root.join("demo").exists());
    }

    #[test]
    fn foreign_skill_present_guards_user_authored_slug() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        // Nothing there yet → not foreign.
        assert!(!foreign_skill_present(root, "demo"));
        // A user-authored skill (no sigil) → foreign, must not be overwritten.
        fs::create_dir_all(root.join("demo")).unwrap();
        fs::write(
            root.join("demo").join("SKILL.md"),
            "---\nname: demo\n---\n\nMy own skill.\n",
        )
        .unwrap();
        assert!(foreign_skill_present(root, "demo"));
        // A knack-linked skill (sigil in body) → ours, safe to overwrite.
        fs::write(
            root.join("demo").join("SKILL.md"),
            render_linked_skill("demo", "claude", LINKED_SRC),
        )
        .unwrap();
        assert!(!foreign_skill_present(root, "demo"));
    }

    #[test]
    fn remove_linked_skill_refuses_user_authored_folder() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("mine")).unwrap();
        fs::write(
            root.join("mine").join("SKILL.md"),
            "---\nname: mine\n---\n\nUser body.\n",
        )
        .unwrap();
        assert!(!remove_linked_skill(root, "mine").unwrap());
        assert!(root.join("mine").join("SKILL.md").exists());
    }

    #[test]
    fn file_carries_sigil_matches_body_sigil_not_only_line_one() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("SKILL.md");
        fs::write(&p, render_linked_skill("demo", "claude", LINKED_SRC)).unwrap();
        assert!(file_carries_sigil(&p));

        // Line-1 sigil (meta-skill style) still matches.
        let q = dir.path().join("meta.md");
        fs::write(&q, format!("{SHIM_SIGIL}\n---\nname: knack\n---\n")).unwrap();
        assert!(file_carries_sigil(&q));

        // No sigil anywhere near the top → not ours.
        let r = dir.path().join("user.md");
        fs::write(&r, "---\nname: user\n---\n\nbody\n").unwrap();
        assert!(!file_carries_sigil(&r));
    }
}
