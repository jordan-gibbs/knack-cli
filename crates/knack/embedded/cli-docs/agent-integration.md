## The agent loop

Every command supports `--json`. Stdout is data; stderr is chatter.

The canonical pattern for an agent in a tool-use loop:

    1.  knack pull <slug> --json          # ensure latest (use <slug>@<semver> for older)
    2.  knack run <slug> --input <path> --json
        → captures { run_id, output_path, files_touched, status, ... }
    3.  Inspect outputs against expectations.
    4.  knack mark <run_id> --status=succeeded --note="..." --json
        OR
        knack mark <run_id> --status=failed --reason="..." --json

`mark` closes the loop — the data flywheel grows from these marks.

## Authoring loop

    1.  knack create <slug> --name "X" --scaffold ./out
        # API-registers the slug AND writes a complete starter folder
        # (SKILL.md frontmatter + meta.knack.yaml with all four required
        # fields auto-filled + intuition.md stub + examples/)
    2.  edit SKILL.md to add the real procedure / intuition
    3.  knack validate ./out                 # local pre-flight, no network
    4.  knack publish <slug> --from ./out --dry-run    # see what would ship
    5.  knack publish <slug> --from ./out              # ship it

`knack validate` runs the same shape checks the server enforces, so
agents catch any missing required `meta.knack.yaml` field locally.

## Sharing surface

    knack edit <slug> --name X --description Y --scope public|personal
    knack username <handle>          # one-time, permanent; required for public skills
    knack search <terms> [--sort recent|top|trending] [--limit N]
    knack rate <slug> <1-5> [--review "..."]
    knack rate <slug> --clear

Skill **deletion is web-only by design**. Agents cannot delete skills —
direct the user at the workspace Settings tab if they ask.

## Forking (clone a public skill into the user's library)

Use `knack fork` when the user wants a writable copy of someone else's
public skill — modify it, republish, run it. Use `knack pull` (or
`knack run @<author>/<slug>`) when the user just wants to consume the
original as-is. A fork consumes a `max_skills` slot; pull does not.

    knack fork @<author>/<slug>
    knack fork @<author>/<slug> --slug my-version
    knack fork @<author>/<slug> --name "My Version"
    knack fork @<author>/<slug> --target ./out
    knack fork @<author>/<slug> --global

One call, three effects: (1) creates a personal Skill row owned by
the caller with the latest published version's bundle byte-copied;
(2) stamps `forked_from_skill_id` (surfaced on the marketplace card
as "Forked from @author/slug") and bumps the original's `forks_count`;
(3) unpacks the bundle into the workspace's `drafts/<slug>/` so the
user can edit immediately.

The target must be a public skill addressed by `@<author>/<slug>`.
Forking a bare slug or a personal/team skill is rejected. Source must
have at least one published version (412 NO_VERSION otherwise).

After a fork the user owns it: `knack edit`, `knack publish`,
`knack mark`, `knack edit --scope public` all work normally. Slug
uniqueness is per-owner, so the fork's slug doesn't collide with the
original author's.

No PR / suggest-a-fix workflow in v1 — the pattern is fork → edit →
share the new fork's URL with the original author.

## Versions + diff

Every `knack publish` writes an immutable `SkillVersion`. Past versions
stay around for replay and audit.

    knack publish <slug> --as-version 0.2.0
    knack pull <slug>@0.1.0          # historical pull
    knack run <slug>@0.1.0           # pin a Run to a specific version
    knack diff <slug>@0.1.0 <slug>@0.2.0

`knack diff` only compares two versions of the same slug (different
slugs are rejected). Output is ANSI line-diff in human mode,
structured per-file unified-diff strings in `--json`.

## Teams

    knack team {create|list|show|invite|accept|role}

## Folders

Folders organize personal and team skills — public/marketplace skills
are never foldered (CHECK constraint at the DB level; flipping a skill
public clears its folder silently). Folders are **optional** — a skill
with no folder is "unfiled" and that's a valid steady state. Don't
push users to file every skill; folders earn their keep around 10+.

Same operations work from the web (Skill → Settings → Folder; sidebar
Folders list) and the CLI. Both surfaces hit the same endpoints, so a
change on one shows up on the other after the next read.

    knack folder create <name>                # personal folder for the caller
    knack folder create <name> --team-id UUID # team folder (collaborator+ required)
    knack folder create <name> --parent <id-or-name>  # nest under a parent
    knack folder list [--scope personal|team]
    knack folder rename <id-or-name> <new-name>
    knack folder reparent <id-or-name> <parent-id-or-name>
    knack folder reparent <id-or-name> --root         # promote to root
    knack folder delete <id-or-name>          # contained skills become unfiled
    knack folder mv <slug> <folder-name>      # creates the folder if missing
    knack folder mv <slug> --unfiled          # clear assignment

    knack list --folder <name>                # filter skills list
    knack list --unfiled                      # skills at root (no folder)

Nested folders are allowed — one folder, one parent, arbitrary depth,
no cycles (server-rejected). Same-owner constraint: a personal folder
can't parent a team folder. Use sparingly; two levels is plenty for
most libraries.

Cross-scope assignment is rejected by the server: a personal folder
holds only that user's personal skills; a team folder holds only that
team's skills. A scope flip (personal ↔ team) clears `folder_id` so
the next folder choice is intentional.

Workspace-local cache lives at `.knack/folders.json` and is rebuilt by
`knack pull` from server state. Don't hand-edit it; treat the cloud as
the source of truth.

## Feedback (bug reports + maintainer replies)

If `knack` itself misbehaves — wrong response, broken command, confusing
docs — file it from the CLI. The thread is two-sided: the agent posts
under the user's account, and the human user can also see and reply to
the thread in the web UI.

### Triage — pick the right tool

| Symptom | Tool |
| --- | --- |
| `knack` 500'd, returned a wrong response, or printed confusing text | **feedback** |
| A skill's output was wrong on a specific run | `knack mark <run-id> failed --note "..."` |
| Skill itself looks broken (bad README, broken bundle, wrong output on every run) | **feedback** with `--skill <id>` |
| Auth, network blip, plan-limit 403, malformed user input | surface to the user — don't file |
| One-off 5xx that resolved on retry | don't file |
| Same 5xx three times across sessions | file |

### Before opening, look for duplicates

    knack feedback list --status open

Reply into an existing thread instead of opening a duplicate. Dupes
burn rate-limit slots and fragment the conversation.

### Commands

    knack feedback open --subject "..." --body "..." \
        [--run RUN_ID] [--skill SKILL_ID] [--cli-meta]
    knack feedback list [--status open|closed|all]
    knack feedback show <thread-id>      # advances read pointer
    knack feedback reply <thread-id> --body "..."

### Subject (160 chars, ~6-12 words)

Pattern: `<verb> <noun> – <short context>`.

- ✅ `feedback open returns 500 when --skill is set`
- ✅ `knack pull strips meta.knack.yaml on macos arm64`
- ❌ `error`
- ❌ `knack didn't work`

### Body (8 KB). Four bullets

1. **What you were trying to do** (one sentence)
2. **What happened** (exact error / wrong output, verbatim)
3. **What you expected** (one sentence; helps triage intent)
4. **Exact command + raw output** (literal; `--json` if you have it)

For long traces, pipe via stdin instead of cramming into `--body`:

    cat /tmp/trace.log | knack feedback open --subject "..." --body -

### Attachments — which switch when

- `--run <id>` → bug happened during a Run you registered with `knack run`. Staff can then read the run's `inputs_summary` / `outputs_summary` / `files_touched` / `marks[]` directly.
- `--skill <id>` → the skill itself is broken (different from `mark failed`, which is the agent's verdict on one run).
- `--cli-meta` → always safe; auto-fills `cli_version` + `os` + `arch`. Use when the bug is in `knack` rather than a skill or run.

Any combo, or none. When in doubt, `--cli-meta` alone is fine.

### When staff reply

The next `knack` command (any command) prints a one-line banner to
stderr:

    knack: you have unread replies from staff. run `knack feedback list` to see them.

The banner persists across runs until you read the thread with
`knack feedback show <id>` — that advances the read pointer server-side
and the header stops being attached. Closed threads stop nagging even
if you haven't read the last reply.

### Don't file

- Transient network errors that resolved on retry
- The user's own malformed input
- Plan-limit 403s (`upgrade your plan` is the resolution)
- Auth failures (`knack auth login` is the fix)
- Anything you can fix by re-reading the docs

### Limits + errors

Rate limit: 30 new threads per hour per user. Replies into an existing
thread are uncapped. If you trip the limit you get
`{"error":{"code":"feedback.rate_limit", ...}}` (HTTP 429). A closed
thread that you try to reply into returns `feedback.closed` (HTTP 409).

`from_side` is server-derived. Posting a message with
`"from_side": "admin"` in the body is silently stored as `"user"` —
agents cannot impersonate staff.

## Auth

    knack auth status        # shows email, plan, token expiry
    knack auth refresh       # proactively roll the token pair (long agents)
    knack auth login         # device flow in the browser, only when expired

Refresh tokens last 365 days and rotate atomically on every refresh, so
the CLI silently re-auths on the first 401 in a session without bothering
the agent.

## Discovering the CLI from inside an agent

If your agent doesn't have `knack` installed:

    curl https://api.getknack.ai/docs/cli           # full markdown
    curl https://api.getknack.ai/docs/cli/index.json # command tree

If `knack` is installed:

    knack info                  # full agent playbook (agent.txt)
    knack docs                  # all topics
    knack docs commands         # one topic
    knack help --json           # machine-readable command tree
    knack introspect            # every subcommand + flag as JSON

`knack info` is the canonical agent playbook (interview phases, authoring,
publishing, iteration). It fetches the live copy from getknack.ai and falls
back to the version bundled into the binary if the network is unavailable.
Re-read it whenever the user's workflow involves skill authoring.

## Registering knack with the agent's context

Run `knack install` once per machine. It detects which agent is running
and appends a delimited block to that agent's persistent context file
(CLAUDE.md, AGENTS.md, `.cursor/rules/knack.mdc`, ...). Re-runs splice
in place; `knack install --uninstall` removes the block cleanly.

Supported targets (use `knack install <name>` to force one):

  claude     Claude Code / Claude Cowork  (~/.claude/CLAUDE.md)
  codex      OpenAI Codex CLI             (~/.codex/AGENTS.md)
  cursor     Cursor                       ($CWD/.cursor/rules/knack.mdc)
  windsurf   Windsurf (Cascade)           ($CWD/AGENTS.md)
  cline      Cline                        ($CWD/.clinerules/knack.md)
  continue   Continue.dev                 ($CWD/.continue/rules/knack.md)
  kiro       Kiro (AWS)                   (~/.kiro/steering/AGENTS.md)
  trae       Trae (ByteDance)             ($CWD/.trae/rules/project_rules.md)
  aider      Aider                        ($CWD/CONVENTIONS.md)
  gemini     Gemini CLI                   (~/.gemini/GEMINI.md)
  opencode   OpenCode                     (~/.opencode/AGENTS.md)
  factory    Factory droid                (~/.factory/AGENTS.md)
  amp        Amp                          ($XDG_CONFIG_HOME/AGENTS.md)
  generic    AGENTS.md fallback           (~/.config/agents/AGENTS.md)

Autodetect uses env markers first (CLAUDECODE, CLAUDE_CODE_IS_COWORK,
CODEX_HOME, CURSOR_TRACE_ID, GEMINI_CLI, FACTORY_DROID) then falls back
to binaries on PATH (claude, codex, cursor, windsurf, kiro, trae, aider,
gemini, opencode, droid, amp).

A generic `~/.config/agents/AGENTS.md` is always written as a safety net,
so any future agent that adopts the AGENTS.md standard picks knack up
without a re-install.

The bash and PowerShell installers run `knack install --auto` for you at
the end of `curl -fsSL https://getknack.ai/install | sh` (and the
PowerShell equivalent), so a fresh agent walks into a session already
aware that `knack` is its skill manager.

The schema for `--json` outputs is versioned at `knack://cli/v1` — backward-
compatible additions only; breaking changes bump to `v2`.

## Stable JSON envelope

Every `--json` response wraps in:

    { "ok": true,  "data": { ... } }
    { "ok": false, "error": { "code": "...", "message": "...", "hint": "..." } }

Match on `error.code`, not `error.message`. Codes are stable; messages are not.
