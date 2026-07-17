## Commands

The CLI is a thin skill-store client. Authoring the SKILL.md folder happens
in chat with your agent (see `agent-integration` topic); the CLI publishes
the result. Knack runs in one of two modes, picked once at `knack init`:
self-host (your GitHub repo) or Knack Cloud. Most commands work the same
in both modes; the few that don't are flagged below.

### Setup

    knack init                                              # interactive picker (TTY only)
    knack init --self-host --github-repo <you>/<repo> \     # github mode, non-interactive
        [--visibility private|public] [--local-path DIR]
    knack init --cloud                                       # cloud mode, non-interactive

### Auth

    knack auth login    [--no-browser] [--account NAME] \
                        [--expires-in-days N | --never-expires] \
                        [--scope full|read] [--label LBL]
        # cloud: device flow → mints a PAT, default 90d TTL
        #        --never-expires opts out (use only for unattended CI + vaults)
        #        --scope read mints a token denied at write routes (with a small
        #        allowlist: mark, feedback, revoke-own-token)
        # github: probes `gh api user`, reports gh state (no Knack token minted)
    knack auth logout   [--account NAME]
    knack auth status                                        # mode-aware: shows backend + identity
    knack auth refresh                                       # cloud only; no-op in github mode

### Authoring

    knack create <slug> --name "..." [--description "..."] [--scope personal|team|public]
        # --scope / --team-id are cloud only; github mode ignores them
        # (every github-mode skill lives in the repo's `skills/` root)
    knack validate <slug>
    knack publish <slug> [--from DIR] [--major|--minor|--patch] [--as-version X.Y.Z] [--dry-run]
        # source folder: --from wins; otherwise .knack/drafts/<slug> →
        # .knack/skills/<slug> → ~/.knack/skills/<slug> → (github mode)
        # the registry clone's skills/<slug>. Both modes ship whatever
        # that folder holds. github: syncs the folder into the registry,
        # commits, tags <slug>/vX.Y.Z, verifies the tag byte-matches the
        # source, then pushes. --dry-run prints the file manifest in
        # both modes without touching anything.
    knack edit <slug> --name "..." --description "..." --scope ...        # cloud only

### Discovery + consumption

    knack list                                               # github: walks local clone; cloud: API
    knack search <query> [--sort recent|top|trending]        # github: local grep; cloud: marketplace
    knack pull <slug>[@<semver>] [--target DIR] [--global]
    knack pull @<owner>/<slug>[@<ver>]                       # external GitHub via Contents API
    knack pull @<owner>/<repo>:<slug>[@<ver>]                # external with custom repo
    knack export [--to DIR] [--scope SCOPE] [--limit N]      # cloud: bulk pull entire library
                                                              # github: prints local skills/ path
    knack diff <slug>@<a> <slug>@<b>                         # two published versions
    knack diff <slug>@<a> <local-dir>                        # published vs local folder
        # both modes; github resolves versions from the <slug>/v<ver> tags.
        # compares SKILL.md, meta.knack.yaml (+ legacy intuition.md)

### Running + telemetry

    knack run <slug>[@<semver>] [--input PATH]... [--runtime TAG] \
                                [--agent-id ID] [--no-push]
    knack mark <run_id>[,<run_id>...] succeeded|failed \
                                [--note "..."] [--reason "..."] \
                                [--output PATH]... [--no-push]

`--input` and `--output` are repeatable. `mark` accepts a comma-separated
list of run-ids to verdict several runs in one call; the same `--note`
and `--output` apply to every id.

In github mode every `run` and `mark` auto-commits the affected JSONL
day-file and pushes to the repo's default remote/branch (commit message:
`telemetry: <event> <skill> <run_id>`). The local append always succeeds
even when the push fails (offline, branch diverged); a stderr warning
tells you how to recover, and the next successful command carries the
queued commits.

The push target is resolved per-repo: `KNACK_REMOTE_NAME` /
`KNACK_REMOTE_BRANCH` env vars override; otherwise `git symbolic-ref
refs/remotes/<remote>/HEAD` is consulted, then `gh repo view`, then the
`origin/main` fallback. `master`-default repos and fork workflows work
without any configuration.

Three opt-outs for the auto-push (local commit still lands either way):

    knack run --no-push                    # per-invocation
    KNACK_AUTO_PUSH=0 knack run ...        # per-shell env
    # or in <repo>/knack.yaml:
    auto_push: false                        # persistent per-workspace

### Analyzing runs

    knack runs overview [--since DATE] [--until DATE] [--min-runs N] \
                        [--team <slug-or-id>]
    knack runs list <slug> [--status STATUS] [--version V] [--agent TAG] \
                           [--since DATE] [--until DATE] [--note-contains TEXT] \
                           [--limit N] [--cursor C]
    knack runs show <run-id>
    knack runs stats <slug> [--group-by version|agent|version,agent] \
                            [--since DATE] [--until DATE]
    knack runs trend <slug> [--interval day|week] \
                            [--group-by DIMS] [--since DATE] [--until DATE]
    knack runs diff <slug> <ver-a> <ver-b> [--since DATE] [--until DATE]

`--since` / `--until` accept `YYYY-MM-DD` or `<N>d` (e.g. `7d` = seven
days back). Default window for `runs list` / `stats` / `trend`: 30 days
back to today. `knack mark`'s parent-run resolver walks every monthly
JSONL directory in self-host mode (not capped at 30 days), so a mark
weeks after `started` still finds its parent run.

`overview` is the portfolio dashboard — one row per skill the caller can
read, with `regression` and `stale` flags. Default first call when an
agent loads without a specific slug in mind. The `regression` field is
**suppressed when either cohort has fewer than 3 marked runs**
(`MIN_REGRESSION_RUNS = 3`), so `regression: null` doesn't necessarily
mean "no problem" — cross-check with `runs stats --group-by version`.

`stats` groups by one or more dimensions (`version`, `agent`, or both).
Each bucket carries `key`, `runs_total`, `runs_succeeded`, `runs_failed`,
`success_rate`, `p50_ms`, `p95_ms`, `last_run_at`, and up to 3 top
failure notes. Notes are clustered on a normalized form (lowercase +
collapsed whitespace + stripped trailing punctuation), so cosmetic
variants like "Edge case BROKE" / "edge case broke ." aggregate to one
entry; the original first-seen casing is preserved for display.

`trend` is the time axis: daily or weekly buckets. Every period in the
window emits a point — empty ones carry `buckets: []` — so the series is
gap-free and plot-ready.

`diff` is two versions side-by-side; `delta` is `null` when either side
has zero runs.

Both modes return the same schema. Self-host aggregates JSONL locally;
cloud calls `/skills/{id}/stats?group_by=...`, `/skills/{id}/stats/trend`,
or `/runs/overview`.

### Interview (agent-driven authoring)

    knack interview start                                    # drops the interview skill into .claude/skills/
    knack interview save --session <id> --phase <p> --data <json>
    knack interview advance --session <id>
    knack interview status --session <id>
    knack interview resume --session <id>

### Folders, social, agent registration

    knack folder create|list|rename|delete|mv ...           # cloud only (personal + team scopes)
    knack feedback ...                                       # cloud only
    knack team ...                                           # cloud only
    knack fork <slug>                                        # cloud only
    knack rate <slug> <1-5>                                  # cloud only
    knack username claim <handle>                            # cloud only
    knack install [<agent>|--auto|--all]                     # register knack with the local agent runtime
    knack uninstall [--script]
    knack upgrade
    knack sync <slug>                                        # refresh per-skill agent shims

### Meta

    knack docs [<topic>]                                     # offline docs
    knack info                                               # canonical agent playbook
    knack introspect                                         # machine-readable command tree
    knack completions <shell>
    knack debug

Every command supports: `--json`, `--quiet`, `--no-color`, `--auth-token`,
`--account`.

Folders organize personal and team skills only. Folders are optional —
unfiled is a valid steady state — and every operation (create, rename,
move, delete) is reversible. The web workspace (Skill → Settings →
Folder section, plus the sidebar Folders list) and the CLI hit the
same `/folders` and `PATCH /skills/{id}` endpoints, so changes from
either surface appear in the other on next read.

### Typical agent-driven flow

    # one-time
    knack auth login

    # author the skill folder in chat with your agent (SKILL.md with
    # ## Intuition section, optional scripts/, assets/, references/), then:
    knack create month-end-close --name "Month-end close"
    knack publish month-end-close --from ./month-end-close

    # iterate
    knack pull month-end-close
    # edit files
    knack publish month-end-close            # auto-bumps patch

    # use the skill on real work
    knack run month-end-close --input ./october.xlsx
    knack mark <run_id> succeeded
