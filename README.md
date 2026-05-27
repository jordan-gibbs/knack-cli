# Knack

Teach AI how you actually work.

Knack captures the workflows you do over and over. The judgment calls, the
exceptions, the parts that are obvious to you and invisible to a junior. Your
agent interviews you, captures what only you know, and ships it as a portable
Anthropic Skill that runs in Claude, Cursor, Codex, or any agent.

## Install

```bash
curl -fsSL https://knack.ai/install | sh
```

Windows (PowerShell):

```powershell
irm https://knack.ai/install.ps1 | iex
```

Or have your agent install it for you. In Claude Code, Cursor, or Codex, just
say "install knack" and the agent will run the script and register itself.

## How it works

Six phases. Your agent runs the interview. The CLI captures the state and
ships the result as a real skill folder you can use, version, and share.

1. **Genesis.** Name the part of your job you'd skip if you could.
2. **Artifacts.** Drop in real files from past work. Whatever your normal
   Tuesday looks like.
3. **Intuition.** The exceptions that aren't really exceptions. The judgment
   calls only you make.
4. **Compile.** Everything you taught becomes a folder of instructions,
   examples, and tests. Open Anthropic Skills format. Versioned. Diffable.
   Plain text.
5. **Refine.** Run it on real work. When it misses, point it out. Every miss
   makes the next run sharper.
6. **Publish.** Release a version. To your own GitHub repo, or to the public
   marketplace at knack.ai.

## Two ways to store your knacks

**Self-host (GitHub).** Your knacks live in a private GitHub repo under your
account. Free, private, yours. No third-party account, no servers, no
telemetry. Publishing is a git commit, a per-skill tag, and a push.

```bash
knack init --self-host
```

**Knack Cloud.** Zero setup, public marketplace, free tier at
[knack.ai](https://knack.ai). Team features like sharing, roles, audit log,
and SSO live here.

```bash
knack init --cloud
knack auth login
```

Same CLI for both. Pick a mode with `knack init`.

## Quick start

```bash
knack init                          # pick self-host or cloud
knack create email-triage \
  --name "Email triage" \
  --description "Sort inbox into reply / archive / defer."
# Your agent edits SKILL.md and walks you through the interview...
knack publish email-triage
knack list
```

After publish, your skill is at `<your-repo>/skills/email-triage/` and tagged
`email-triage/v0.1.0`. Future versions get their own tags. Every version is
immutable.

## For agents loading this README

If you are an agent reading this to operate Knack on the user's behalf, here
is the operating surface:

- The CLI command is `knack`. Run `knack --help` for the full surface or
  `knack introspect` for a machine-readable command tree.
- The user's mode (cloud vs github self-host) lives at
  `~/.knack/config.yaml`. Read it before assuming.
- Skill folders are at `<workspace>/skills/<slug>/`. Required files:
  `SKILL.md` (with YAML frontmatter `name` and `description`) and
  `meta.knack.yaml` (with `id`, `name`, `slug`, `author`). Optional:
  `intuition.md`, `examples/`, `scripts/`, `assets/`, `references/`, `tests/`.
- To start the 6-phase interview from a conversation, run
  `knack interview start`. The CLI writes the interview SKILL.md into the
  current project's `.claude/skills/knack-interview/` and returns a session
  id. Persist captured data with `knack interview save --session <id>
  --phase <p> --data <json>` and advance phases with
  `knack interview advance --session <id>`.
- For machine-readable output on any command, pass `--json`. Stdout is JSON,
  stderr is human chatter and warnings.
- Exit codes are stable: `0` success, `1` user error, `2` auth, `3` network,
  `4` conflict, `5` plan limit, `64` usage, `70` internal. Full reference:
  `knack docs exit-codes`.
- The full agent playbook (canonical operating guide) is
  `knack info`. It prints to stdout, no decoration. Fetches the latest copy
  from `getknack.ai/agent.txt` and falls back to the version embedded in the
  binary.

## What's in this repo

```
crates/knack/                  the CLI binary
crates/knack-types/            shared wire-format types (on crates.io)
crates/knack-backend-github/   GitHub self-host implementation
skills/interview/              the interview skill your agent loads
skills/installer/              the install skill agents load on first ask
install.sh / install.ps1       the curl|sh installer (POSIX + Windows)
```

## Contributing

Bug fixes, typo fixes, and documentation improvements are welcome. Feature
PRs are kept narrow. Please open a Discussion first so we can talk through
direction before code lands. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT. See [LICENSE](LICENSE).
