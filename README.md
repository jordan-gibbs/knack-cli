# Knack

A CLI for authoring, validating, versioning, and observing agent skills.

Knack turns the workflows you do over and over into Anthropic Skills:
structured folders of instructions, examples, tests, and metadata that any
agent (Claude, Cursor, Codex, Cowork) can load. The artifact is real and
inspectable. Every version is immutable. Every run is logged. When a skill
misses an edge case, you flag it, the skill gets a new rule, and the next
run is sharper than the last.

## What you actually ship

```
skills/email-triage/
├── SKILL.md            # the playbook the agent loads
├── meta.knack.yaml     # id, name, slug, author, version, description
├── intuition.md        # the edge cases and judgment calls
├── examples/           # input/output pairs from real past work
├── scripts/            # optional helper scripts
├── assets/             # optional static files
├── references/         # optional reference material
└── tests/              # optional assertions that run pre-publish
```

This is the open [Anthropic Skills](https://github.com/anthropics/skills)
format. Portable across agents. Plain text. Diffable.

## Install

```bash
curl -fsSL https://knack.ai/install | sh
```

Windows (PowerShell):

```powershell
irm https://knack.ai/install.ps1 | iex
```

Or in any Claude Code, Cursor, or Codex session, just say "install knack."

## What's different about how Knack captures a skill

**Edge cases get captured up front, not discovered in production.** The
interview's intuition phase is scenario-driven: "what if the date format is
malformed, what if the customer is on a legacy plan, what if the request
has two conflicting fields." Every answer becomes a rule in `intuition.md`,
which is what makes the skill robust on a Tuesday afternoon when the input
looks weird.

**Local pre-flight validation catches authoring mistakes before publish.**
`knack validate <slug>` checks every required field in `meta.knack.yaml`,
every required frontmatter key in `SKILL.md`, and the structure of any
`tests/`. You don't burn a version number to find out you forgot a field.

**Versions are immutable.** Publishing creates a per-skill git tag
(`email-triage/v0.1.0`) and a content hash. Once shipped, that version is
forever. New work gets a new version. No silent rewrites.

**Run telemetry is built in.** Every `knack run` records what was asked,
what the skill produced, whether it succeeded, how long it took, and how
many tokens it used. In self-host mode the log is a JSONL file in your
repo at `runs/<yyyy-mm>/<yyyy-mm-dd>.jsonl`. In cloud mode it lands in
api.getknack.ai with per-skill rollups.

**Iteration is a loop, not a one-shot.** When a run misses, `knack mark
<run-id> failed --reason "..."` records the gap. The agent reads it back
into the intuition phase, captures the new rule, and `knack publish` bumps
the version. The next run uses the sharper skill. The history is in the
commit log.

## Two ways to store your skills

**Self-host (GitHub).** Skills live in a private GitHub repo under your
account. Free. No third-party account. No telemetry leaves your machine.
Publishing is a git commit, tag, and push.

```bash
knack init --self-host
```

**Knack Cloud.** Zero setup. Public marketplace and team features (sharing,
roles, audit log, SSO) live here. Free tier at
[knack.ai](https://knack.ai).

```bash
knack init --cloud
knack auth login
```

Same CLI surface. Same skill folder format. Same commands. The backend just
decides where versions and run logs go.

## Quick start

```bash
knack init                                          # pick self-host or cloud
knack create email-triage \
  --name "Email triage" \
  --description "Sort inbox into reply / archive / defer."

# Your agent runs the 6-phase interview and writes the skill files.
# You edit SKILL.md and intuition.md to taste.

knack validate email-triage                         # pre-flight check
knack publish email-triage                          # tag + ship
knack run email-triage --input ./today.eml          # use it
knack mark <run-id> succeeded                       # close the loop
knack list                                          # see what you have
```

## For agents loading this README

If you're an agent loading this to operate Knack on the user's behalf, the
surface is:

- The binary is `knack`. `knack --help` for the full tree;
  `knack introspect --json` for a machine-readable command catalog.
- The user's backend mode is in `~/.knack/config.yaml`. Read it first.
- Skill folders are at `<workspace>/skills/<slug>/`. The required files are
  `SKILL.md` (with frontmatter `name`, `description`) and `meta.knack.yaml`
  (with `id`, `name`, `slug`, `author`). Optional: `intuition.md`,
  `examples/`, `scripts/`, `assets/`, `references/`, `tests/`.
- To start the 6-phase interview, run `knack interview start`. The CLI
  writes the interview SKILL.md into `<cwd>/.claude/skills/knack-interview/`
  and returns a session id. Persist captured data per phase with
  `knack interview save --session <id> --phase <p> --data <json>`. Advance
  with `knack interview advance --session <id>`.
- Pre-flight validation is `knack validate <slug>`. It returns a structured
  issues list (path, message, code) so you can repair the file and retry
  before paying a publish round-trip.
- After a run, close the loop with `knack mark <run-id> succeeded|failed
  --reason "..."`. The reason text feeds back into the next interview pass.
- For machine-readable output on any command, pass `--json`. Stdout is the
  JSON envelope; stderr is human-only chatter.
- Exit codes are stable: `0` success, `1` user error, `2` auth, `3`
  network, `4` conflict, `5` plan limit, `64` usage, `70` internal. Full
  reference: `knack docs exit-codes`.
- The canonical agent playbook is `knack info`, which fetches
  `getknack.ai/agent.txt` and falls back to the version baked into the
  binary at build time.

## What's in this repo

```
crates/knack/                  the CLI binary
crates/knack-types/            shared wire-format types (on crates.io)
crates/knack-backend-github/   GitHub self-host implementation
skills/interview/              the 6-phase interview skill
skills/installer/              the install skill agents load on first ask
install.sh / install.ps1       the curl|sh installer
```

## Contributing

Bug fixes, typo fixes, and documentation improvements are welcome. Feature
PRs are kept narrow. Open a Discussion first so we can talk through
direction before code lands. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT. See [LICENSE](LICENSE).
