# Knack

Skill management for agents. Your skills, your repo, your rules.

## Install

```bash
curl -fsSL https://knack.ai/install | sh
```

Or have your agent install it for you in any Claude Code, Cursor, or Codex
session ("install knack").

## Two ways to run

**Self-host (GitHub):** Your skills live in a private GitHub repo under your
account. Free, private, yours. No third-party account, no servers, no
telemetry. Authoring is `knack create`; publishing is a git commit, tag, and
push to your repo.

**Knack Cloud:** Zero setup, public marketplace, free tier at
[knack.ai](https://knack.ai). Team features (sharing, roles, audit log, SSO)
live here.

Same CLI, same commands. Pick a mode with `knack init`.

## Quick start: self-host

```bash
knack init --self-host       # creates github.com/<you>/<repo>, clones locally
knack create email-triage \
  --name "Email triage" \
  --description "Sort inbox into reply / archive / defer."
# ...edit .../skills/email-triage/SKILL.md to taste...
knack publish email-triage   # commits + tags email-triage/v0.1.0, pushes
knack list                   # shows skills in your repo
```

## Quick start: cloud

```bash
knack init --cloud
knack auth login             # device-code flow against knack.ai
knack create email-triage --name "Email triage"
knack publish email-triage
knack list
```

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
