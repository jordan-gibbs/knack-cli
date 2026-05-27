# Knack

Skill management for agents. Your skills, your repo, your rules.

## Install

```bash
curl -fsSL https://knack.ai/install | sh
```

Or have your agent install it for you in any Claude Code, Cursor, or Codex session:

```
> install knack
```

## Quick start

```bash
knack init                 # pick self-host or cloud
knack create email-triage  # author a new skill
knack run email-triage     # use it in your current project
```

## Two ways to run

**Self-host:** Your skills live in your own GitHub repo. Free, private, yours. No
account, no servers, no telemetry.

**Knack Cloud:** Zero setup, public marketplace, free tier at [knack.ai](https://knack.ai).
Team features (sharing, roles, audit log, SSO) live here.

Same CLI, same commands. Pick a mode with `knack init`.

## What's in this repo

```
crates/knack/                  the CLI binary
crates/knack-types/            shared wire-format types (published to crates.io)
crates/knack-backend-github/   GitHub mode implementation
skills/interview/              the interview skill the agent loads
install.sh                     the curl|sh installer
```

## Contributing

Bug fixes, typo fixes, and documentation improvements are welcome. Feature
PRs are kept narrow: please open a Discussion first so we can talk through
direction before code lands. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT. See [LICENSE](LICENSE).
