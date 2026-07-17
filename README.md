<img width="1600" height="800" alt="Hero (1)" src="https://github.com/user-attachments/assets/b4b9a465-6944-4eea-a381-8817f030931c" />

# Knack

An agent-driven CLI for authoring, validating, versioning, running, and observing agent skills.

Knack turns your clunky Skill.md files into managed workflows:
structured folders of instructions, examples, tests, and metadata that any
agent (Claude, Cursor, Codex, Cowork) can load. The artifact is
inspectable and immutable; every skill run is logged. When a skill
misses an edge case, you flag it, the skill gets a new rule, and the next
run is sharper than the last.

## Anatomy of a Knack Skill

```
skills/email-triage/
├── SKILL.md            # the playbook the agent loads — includes a `## Intuition`
│                       # section with `### Always` / `### Except when` /
│                       # `### Edge cases` subsections that hold every rule
├── meta.knack.yaml     # name, slug, author, version, description
├── examples/           # input/output pairs from real past work
├── scripts/            # optional helper scripts
├── assets/             # optional static files
├── references/         # optional reference material
└── tests/              # optional assertions that run pre-publish
```

This is compatible with the open [Anthropic Skills](https://github.com/anthropics/skills)
format, so it's portable across agent providers.

## Install

```bash
curl -fsSL https://getknack.ai/install | sh
```

Windows (PowerShell):

```powershell
irm https://getknack.ai/install.ps1 | iex
```

## Two modes, one CLI

**Self-host (GitHub).** Skills live in a private GitHub repo under your
account. No Knack account, no Knack server, no telemetry leaves your
machine, no caps. Publishing is a git commit, a per-skill tag, and a push.

Prereqs: `git` on PATH, [GitHub CLI](https://cli.github.com) on PATH, and
`gh auth login` already run. `knack init --self-host` shells out to `gh`
to create the repo and to `git` to commit/push; it will not authenticate
either tool for you.

```bash
knack init --self-host --github-repo <you>/<your-skills-repo>
```

**Knack Cloud.** Zero setup. Public marketplace and team features
(sharing, roles, audit log, SSO) live here. Free tier at
[getknack.ai](https://getknack.ai).

```bash
knack init --cloud
knack auth login                       # 90-day PAT by default
knack auth login --scope read          # read-only PAT for CI
knack auth login --never-expires       # no-expiry, vault-only
```

Same commands. Same skill folder format. The backend just decides where
versions and run logs go. Pick once at `knack init`; switch later by
re-running it.

Read-scoped PATs (`--scope read`) authenticate on GETs but are denied at
write routes with a small allowlist for `knack mark`, feedback posts,
and revoking themselves. Useful when a CI agent only needs to poll
telemetry.

## The iteration loop

1. Author. `knack create my-skill` scaffolds. Your agent runs the four-phase
   interview (Genesis, Artifacts, Intuition, Dry Run), filling in `SKILL.md`
   (procedure plus the `## Intuition` subsections) and `examples/`.
2. Validate. `knack validate my-skill` catches schema mistakes locally
   before you burn a version number.
3. Publish. `knack publish my-skill` bumps the version, syncs the
   folder's content into the registry, commits, tags, verifies the tag
   matches the folder byte for byte, pushes. Tag is `my-skill/v<X.Y.Z>`.
   Immutable.
4. Run. `knack run my-skill --input ...`. The agent does the actual work
   using SKILL.md as its playbook. CLI generates the `run_id` and writes
   the `started` event.
5. Mark. `knack mark <run-id> succeeded --output ...` (or
   `failed --reason "..."`). The note text feeds back into the next
   interview pass.
6. Bump. When a miss matters, edit the relevant subsection of
   `SKILL.md`'s `## Intuition` block (add an `### Except when` carve-out
   or an `### Edge cases` bullet), then `knack publish` again. Version
   goes from 0.1.1 to 0.1.2. The git log is the history.

## Run skills as slash commands

`knack link <slug>` installs a published skill into every agent on your
machine as a native `/<slug>` slash command — `~/.claude/skills/<slug>/` for
Claude Code / Cowork, `~/.agents/skills/<slug>/` for Codex, and so on. The
agent runs it like any built-in command. A telemetry wrapper baked into the
linked `SKILL.md` still records a run (`knack run` → `knack mark`),
best-effort, so linked skills keep feeding the iteration loop.

```bash
knack link monthly-close            # /monthly-close in every installed agent (global)
knack link monthly-close --local    # this project only
knack unlink monthly-close          # remove it
knack link --list                   # what's linked, where
```

Linked copies are **pinned** to the version you linked — knack never
auto-pulls a newer one, so a teammate's publish can't silently change what
your agent runs. When a newer version is published upstream, `knack run`
flags it (version + author) and you adopt it explicitly:

```bash
knack link --check                  # which linked skills have upstream updates, and by whom
knack link monthly-close            # update one to the latest
knack link --all                    # update every linked skill
```

`knack link` won't overwrite a skill you authored yourself; pass `--force`
to replace a non-knack skill of the same name. Full details:
`knack docs linking`.

## What works in self-host mode

The full lifecycle, with no cloud round-trip:

| Command | What it does in self-host |
|---|---|
| `knack init --self-host` | Creates the GitHub repo (if missing), clones locally, scaffolds layout, pushes the initial commit |
| `knack auth status` | Shows backend, repo, local clone, resolved `gh` user |
| `knack auth login` | Probes `gh api user` and reports whether gh is installed + authenticated. No Knack token minted; the gh credential is what every subsequent command uses. |
| `knack create <slug>` | Scaffolds a new skill folder in your clone |
| `knack validate <slug>` | Pre-flight checks `SKILL.md` + `meta.knack.yaml` + `tests/` |
| `knack publish <slug>` | Syncs the source folder into the registry (`--from` wins; otherwise workspace → registry clone), bumps version in `meta.knack.yaml`, commits, tags `<slug>/v<X.Y.Z>`, verifies the tag byte-matches the source, pushes |
| `knack list` | Walks your local clone, shows every skill + current version |
| `knack search <q>` | Greps the local clone for matches in slug / description / SKILL.md |
| `knack pull <slug>` | Resolves the latest tag, writes the skill folder into your workspace |
| `knack pull <slug>@<ver>` | Pulls a specific historical version |
| `knack pull @other-user/<slug>` | Fetches from any public `<owner>/knack-skills` repo via the GitHub Contents API |
| `knack pull @other-user/<repo>:<slug>@<ver>` | Full external spec: owner, repo, slug, optional version |
| `knack export` | Self-host: points at the local `skills/` directory (no work to do). Cloud: bulk-pulls every skill in the library into `./knack-export-<date>/<scope>/<slug>/`. |
| `knack run <slug> --input ... --input ...` | Registers a run, writes a `started` event to local JSONL with all inputs. `--no-push` skips the telemetry git push (so does `KNACK_AUTO_PUSH=0` or `auto_push: false` in `knack.yaml`). |
| `knack mark <run-id> succeeded --output ... --output ... --note ...` | Closes the loop with outputs, note, and a computed `duration_ms`. Pass a comma-separated list (`a,b,c`) to bulk-verdict several runs at once. |
| `knack runs overview [--team <slug-or-id>]` | Portfolio dashboard: every skill the caller can read, with `regression` and `stale` flags. `--team` scopes to one team's library (cloud only). |
| `knack runs list <slug>` | Page past runs, filter by `--status`, `--version`, `--agent`, `--since`, `--until`, `--note-contains` |
| `knack runs show <run-id>` | Single run snapshot, including the note and computed duration |
| `knack runs stats <slug> --group-by [version\|agent\|version,agent]` | Cohort rollup; supports cross-tab grouping |
| `knack runs trend <slug> --interval day\|week` | Time-bucketed series; every period emits a point (gap-free) |
| `knack runs diff <slug> <ver-a> <ver-b>` | Side-by-side cohort comparison; deltas `null` when either cohort empty |

## Run telemetry schema

Every `knack run` and `knack mark` appends one line to
`<your-repo>/runs/<YYYY-MM>/<YYYY-MM-DD>.jsonl`. Same schema in both
modes; in cloud mode it also lands server-side for the rollups.

```jsonc
// `knack run email-triage --input ./today.eml --runtime claude-code`
{
  "event": "started",
  "run_id": "2efecd46-...",
  "skill": "email-triage",
  "version": "0.2.1",
  "agent": "claude-code",
  "inputs": ["./today.eml"],
  "status": "started",
  "at": "2026-05-27T18:42:11Z"
}

// `knack mark 2efecd46-... succeeded --output ./out/triaged.csv --note "clean"`
{
  "event": "marked",
  "run_id": "2efecd46-...",
  "skill": "email-triage",
  "version": "0.2.1",
  "agent": "claude-code",
  "inputs": ["./today.eml"],
  "outputs": ["./out/triaged.csv"],
  "status": "succeeded",
  "note": "clean",
  "at": "2026-05-27T18:42:16Z"
}
```

`knack mark` returns a `RunSnapshot` that also includes `duration_ms`
(computed from the two events' timestamps). Fields are documented in
[`crates/knack-backend-github/src/runs.rs`](crates/knack-backend-github/src/runs.rs).

**Push policy.** Every `knack run` and `knack mark` auto-commits the
affected JSONL file and pushes to the repo's default remote/branch.
The commit message is `telemetry: <event> <skill> <run_id>`. Only the
day's JSONL is staged, so unrelated working-tree changes are NOT swept
into the telemetry commit. If the push fails (offline, branch diverged),
the local append still succeeds and the CLI prints a stderr warning
telling you how to recover; the next successful `run` / `mark` /
`publish` carries the queued commits.

The remote name and default branch are resolved per-repo (in order):
`KNACK_REMOTE_NAME` / `KNACK_REMOTE_BRANCH` env vars → local
`git symbolic-ref refs/remotes/<remote>/HEAD` → `gh repo view --json
defaultBranchRef` → `origin/main` fallback. So `master`-default repos,
fork workflows, and custom remotes work without configuration.

**Three ways to opt out** of the auto-push (the local commit still lands
either way; only the network hop is skipped):

```
knack run --no-push                  # per-invocation
KNACK_AUTO_PUSH=0 knack run ...      # per-shell
# or in <repo>/knack.yaml:
auto_push: false                     # persistent per-workspace
```


## For agents loading this README

Operating surface, when you're driving the CLI on a user's behalf:

- Binary is `knack`. `knack --help` for the full tree; `knack introspect --json`
  for a machine-readable command catalog.
- User's backend mode is at `~/.knack/config.yaml`. Read it before anything
  else: `mode: github` skips cloud auth; `mode: cloud` requires
  `knack auth login`.
- Skill folders are at `<workspace>/skills/<slug>/`. Required:
  `SKILL.md` (with frontmatter `name`, `description`; rules live inside
  under `## Intuition` with `### Always` / `### Except when` /
  `### Edge cases` subsections) and `meta.knack.yaml` (with
  `name`, `slug`, `author`, `version`). Optional: `examples/`,
  `scripts/`, `assets/`, `references/`, `tests/`. (Skills pulled from
  older cloud versions may also ship a sidecar `intuition.md`; the
  pull/publish path tolerates it for back-compat but new authoring
  does not produce one.)
- The four-phase interview skill is embedded in the binary. Start it with
  `knack interview start`. The CLI writes the skill into
  `<cwd>/.claude/skills/knack-interview/` and returns a session id you pass
  to subsequent `knack interview save / advance` calls.
- Pre-flight validation: `knack validate <slug>` returns a structured
  issues list. Fix and retry locally before paying any publish round-trip.
- `--input` on `knack run` and `--output` on `knack mark` are both
  repeatable. Pass each file separately.
- Every command supports `--json`. Stdout is JSON; stderr is chatter.
- Exit codes are stable: `0` success, `1` user, `2` auth, `3` network,
  `4` conflict, `5` plan limit, `6` partial failure (bulk op — some
  items succeeded, some didn't), `64` usage, `70` internal. See
  `knack docs exit-codes` for the full list.
- The canonical agent playbook is `knack info` (fetches `getknack.ai/agent.txt`,
  embedded fallback when offline). It's large (~19k tokens), so pull only the
  section you need: `knack info --list` shows the index and
  `knack info <slug>...` prints one or more sections (e.g. `knack info running`,
  or `knack info interview authoring publishing`).
- To make a published skill a real `/<slug>` command, `knack link <slug>`
  (per-machine, `--local` for one project). Linked copies are pinned; `knack
  run` flags upstream updates and the user adopts them with `knack link` /
  `knack link --all`. `knack unlink <slug>` removes it.

## Contributing

Bug fixes, typo fixes, and documentation improvements are welcome. Feature
PRs are kept narrow. Open a Discussion first so we can talk through
direction before code lands. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT. See [LICENSE](LICENSE).

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=jordan-gibbs/knack-cli&type=Date)](https://star-history.com/#jordan-gibbs/knack-cli&Date)
