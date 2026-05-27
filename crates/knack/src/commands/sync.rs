//! `knack sync` — no-op in the meta-skill model; kept for API stability.
//!
//! Pre-0.4 versions wrote per-skill shims into each installed agent's
//! native discovery surface (`~/.claude/skills/<slug>/SKILL.md`,
//! `~/.agents/skills/<slug>/SKILL.md`, etc.) after every pull/publish.
//! The 0.4 meta-skill model dropped that: agents discover individual
//! skills via `knack list` at session time, not via on-disk shims, so
//! the per-skill write loop is gone.
//!
//! What's left:
//!   - `knack sync --purge` still removes leftover sigil-bearing shims
//!     (the cleanup path for users migrating off pre-0.4 layouts) AND
//!     the new-format meta-skill files.
//!   - `knack sync` (no flag) and `knack sync --all-detected` are now
//!     idempotent no-ops that emit an empty report. Callers (pull,
//!     publish) keep invoking [`sync_one_skill`]; the signature is
//!     stable so call sites don't have to special-case.

use clap::Args;
use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::errors::CliResult;
use crate::output::{chatter, emit_ok, OutputMode};

use super::install::{installed, installed::Scope, shim, targets};

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Limit sync to one runtime by slug. No-op in 0.4+, retained so
    /// the CLI surface doesn't break for callers that still pass it.
    #[arg(long)]
    pub agent: Option<String>,

    /// Reserved. No-op in 0.4+ (per-skill shims are gone). Retained for
    /// CLI stability with pre-0.4 wrapper scripts.
    #[arg(long)]
    pub all_detected: bool,

    /// Print intended writes/deletes; touch nothing on disk.
    #[arg(long)]
    pub dry_run: bool,

    /// Remove every knack-authored shim across all known agents — both
    /// new-format meta-skill files and legacy per-skill shims left over
    /// from pre-0.4 installs.
    #[arg(long, conflicts_with_all = ["agent", "all_detected"])]
    pub purge: bool,

    /// Reserved. No-op in 0.4+. Retained for CLI stability.
    #[arg(long)]
    pub global: bool,
}

pub fn run(args: SyncArgs, _client_config: Config, mode: OutputMode) -> CliResult<()> {
    if args.purge {
        let report = purge_all(args.dry_run);
        emit_report(mode, &report, args.dry_run);
        return Ok(());
    }

    chatter(
        mode,
        "knack sync is a no-op in the meta-skill model. \
         Use `knack list` to discover skills; \
         run `knack sync --purge` to clean up legacy per-skill shims.",
    );
    emit_ok(
        mode,
        json!({"written": [], "up_to_date": [], "removed": [], "skipped": []}),
        || {},
    );
    Ok(())
}

/// No-op in the meta-skill model. Pre-0.4 this wrote per-skill shims
/// into each installed agent's native discovery directory after every
/// pull / publish; 0.4 dropped that — `knack list` is the runtime
/// discovery mechanism now. Returns an empty [`SyncReport`] so
/// `pull` / `publish` call sites keep emitting their `shims: []` field
/// without special-casing.
pub fn sync_one_skill(_slug: &str, _scope: Scope, _client_config: &Config) -> SyncReport {
    SyncReport::default()
}

fn purge_all(dry_run: bool) -> SyncReport {
    let mut report = SyncReport::default();
    for entry in installed::list().unwrap_or_default() {
        let Some(target) = targets::find(&entry.slug) else {
            continue;
        };
        let Some(root) = (target.shim_root)(entry.scope) else {
            continue;
        };
        if dry_run {
            report.removed.push(ShimResult {
                agent: entry.slug.clone(),
                path: root.display().to_string(),
                status: "would_purge".into(),
                reason: None,
            });
            continue;
        }
        match shim::remove_all_shims(target, &root) {
            Ok(n) => {
                if n > 0 {
                    report.removed.push(ShimResult {
                        agent: entry.slug.clone(),
                        path: root.display().to_string(),
                        status: format!("purged_{n}"),
                        reason: None,
                    });
                }
            }
            Err(e) => report.skipped.push(ShimResult {
                agent: entry.slug,
                path: root.display().to_string(),
                status: "skipped".into(),
                reason: Some(e.to_string()),
            }),
        }
    }
    report
}

#[derive(Debug, Clone, Serialize)]
pub struct ShimResult {
    pub agent: String,
    pub path: String,
    pub status: String,
    pub reason: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct SyncReport {
    pub written: Vec<ShimResult>,
    pub up_to_date: Vec<ShimResult>,
    pub removed: Vec<ShimResult>,
    pub skipped: Vec<ShimResult>,
}

fn emit_report(mode: OutputMode, report: &SyncReport, dry_run: bool) {
    if mode.json {
        emit_ok(
            mode,
            json!({
                "dry_run": dry_run,
                "written": report.written,
                "up_to_date": report.up_to_date,
                "removed": report.removed,
                "skipped": report.skipped,
            }),
            || {},
        );
        return;
    }
    if mode.quiet {
        return;
    }
    if dry_run {
        println!("[dry-run]");
    }
    for r in &report.written {
        println!("Wrote   {:<10} → {}", r.agent, r.path);
    }
    for r in &report.up_to_date {
        println!("Skipped {:<10} (up to date) {}", r.agent, r.path);
    }
    for r in &report.removed {
        println!("Removed {:<10} {}", r.agent, r.path);
    }
    for r in &report.skipped {
        println!(
            "Skipped {:<10} ({}) {}",
            r.agent,
            r.reason.as_deref().unwrap_or("?"),
            r.path
        );
    }
    if report.written.is_empty()
        && report.up_to_date.is_empty()
        && report.removed.is_empty()
        && report.skipped.is_empty()
    {
        println!("Nothing to sync.");
    }
}
