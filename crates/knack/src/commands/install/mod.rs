//! `knack install` — register knack with the AI agent on this machine.
//!
//! For runtimes with native skill discovery (Claude Code, Codex, Cline,
//! Kiro, Windsurf, Trae, Gemini, OpenCode, Factory, Amp, Cowork) we do a
//! **dual write**:
//!
//!   1. **Meta-skill** at `<shim_root_home>/knack/SKILL.md` — full
//!      operating playbook (interview phases, publish flow, runtime
//!      gotchas). Loaded lazily via progressive disclosure when the
//!      user mentions Knack or skill authoring. 200+ tokens, but pays
//!      its way only when relevant.
//!   2. **Pointer block** at `config_path()` (`~/.claude/CLAUDE.md`,
//!      `~/.codex/AGENTS.md`, etc.) — short always-loaded snippet that
//!      tells the agent Knack is installed, lists the headline commands
//!      (`knack list` / `knack info` / `knack run`), and points at the
//!      meta-skill for full detail. ~80 tokens, in every chat from
//!      session start.
//!
//! The split lets the agent know Knack exists from the first turn (via
//! the pointer) without paying the full meta-skill token cost on every
//! chat — disclosure handles that lazily.
//!
//! Other shim styles:
//!
//!   - **NativeRule** (Cursor): single write at
//!     `<shim_root_home>/knack.mdc` with `alwaysApply: false`. Cursor's
//!     rule loader matches it on description; no separate pointer
//!     needed because the file location IS Cursor's general rules
//!     directory.
//!
//!   - **TextBlock** (Aider, Continue.dev): no native skill discovery,
//!     so the meta-skill model doesn't apply. Single write: splice a
//!     delimited block into the agent's free-form rules file
//!     (`CONVENTIONS.md`, `.continue/rules/knack.md`).
//!
//!   - `generic`: AGENTS.md fallback, same as TextBlock.
//!
//! Idempotent: re-runs overwrite the same path with identical bytes
//! (no-op) or splice in place between sentinels. `--uninstall` removes
//! both new-format meta-skill files AND any legacy install blocks /
//! per-skill shims left over from pre-0.4 versions, so the cleanup is
//! comprehensive even after migrations.
//!
//! Autodetect (when called with no arg or `--auto`): walk the target
//! registry, take the first env-marker hit (e.g. `CLAUDECODE=1`), fall
//! back to the first binary-on-PATH hit. Always also writes the generic
//! `~/.config/agents/AGENTS.md` as a safety net so a future AGENTS.md-aware
//! agent picks knack up without re-installing.

pub mod block;
pub mod detect;
pub mod installed;
pub mod shim;
pub mod targets;

use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use crate::errors::{CliError, CliResult};
use crate::output::{emit_err, emit_ok, OutputMode};
use crate::skill_pack::parse_skill_md_frontmatter;

use installed::Scope;
use targets::{AgentTarget, ConfigStyle, ShimStyle};

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Specific agent slug ("claude", "codex", "cursor", "aider", "gemini",
    /// "opencode", "factory", "amp", "generic"). Omit for autodetect.
    pub target: Option<String>,

    /// Same as bare `knack install`. Exists so installer scripts can be
    /// explicit ("we mean autodetect, not 'please ask the user'").
    #[arg(long)]
    pub auto: bool,

    /// Write to every known target on this machine. Useful for testing.
    #[arg(long, conflicts_with = "target")]
    pub all: bool,

    /// Print what would be written without touching the filesystem.
    #[arg(long)]
    pub print: bool,

    /// Remove the knack block from every known agent config file.
    #[arg(long, conflicts_with_all = ["target", "all", "auto", "print"])]
    pub uninstall: bool,
}

/// The Knack meta-skill in canonical SKILL.md form (frontmatter + body).
///
/// Single source of truth for both this CLI and the public install
/// target at https://github.com/knack-skills/knack/blob/main/skills/knack/SKILL.md
/// (mirrored manually on edit). Codex's built-in `skill-installer`
/// fetches the public copy; this constant is what `knack install`
/// writes when running on local agents.
///
/// `render_body` decides per shim style whether to ship this file
/// as-is (NativeSkill), re-wrap its body in `.mdc` frontmatter
/// (NativeRule), or strip the frontmatter and ship plain markdown
/// (TextBlock / None).
const META_SKILL_FULL: &str = include_str!("../../../embedded/meta-skill.md");

/// Short always-loaded "Knack is installed, here's how to find more"
/// pointer that lives in the runtime's general context file
/// (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`, etc.) alongside the
/// progressively-disclosed meta-skill. Goals:
///
///   * Small footprint — pays its way in tokens. Detail lives in the
///     meta-skill, which loads lazily.
///   * Always-loaded awareness — agent knows Knack exists and what
///     it's for from session start, even before the user mentions
///     anything Knack-related. This is the gap the meta-skill model
///     alone left: the meta-skill is invisible until something
///     triggers progressive disclosure.
///   * Points at the meta-skill — if the agent wants more, it
///     knows where to look (and most runtimes will load the
///     meta-skill body automatically once the user mentions skills).
const INSTALL_BLOCK_POINTER: &str = "\
## Knack — skill manager (installed)

The `knack` CLI is installed on this machine. It manages AI agent
skills in the open Anthropic Skills format. Source:
https://github.com/jordan-gibbs/knack — site: https://getknack.ai.

When relevant:
- `knack list` — what skills the user has access to
- `knack info` — full agent playbook (prints to stdout)
- `knack run <slug>` — execute a skill (opens a server-side telemetry record)

Your runtime auto-loads the `knack` meta-skill via progressive
disclosure when the user mentions Knack, skills, or wants to teach a
repeatable workflow. The meta-skill has the full operating guide; this
block is just a pointer.";

pub fn run(args: InstallArgs, mode: OutputMode) -> CliResult<()> {
    if args.uninstall {
        return run_uninstall(mode);
    }

    let targets = select_targets(&args, mode)?;
    let report = apply(&targets, args.print);
    emit_report(mode, &report, args.print);
    Ok(())
}

fn select_targets(args: &InstallArgs, mode: OutputMode) -> CliResult<Vec<&'static AgentTarget>> {
    if args.all {
        return Ok(targets::TARGETS.iter().collect());
    }
    if let Some(name) = &args.target {
        return match targets::find(name) {
            Some(t) => Ok(vec![t]),
            None => {
                let err = CliError::User {
                    code: "UNKNOWN_TARGET".into(),
                    message: format!("unknown agent target: {name}"),
                    hint: Some(format!("known: {}", targets::names().join(", "))),
                };
                emit_err(mode, &err);
                Err(err)
            }
        };
    }
    // Autodetect path: first agent-specific hit, plus generic as safety net.
    let mut out: Vec<&'static AgentTarget> = Vec::new();
    if let Some(t) = detect::autodetect() {
        out.push(t);
    }
    if let Some(g) = targets::find("generic") {
        if !out.iter().any(|t| t.name == "generic") {
            out.push(g);
        }
    }
    Ok(out)
}

#[derive(Debug)]
struct TargetOutcome {
    name: &'static str,
    display: &'static str,
    path: Option<PathBuf>,
    body: String,
    status: Status,
}

#[derive(Debug)]
enum Status {
    Wrote,
    UpToDate,
    DryRun,
    Skipped(String),
}

fn apply(targets: &[&'static AgentTarget], dry_run: bool) -> Vec<TargetOutcome> {
    let mut outcomes: Vec<TargetOutcome> = Vec::new();
    for t in targets {
        outcomes.push(apply_primary(t, dry_run));
        // For NativeSkill targets we ALSO write a short always-loaded
        // pointer block into the runtime's general context file
        // (CLAUDE.md, AGENTS.md, etc.). The meta-skill written above
        // is progressively disclosed; this block is the baseline
        // "knack is installed, here's where the playbook lives"
        // awareness the agent gets at session start.
        //
        // NativeRule (Cursor): no pointer write — its config_path IS
        // the meta-skill location (`~/.cursor/rules/knack.mdc`), so
        // a second write would conflict with itself.
        //
        // TextBlock / None: the primary write IS the install block at
        // config_path, so no separate pointer needed.
        if t.shim_style == ShimStyle::NativeSkill {
            if let Some(outcome) = apply_pointer_block(t, dry_run) {
                outcomes.push(outcome);
            }
        }
    }
    outcomes
}

/// Primary install for `target`. For NativeSkill / NativeRule this is
/// the meta-skill file at `<shim_root>/knack/SKILL.md` (or `knack.mdc`
/// for Cursor). For TextBlock / None this is the install block at
/// `config_path()`.
fn apply_primary(t: &'static AgentTarget, dry_run: bool) -> TargetOutcome {
    let body = render_body(t);
    let Some(path) = install_path_for_target(t) else {
        return TargetOutcome {
            name: t.name,
            display: t.display,
            path: None,
            body,
            status: Status::Skipped("no install path on this OS".into()),
        };
    };
    if dry_run {
        return TargetOutcome {
            name: t.name,
            display: t.display,
            path: Some(path),
            body,
            status: Status::DryRun,
        };
    }
    // NativeSkill / NativeRule: write a full SKILL.md or .mdc
    // file (overwrite). TextBlock / None fall back to the legacy
    // splice-block-into-context-file behavior at config_path.
    let res = match t.shim_style {
        ShimStyle::NativeSkill | ShimStyle::NativeRule => write_full(&path, &body),
        ShimStyle::TextBlock | ShimStyle::None => match t.style {
            ConfigStyle::AppendBlock => block::upsert(&path, &body),
            ConfigStyle::WriteFile => write_full(&path, &body),
        },
    };
    let status = match res {
        Ok(true) => Status::Wrote,
        Ok(false) => Status::UpToDate,
        Err(e) => Status::Skipped(format!("write failed: {e}")),
    };

    // Record the install so `sync` and `--uninstall` can find the file
    // later without recomputing the path. Both `Wrote` and `UpToDate`
    // mean "the target is installed at this path", which is the state
    // the record should reflect. NativeSkill / NativeRule are always
    // HOME-scoped (we write into the user-level skill directory); for
    // TextBlock / None we infer from where the path lives.
    if matches!(status, Status::Wrote | Status::UpToDate) {
        let scope = match t.shim_style {
            ShimStyle::NativeSkill | ShimStyle::NativeRule => Scope::Home,
            _ => infer_scope(&path),
        };
        let _ = installed::add(t.name, scope, path.clone());
    }

    TargetOutcome {
        name: t.name,
        display: t.display,
        path: Some(path),
        body,
        status,
    }
}

/// Secondary install for NativeSkill targets: splice the always-loaded
/// pointer block into the runtime's general context file. Returns
/// `None` if `config_path()` doesn't resolve on this OS (which
/// shouldn't happen for any of our NativeSkill targets but stays
/// defensive).
fn apply_pointer_block(t: &'static AgentTarget, dry_run: bool) -> Option<TargetOutcome> {
    let path = (t.config_path)()?;
    let body = INSTALL_BLOCK_POINTER.to_string();
    if dry_run {
        return Some(TargetOutcome {
            name: t.name,
            display: t.display,
            path: Some(path),
            body,
            status: Status::DryRun,
        });
    }
    // All NativeSkill targets have AppendBlock style for their general
    // context file — they all support a markdown block delimited by
    // sentinels. The block::upsert helper is idempotent and preserves
    // the surrounding user content.
    let res = block::upsert(&path, &body);
    let status = match res {
        Ok(true) => Status::Wrote,
        Ok(false) => Status::UpToDate,
        Err(e) => Status::Skipped(format!("write failed: {e}")),
    };
    Some(TargetOutcome {
        name: t.name,
        display: t.display,
        path: Some(path),
        body,
        status,
    })
}

/// Best-effort scope guess. Used only to tag the `installed.json` entry
/// so `sync` knows whether to refresh workspace shims or HOME shims
/// after each pull. False-positives are cheap (we just rewrite the
/// shim in the other scope on the next sync).
fn infer_scope(path: &std::path::Path) -> Scope {
    if let Some(home) = dirs::home_dir() {
        if path.starts_with(&home) {
            return Scope::Home;
        }
    }
    Scope::Project
}

/// Result of one shim/config-block sweep. Shared by the legacy
/// `install --uninstall` flag and the top-level `knack uninstall`
/// command so both report identical structure.
#[derive(Debug, Default)]
pub(crate) struct StripReport {
    /// `(agent_slug, path)` pairs whose install block / config file was
    /// removed.
    pub removed: Vec<(&'static str, String)>,
    /// Count of per-skill shim files (or text blocks) deleted across
    /// every known target.
    pub shims_removed: usize,
}

/// Walk every known agent target and remove anything knack put there.
///
/// Steps (each independently idempotent):
///   1. Sweep the runtime's native skill / rule directory at both Home
///      and Project scopes. Catches the new-format meta-skill plus any
///      legacy per-skill shims left from pre-0.4 installs.
///   2. Strip the legacy install block from the agent's general context
///      file (`config_path`). For NativeSkill / NativeRule targets this
///      is a backwards-compat sweep; the current install no longer
///      writes there. For TextBlock / None targets this is the primary
///      install location.
///   3. Drop the matching `installed.json` entry so future syncs don't
///      chase a runtime we just walked away from.
///
/// Pure side-effect surface. The caller decides how to render the
/// resulting [`StripReport`] for human / JSON output.
pub(crate) fn strip_shims() -> StripReport {
    let mut report = StripReport::default();
    for t in targets::TARGETS {
        for scope in [Scope::Home, Scope::Project] {
            if let Some(root) = (t.shim_root)(scope) {
                if let Ok(n) = shim::remove_all_shims(t, &root) {
                    report.shims_removed += n;
                }
            }
        }

        if let Some(path) = (t.config_path)() {
            let did = match t.style {
                ConfigStyle::WriteFile => {
                    // Only delete WriteFile config_paths that aren't
                    // already covered by the shim sweep above. Cursor's
                    // config_path equals its shim_root + knack.mdc, so
                    // step 1 already handled it; deleting again would
                    // be a no-op but the existence check avoids spurious
                    // logging.
                    if path.exists() && t.shim_style == ShimStyle::None {
                        std::fs::remove_file(&path).is_ok()
                    } else {
                        false
                    }
                }
                ConfigStyle::AppendBlock => block::remove(&path).unwrap_or(false),
            };
            if did {
                report.removed.push((t.name, path.display().to_string()));
            }
        }

        let _ = installed::remove(t.name);
    }
    report
}

fn run_uninstall(mode: OutputMode) -> CliResult<()> {
    let report = strip_shims();
    if mode.json {
        emit_ok(
            mode,
            json!({
                "removed": report.removed.iter().map(|(n, p)| json!({ "target": n, "path": p })).collect::<Vec<_>>(),
                "shims_removed": report.shims_removed,
            }),
            || {},
        );
    } else if report.removed.is_empty() && report.shims_removed == 0 {
        println!("Nothing to uninstall (no knack block or shims found in any known target).");
    } else {
        for (n, p) in &report.removed {
            println!("Removed {n} → {p}");
        }
        if report.shims_removed > 0 {
            println!("Removed {} per-skill shim(s).", report.shims_removed);
        }
    }
    Ok(())
}

fn write_full(path: &std::path::Path, body: &str) -> std::io::Result<bool> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    if existing == body {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)?;
    Ok(true)
}

/// Strip YAML frontmatter from a SKILL.md string and return the body.
/// Returns the input unchanged if there's no `---` fence, no closing
/// fence, or only a BOM. Doesn't allocate when there's nothing to strip.
fn strip_skill_md_frontmatter(skill_md: &str) -> &str {
    let trimmed = skill_md.trim_start_matches('\u{feff}');
    let after_bom = &skill_md[skill_md.len() - trimmed.len()..];
    if !trimmed.starts_with("---") {
        return after_bom;
    }
    // Skip the opening `---` line.
    let Some(first_nl) = trimmed.find('\n') else {
        return after_bom;
    };
    let after_open = &trimmed[first_nl + 1..];
    // Find the closing `---` (or `...`) on its own line.
    let mut byte = 0usize;
    for line in after_open.split_inclusive('\n') {
        let s = line.trim_end_matches(['\r', '\n']);
        if s == "---" || s == "..." {
            return after_open[byte + line.len()..].trim_start_matches(['\r', '\n']);
        }
        byte += line.len();
    }
    after_bom
}

/// Pull the `description:` field out of a SKILL.md's frontmatter.
/// Used by NativeRule rendering to bake the same description into the
/// `.mdc` wrapper. Returns a generic fallback if parsing fails.
fn meta_skill_description() -> String {
    match parse_skill_md_frontmatter(META_SKILL_FULL) {
        Ok(Some(fm)) => fm
            .description
            .unwrap_or_else(|| "Use the knack CLI for portable agent skill management".into()),
        _ => "Use the knack CLI for portable agent skill management".into(),
    }
}

/// Render the install body for a target. Format depends on shim style:
///
///   - NativeSkill → ship META_SKILL_FULL as-is with sigil prepended.
///     Loaded by the agent's native skill discovery on session start;
///     the SKILL.md's frontmatter description triggers progressive
///     disclosure.
///   - NativeRule → strip the SKILL.md frontmatter, wrap the body in
///     Cursor `.mdc` frontmatter (description + alwaysApply:false) with
///     sigil prepended. Description-matched, not always-on.
///   - TextBlock / None → strip the SKILL.md frontmatter and ship plain
///     markdown body. Spliced into a free-form rules file via
///     [`block::upsert`].
fn render_body(target: &AgentTarget) -> String {
    match target.shim_style {
        ShimStyle::NativeSkill => format!(
            "{sigil}\n{full}",
            sigil = shim::SHIM_SIGIL,
            full = META_SKILL_FULL.trim_start_matches('\u{feff}'),
        ),
        ShimStyle::NativeRule => format!(
            "{sigil}\n---\ndescription: {desc}\nalwaysApply: false\n---\n\n{body}",
            sigil = shim::SHIM_SIGIL,
            desc = meta_skill_description(),
            body = strip_skill_md_frontmatter(META_SKILL_FULL).trim_start(),
        ),
        ShimStyle::TextBlock | ShimStyle::None => {
            let body = strip_skill_md_frontmatter(META_SKILL_FULL).trim_start();
            match target.style {
                ConfigStyle::WriteFile => format!(
                    "---\ndescription: Use the knack CLI for portable skill management\nalwaysApply: true\n---\n\n{body}",
                ),
                ConfigStyle::AppendBlock => body.trim_end_matches('\n').to_string(),
            }
        }
    }
}

/// Where the install body for `target` should land. Differs from
/// `target.config_path()` for NativeSkill / NativeRule targets — those
/// now write a meta-skill file inside the runtime's native skill
/// directory, NOT a block into the agent's general context file.
fn install_path_for_target(target: &AgentTarget) -> Option<PathBuf> {
    match target.shim_style {
        ShimStyle::NativeSkill => {
            (target.shim_root)(Scope::Home).map(|r| r.join("knack").join("SKILL.md"))
        }
        ShimStyle::NativeRule => (target.shim_root)(Scope::Home).map(|r| r.join("knack.mdc")),
        ShimStyle::TextBlock | ShimStyle::None => (target.config_path)(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_dry_run_returns_two_outcomes_per_native_skill_target() {
        // Claude is NativeSkill → dual write: meta-skill + pointer
        // block. Each shows up as its own outcome so the dry-run
        // report lists both paths the user can review.
        let claude = targets::find("claude").expect("claude target registered");
        let outcomes = apply(&[claude], true);
        assert_eq!(outcomes.len(), 2, "expected meta-skill + pointer block");
        for o in &outcomes {
            assert!(matches!(o.status, Status::DryRun));
        }
        // First outcome is the meta-skill (lands in skills/ dir).
        let meta = &outcomes[0];
        let meta_path = meta.path.as_ref().unwrap().display().to_string();
        assert!(
            meta_path.ends_with("knack/SKILL.md") || meta_path.ends_with(r"knack\SKILL.md"),
            "meta-skill path looks wrong: {meta_path}"
        );
        assert!(meta.body.contains("knack list"));
        // Second outcome is the pointer block (lands in CLAUDE.md).
        let pointer = &outcomes[1];
        let pointer_path = pointer.path.as_ref().unwrap().display().to_string();
        assert!(
            pointer_path.ends_with("CLAUDE.md"),
            "pointer path should be CLAUDE.md: {pointer_path}"
        );
        assert!(pointer.body.contains("Knack — skill manager (installed)"));
        assert!(pointer.body.contains("knack info"));
    }

    #[test]
    fn apply_dry_run_single_outcome_for_textblock_target() {
        // Aider is TextBlock → no meta-skill, just the install block
        // at config_path. One outcome only.
        let aider = targets::find("aider").expect("aider target registered");
        let outcomes = apply(&[aider], true);
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0].status, Status::DryRun));
    }

    #[test]
    fn apply_dry_run_single_outcome_for_cursor() {
        // Cursor is NativeRule → its meta-skill .mdc IS its config_path
        // file, so we skip the duplicate pointer write to avoid clobber.
        let cursor = targets::find("cursor").expect("cursor target registered");
        let outcomes = apply(&[cursor], true);
        assert_eq!(outcomes.len(), 1, "Cursor gets one .mdc, not a dual write");
    }

    #[test]
    fn render_body_native_skill_emits_skill_md_with_sigil_and_frontmatter() {
        // Claude is a NativeSkill target — render should produce a
        // SKILL.md that the runtime's progressive disclosure can load.
        let claude = targets::find("claude").expect("claude target registered");
        assert_eq!(claude.shim_style, ShimStyle::NativeSkill);
        let body = render_body(claude);
        assert!(body.starts_with(shim::SHIM_SIGIL), "missing sigil: {body}");
        assert!(body.contains("name: knack"));
        assert!(body.contains("description: For when the user"));
        assert!(body.contains("knack list"));
        assert!(body.contains("knack info"));
    }

    #[test]
    fn render_body_native_rule_emits_mdc_with_alwaysapply_false() {
        // Cursor is the NativeRule target — its meta-skill should be a
        // description-matched rule, not always-on.
        let cursor = targets::find("cursor").expect("cursor target registered");
        assert_eq!(cursor.shim_style, ShimStyle::NativeRule);
        let body = render_body(cursor);
        assert!(body.starts_with(shim::SHIM_SIGIL), "missing sigil: {body}");
        assert!(body.contains("alwaysApply: false"));
        assert!(body.contains("description: For when the user"));
        assert!(body.contains("knack info"));
    }

    #[test]
    fn render_body_textblock_is_plain_markdown_no_frontmatter() {
        // Aider / Continue are TextBlock targets — no native skill
        // discovery, so the body is plain markdown spliced into a
        // free-form rules file (no frontmatter, no sigil).
        let aider = targets::find("aider").expect("aider target registered");
        assert_eq!(aider.shim_style, ShimStyle::TextBlock);
        let body = render_body(aider);
        assert!(
            !body.starts_with("---"),
            "TextBlock body should not have frontmatter"
        );
        assert!(
            !body.starts_with(shim::SHIM_SIGIL),
            "TextBlock body should not have shim sigil"
        );
        assert!(body.contains("knack list"));
        assert!(body.contains("knack info"));
    }

    #[test]
    fn install_path_native_skill_lands_in_skills_subdir() {
        let claude = targets::find("claude").expect("claude target registered");
        let path = install_path_for_target(claude).expect("path resolvable");
        // Should end with `<runtime>/skills/knack/SKILL.md`, NOT
        // `<runtime>/CLAUDE.md` (the legacy install-block location).
        let s = path.display().to_string();
        assert!(
            s.ends_with("knack/SKILL.md") || s.ends_with("knack\\SKILL.md"),
            "got {s}"
        );
        assert!(s.contains("skills"), "got {s}");
    }

    #[test]
    fn install_path_native_rule_lands_at_bare_knack_mdc() {
        let cursor = targets::find("cursor").expect("cursor target registered");
        let path = install_path_for_target(cursor).expect("path resolvable");
        let s = path.display().to_string();
        // `knack.mdc` (meta-skill), NOT `knack-<slug>.mdc` (legacy per-skill).
        assert!(s.ends_with("knack.mdc"), "got {s}");
    }

    #[test]
    fn targets_find_returns_none_for_unknown() {
        assert!(targets::find("not-a-real-agent").is_none());
        assert!(targets::find("claude").is_some());
        assert!(targets::find("generic").is_some());
    }

    #[test]
    fn target_names_includes_every_planned_agent() {
        let names = targets::names();
        for required in [
            "claude", "codex", "cursor", "windsurf", "cline", "continue", "kiro", "trae", "aider",
            "gemini", "opencode", "factory", "amp", "generic",
        ] {
            assert!(names.contains(&required), "missing target: {required}");
        }
    }
}

fn emit_report(mode: OutputMode, outcomes: &[TargetOutcome], dry_run: bool) {
    if mode.json {
        let items: Vec<_> = outcomes
            .iter()
            .map(|o| {
                let (status, reason) = match &o.status {
                    Status::Wrote => ("wrote", None),
                    Status::UpToDate => ("up_to_date", None),
                    Status::DryRun => ("dry_run", None),
                    Status::Skipped(r) => ("skipped", Some(r.clone())),
                };
                json!({
                    "target": o.name,
                    "display": o.display,
                    "path": o.path.as_ref().map(|p| p.display().to_string()),
                    "status": status,
                    "reason": reason,
                })
            })
            .collect();
        emit_ok(
            mode,
            json!({
                "dry_run": dry_run,
                "targets": items,
            }),
            || {},
        );
        return;
    }
    if mode.quiet {
        return;
    }
    for o in outcomes {
        match &o.status {
            Status::Wrote => println!(
                "Wrote {} → {}",
                o.display,
                o.path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
            Status::UpToDate => println!("Already up to date: {}", o.display),
            Status::DryRun => {
                println!(
                    "[dry-run] Would write {} → {}",
                    o.display,
                    o.path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                );
                println!("--- begin block ---");
                println!("{}", o.body);
                println!("--- end block ---");
            }
            Status::Skipped(reason) => println!("Skipped {}: {}", o.display, reason),
        }
    }
}
