//! Agent integration targets.
//!
//! Each entry maps a coding-agent runtime to the file `knack install` should
//! write to. Adding a new agent is a single struct literal: declare the env
//! marker that proves it's running (or `&[]` if there isn't one), the binary
//! name to look for on PATH, the config-path resolver, and the write style.
//!
//! Ordering matters: `autodetect()` walks this list and returns the first
//! match, so more-specific agents (Claude Code, Codex) come before
//! "everything that writes AGENTS.md" generics.

use std::path::PathBuf;

use super::installed::Scope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigStyle {
    /// Append a delimited markdown block to the file. Idempotent: re-runs
    /// splice in place; original surrounding content is preserved.
    AppendBlock,
    /// Write the whole file (used for Cursor `.mdc` rules which need
    /// frontmatter and are owned end-to-end by knack).
    WriteFile,
}

/// How per-skill shims for this target are written.
///
/// Distinct from [`ConfigStyle`] — that one governs the one-time install
/// block (`knack exists, run knack info`). Shims are the per-pulled-skill
/// registrations that make the runtime's native discovery surface the
/// skill on session start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimStyle {
    /// Runtime natively reads Anthropic Skills (Claude Code, Codex,
    /// Cline, Kiro, Windsurf, Trae, Gemini CLI, OpenCode, Factory, Amp,
    /// and Cowork as of May 2026). One folder per skill at
    /// `<root>/<slug>/SKILL.md` with frontmatter.
    NativeSkill,
    /// Runtime natively reads rule files keyed by description (Cursor).
    /// One file per skill at `<root>/knack-<slug>.mdc`.
    NativeRule,
    /// Runtime reads a free-form context file (AGENTS.md, CONVENTIONS.md).
    /// We splice a small per-skill block into that file.
    TextBlock,
    /// Runtime has no useful discovery mechanism for individual skills.
    /// `generic` is the only target that uses this — the install block
    /// telling the agent "knack exists" is the best we can do.
    None,
}

pub struct AgentTarget {
    /// Slug accepted by `knack install <name>` and reported in output.
    pub name: &'static str,
    /// Human-readable label.
    pub display: &'static str,
    /// Environment variables that prove this agent is currently running.
    /// Any of them being set is sufficient.
    pub env_markers: &'static [&'static str],
    /// Binaries that, if present on PATH, indicate this agent is installed.
    pub binary_markers: &'static [&'static str],
    /// Where to write the install block. Returns `None` if the OS doesn't
    /// have a reasonable destination (e.g. no home directory).
    pub config_path: fn() -> Option<PathBuf>,
    /// How to write the install block.
    pub style: ConfigStyle,
    /// How to write per-skill shims.
    pub shim_style: ShimStyle,
    /// Where per-skill shims live, scoped to home (HOME-shared pool) vs
    /// project (workspace-local). Returns `None` when this combination
    /// doesn't make sense (e.g. `generic` has no native shim story; some
    /// project-only runtimes return `None` for `Home`).
    pub shim_root: fn(Scope) -> Option<PathBuf>,
}

/// Ordered registry. Autodetect walks this list top-down; first env hit wins,
/// then first binary hit wins. `generic` is always last and is added as a
/// safety net by the caller (excluded from autodetect itself).
pub static TARGETS: &[AgentTarget] = &[
    AgentTarget {
        name: "claude",
        display: "Claude Code / Claude Cowork",
        // CLAUDECODE=1 is set by Claude Code on every shell it spawns;
        // CLAUDE_CODE_IS_COWORK is the internal flag Cowork sets to enable
        // eager-flush behavior. Cowork shares the same ~/.claude/CLAUDE.md
        // context system, so one target covers both.
        env_markers: &["CLAUDECODE", "CLAUDE_CODE_IS_COWORK"],
        binary_markers: &["claude"],
        config_path: claude_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::NativeSkill,
        shim_root: claude_shim_root,
    },
    AgentTarget {
        name: "codex",
        display: "OpenAI Codex CLI",
        env_markers: &["CODEX_HOME"],
        binary_markers: &["codex"],
        config_path: codex_path,
        style: ConfigStyle::AppendBlock,
        // Codex reads `.agents/skills/` (workspace) and
        // `$HOME/.agents/skills/` (user), not `.codex/skills/`. See
        // developers.openai.com/codex/skills.
        shim_style: ShimStyle::NativeSkill,
        shim_root: codex_shim_root,
    },
    AgentTarget {
        name: "cursor",
        display: "Cursor",
        env_markers: &["CURSOR_TRACE_ID"],
        binary_markers: &["cursor"],
        config_path: cursor_path,
        style: ConfigStyle::WriteFile,
        // Cursor has native rule discovery (`.cursor/rules/*.mdc`); per
        // confirmed choice we ship one .mdc per skill. Cursor 2.4 also
        // reads `.cursor/skills/` natively, but it also reads
        // `.claude/skills/` and `.agents/skills/` for compat, so the
        // claude+codex NativeSkill writes already cover the skill folder
        // path. The .mdc keeps the description-match rule UX intact.
        shim_style: ShimStyle::NativeRule,
        shim_root: cursor_shim_root,
    },
    AgentTarget {
        name: "windsurf",
        display: "Windsurf (Cascade)",
        env_markers: &[],
        binary_markers: &["windsurf"],
        config_path: windsurf_path,
        style: ConfigStyle::AppendBlock,
        // Native SKILL.md at `.windsurf/skills/` since 2026-03-09.
        shim_style: ShimStyle::NativeSkill,
        shim_root: windsurf_shim_root,
    },
    AgentTarget {
        name: "cline",
        // Cline is a VS Code extension; no env or binary marker on the
        // host shell. Users invoke `knack install cline` manually. We write
        // a dedicated file under .clinerules/ which Cline auto-loads.
        display: "Cline",
        env_markers: &[],
        binary_markers: &[],
        config_path: cline_path,
        style: ConfigStyle::AppendBlock,
        // Native SKILL.md at `.cline/skills/` (workspace) and
        // `~/.cline/skills/` (global) since Cline 3.48.
        shim_style: ShimStyle::NativeSkill,
        shim_root: cline_shim_root,
    },
    AgentTarget {
        name: "continue",
        // Continue.dev: same install story as Cline. No native SKILL.md
        // support as of May 2026 — rules-only — so we stay TextBlock.
        display: "Continue.dev",
        env_markers: &[],
        binary_markers: &[],
        config_path: continue_path,
        style: ConfigStyle::AppendBlock,
        shim_style: ShimStyle::TextBlock,
        shim_root: continue_shim_root,
    },
    AgentTarget {
        name: "kiro",
        display: "Kiro (AWS)",
        env_markers: &[],
        binary_markers: &["kiro"],
        config_path: kiro_path,
        style: ConfigStyle::AppendBlock,
        // Native SKILL.md at `~/.kiro/skills/` / `.kiro/skills/` since
        // Kiro 0.9.
        shim_style: ShimStyle::NativeSkill,
        shim_root: kiro_shim_root,
    },
    AgentTarget {
        name: "trae",
        display: "Trae (ByteDance)",
        env_markers: &[],
        binary_markers: &["trae"],
        config_path: trae_path,
        style: ConfigStyle::AppendBlock,
        // Native SKILL.md at `.trae/skills/`.
        shim_style: ShimStyle::NativeSkill,
        shim_root: trae_shim_root,
    },
    AgentTarget {
        name: "aider",
        display: "Aider",
        env_markers: &[],
        binary_markers: &["aider"],
        config_path: aider_path,
        style: ConfigStyle::AppendBlock,
        // Aider has no native SKILL.md (only the third-party `aider-skills`
        // package, not in mainline as of May 2026).
        shim_style: ShimStyle::TextBlock,
        shim_root: aider_shim_root,
    },
    AgentTarget {
        name: "gemini",
        display: "Gemini CLI",
        env_markers: &["GEMINI_CLI"],
        binary_markers: &["gemini"],
        config_path: gemini_path,
        style: ConfigStyle::AppendBlock,
        // Native SKILL.md since Gemini CLI v0.25 (2026-01-20). Reads both
        // `~/.gemini/skills/` and `~/.agents/skills/`; we target the
        // cross-agent path so a single write services Codex + Gemini + Amp.
        shim_style: ShimStyle::NativeSkill,
        shim_root: gemini_shim_root,
    },
    AgentTarget {
        name: "opencode",
        display: "OpenCode",
        env_markers: &[],
        binary_markers: &["opencode"],
        config_path: opencode_path,
        style: ConfigStyle::AppendBlock,
        // Native SKILL.md at `.opencode/skills/` (workspace) and
        // `~/.config/opencode/skills/` (user).
        shim_style: ShimStyle::NativeSkill,
        shim_root: opencode_shim_root,
    },
    AgentTarget {
        name: "factory",
        display: "Factory droid",
        env_markers: &["FACTORY_DROID"],
        binary_markers: &["droid"],
        config_path: factory_path,
        style: ConfigStyle::AppendBlock,
        // Native SKILL.md at `.factory/skills/` (workspace) and
        // `~/.factory/skills/` (personal).
        shim_style: ShimStyle::NativeSkill,
        shim_root: factory_shim_root,
    },
    AgentTarget {
        name: "amp",
        display: "Amp",
        env_markers: &[],
        binary_markers: &["amp"],
        config_path: amp_path,
        style: ConfigStyle::AppendBlock,
        // Native SKILL.md at `.agents/skills/` (workspace) and
        // `~/.config/agents/skills/` (user).
        shim_style: ShimStyle::NativeSkill,
        shim_root: amp_shim_root,
    },
    AgentTarget {
        name: "generic",
        display: "Generic AGENTS.md",
        env_markers: &[],
        binary_markers: &[],
        config_path: generic_path,
        style: ConfigStyle::AppendBlock,
        // `generic` is the AGENTS.md fallback — there's no specific
        // runtime to register with, so the install block is the entire
        // surface. Per-skill shims would just duplicate it.
        shim_style: ShimStyle::None,
        shim_root: |_| None,
    },
];

pub fn find(name: &str) -> Option<&'static AgentTarget> {
    TARGETS.iter().find(|t| t.name == name)
}

pub fn names() -> Vec<&'static str> {
    TARGETS.iter().map(|t| t.name).collect()
}

// ─── HOME / config overrides ───────────────────────────────────────────────
//
// `dirs::home_dir()` and `dirs::config_dir()` return real machine paths.
// For end-to-end shim tests we need to redirect those at the temp dir so
// a test run never clobbers `~/.agents/skills/` or `~/.config/opencode/`.
// `KNACK_TEST_HOME` / `KNACK_TEST_CONFIG` are recognized only when set.
// They are not documented for users — they exist for the test harness.

fn home_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("KNACK_TEST_HOME") {
        return Some(PathBuf::from(d));
    }
    dirs::home_dir()
}

fn config_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("KNACK_TEST_CONFIG") {
        return Some(PathBuf::from(d));
    }
    dirs::config_dir()
}

// ─── Config-path resolvers ─────────────────────────────────────────────────

fn claude_path() -> Option<PathBuf> {
    // Claude Code honors $CLAUDE_CONFIG_DIR; default is ~/.claude.
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("CLAUDE.md"));
    }
    home_dir().map(|h| h.join(".claude").join("CLAUDE.md"))
}

fn codex_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CODEX_HOME") {
        return Some(PathBuf::from(dir).join("AGENTS.md"));
    }
    home_dir().map(|h| h.join(".codex").join("AGENTS.md"))
}

// Every config_path resolver below is HOME-anchored. The whole table
// is global-by-default: one paste of `knack install` registers Knack
// once for the user, and the per-skill shims land in whichever scope
// `knack pull` runs at (workspace if invoked inside a workspace, HOME
// otherwise). For a handful of agents (cursor, windsurf, trae, amp)
// the HOME path is best-effort — those agents don't have a documented
// HOME load location, and a non-technical user pasting our install
// will get a marker file in the conventional place but may need to
// flick a setting in the agent UI to actually pick it up. We prefer
// "file appears at the expected path" to "silently does nothing."

fn cursor_path() -> Option<PathBuf> {
    // Cursor's user rules are configured via Settings → Rules for User
    // (no documented load file), so HOME is best-effort. We write to
    // ~/.cursor/rules/knack.mdc so the file exists for the user to
    // paste/import into their settings.
    home_dir().map(|h| h.join(".cursor").join("rules").join("knack.mdc"))
}

fn aider_path() -> Option<PathBuf> {
    // Aider has no auto-load from HOME (issue #3433). We write
    // ~/CONVENTIONS.md so the file is present; users add
    // `read: ~/CONVENTIONS.md` to ~/.aider.conf.yml for auto-pickup.
    home_dir().map(|h| h.join("CONVENTIONS.md"))
}

fn windsurf_path() -> Option<PathBuf> {
    // Windsurf's documented user-scope rules file (codeium issue #157).
    home_dir().map(|h| {
        h.join(".codeium")
            .join("windsurf")
            .join("memories")
            .join("global_rules.md")
    })
}

fn cline_path() -> Option<PathBuf> {
    // Cline auto-loads ~/.cline/rules/ globally.
    home_dir().map(|h| h.join(".cline").join("rules").join("knack.md"))
}

fn continue_path() -> Option<PathBuf> {
    // Continue.dev auto-loads ~/.continue/rules/ globally.
    home_dir().map(|h| h.join(".continue").join("rules").join("knack.md"))
}

fn kiro_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".kiro").join("steering").join("AGENTS.md"))
}

fn trae_path() -> Option<PathBuf> {
    // Trae's documented personal rules file. No formal "global" Trae
    // location exists; this is the closest equivalent.
    home_dir().map(|h| h.join(".trae").join("rules").join("user_rules.md"))
}

fn gemini_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".gemini").join("GEMINI.md"))
}

fn opencode_path() -> Option<PathBuf> {
    // OpenCode personal config: $OPENCODE_CONFIG_DIR or ~/.config/opencode/.
    config_dir().map(|c| c.join("opencode").join("AGENTS.md"))
}

fn factory_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".factory").join("AGENTS.md"))
}

fn amp_path() -> Option<PathBuf> {
    // Amp has no documented HOME AGENTS.md, but ~/.config/agents/ is the
    // user-scope Amp directory (matches the skills/ sibling).
    config_dir().map(|c| c.join("agents").join("AGENTS.md"))
}

fn generic_path() -> Option<PathBuf> {
    // The agents.md proposed standard: $XDG_CONFIG_HOME/agents/AGENTS.md or
    // %APPDATA%\agents\AGENTS.md.
    config_dir().map(|c| c.join("agents").join("AGENTS.md"))
}

// ─── Shim-root resolvers ───────────────────────────────────────────────────
//
// For NativeSkill / NativeRule styles, the shim root is a *directory* under
// which we write one folder (or .mdc file) per pulled skill. For TextBlock
// targets the "root" is the same file we already wrote the install block
// into — shims are sentinel-bracketed blocks appended after the install
// block. Both home and project scopes are supported where meaningful.

fn claude_shim_root(scope: Scope) -> Option<PathBuf> {
    let dir = match scope {
        Scope::Home => {
            if let Ok(d) = std::env::var("CLAUDE_CONFIG_DIR") {
                PathBuf::from(d).join("skills")
            } else {
                home_dir()?.join(".claude").join("skills")
            }
        }
        // Workspace: Claude Code also scans <cwd>/.claude/skills/ for
        // project-scoped tools. CWD is the right anchor — the shim
        // writer is invoked from the same process as `knack pull`, which
        // already resolved its workspace via discovery.
        Scope::Project => std::env::current_dir().ok()?.join(".claude").join("skills"),
    };
    Some(dir)
}

fn codex_shim_root(scope: Scope) -> Option<PathBuf> {
    // Codex skill paths per developers.openai.com/codex/skills:
    //   $CWD/.agents/skills, $REPO_ROOT/.agents/skills,
    //   $HOME/.agents/skills, /etc/codex/skills.
    // Note: NOT ~/.codex/skills/ — that path does not exist for Codex.
    let dir = match scope {
        Scope::Home => home_dir()?.join(".agents").join("skills"),
        Scope::Project => std::env::current_dir().ok()?.join(".agents").join("skills"),
    };
    Some(dir)
}

fn cursor_shim_root(scope: Scope) -> Option<PathBuf> {
    // Cursor's HOME rules-on-disk path is undocumented; we still write to
    // ~/.cursor/rules/ so per-skill .mdc files exist for the user to
    // paste/import into Cursor settings.
    let dir = match scope {
        Scope::Home => home_dir()?.join(".cursor").join("rules"),
        Scope::Project => std::env::current_dir().ok()?.join(".cursor").join("rules"),
    };
    Some(dir)
}

fn windsurf_shim_root(scope: Scope) -> Option<PathBuf> {
    // Windsurf added .windsurf/skills/ on 2026-03-09. No documented
    // HOME-scope skill path; we write to ~/.windsurf/skills/ as a
    // conventional best-effort.
    let dir = match scope {
        Scope::Home => home_dir()?.join(".windsurf").join("skills"),
        Scope::Project => std::env::current_dir()
            .ok()?
            .join(".windsurf")
            .join("skills"),
    };
    Some(dir)
}

fn cline_shim_root(scope: Scope) -> Option<PathBuf> {
    // Cline 3.48 reads .cline/skills/ workspace + ~/.cline/skills/ global.
    let dir = match scope {
        Scope::Home => home_dir()?.join(".cline").join("skills"),
        Scope::Project => std::env::current_dir().ok()?.join(".cline").join("skills"),
    };
    Some(dir)
}

fn kiro_shim_root(scope: Scope) -> Option<PathBuf> {
    // Kiro 0.9: ~/.kiro/skills/ global, .kiro/skills/ workspace.
    let dir = match scope {
        Scope::Home => home_dir()?.join(".kiro").join("skills"),
        Scope::Project => std::env::current_dir().ok()?.join(".kiro").join("skills"),
    };
    Some(dir)
}

fn trae_shim_root(scope: Scope) -> Option<PathBuf> {
    // Trae: .trae/skills/ per docs.trae.ai/ide/skills. No documented
    // HOME-scope skill path; we write to ~/.trae/skills/ as a best-effort.
    let dir = match scope {
        Scope::Home => home_dir()?.join(".trae").join("skills"),
        Scope::Project => std::env::current_dir().ok()?.join(".trae").join("skills"),
    };
    Some(dir)
}

fn gemini_shim_root(scope: Scope) -> Option<PathBuf> {
    // Gemini CLI reads both ~/.gemini/skills/ and ~/.agents/skills/. We
    // pick ~/.gemini/skills/ as the unambiguous Gemini-specific location
    // (the cross-agent ~/.agents/skills/ is already covered by the codex
    // shim, so a user with both installed gets one write per location).
    let dir = match scope {
        Scope::Home => home_dir()?.join(".gemini").join("skills"),
        Scope::Project => std::env::current_dir().ok()?.join(".gemini").join("skills"),
    };
    Some(dir)
}

fn opencode_shim_root(scope: Scope) -> Option<PathBuf> {
    // OpenCode: .opencode/skills/ workspace, ~/.config/opencode/skills/ user.
    let dir = match scope {
        Scope::Home => config_dir()?.join("opencode").join("skills"),
        Scope::Project => std::env::current_dir()
            .ok()?
            .join(".opencode")
            .join("skills"),
    };
    Some(dir)
}

fn factory_shim_root(scope: Scope) -> Option<PathBuf> {
    // Factory droid: ~/.factory/skills/ personal, .factory/skills/ workspace.
    let dir = match scope {
        Scope::Home => home_dir()?.join(".factory").join("skills"),
        Scope::Project => std::env::current_dir()
            .ok()?
            .join(".factory")
            .join("skills"),
    };
    Some(dir)
}

fn amp_shim_root(scope: Scope) -> Option<PathBuf> {
    // Amp: .agents/skills/ workspace, ~/.config/agents/skills/ user. The
    // workspace path overlaps with codex's; same content written twice is
    // idempotent (sigil + same body) so no harm.
    let dir = match scope {
        Scope::Home => config_dir()?.join("agents").join("skills"),
        Scope::Project => std::env::current_dir().ok()?.join(".agents").join("skills"),
    };
    Some(dir)
}

// TextBlock targets: shim root == the install-block file itself. The
// shim writer for TextBlock splices `<!-- knack:skill:<slug>:start -->`
// pairs into that file. We surface the file path here so shim sync
// has a single uniform interface.

fn continue_shim_root(_: Scope) -> Option<PathBuf> {
    continue_path()
}
fn aider_shim_root(_: Scope) -> Option<PathBuf> {
    aider_path()
}
