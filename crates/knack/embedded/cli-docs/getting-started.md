## Install

macOS / Linux:

    curl -fsSL https://getknack.ai/install | sh

Windows (PowerShell):

    irm https://getknack.ai/install.ps1 | iex

Verify:

    knack --version

## Pick a backend

knack runs in one of two modes. Pick once with `knack init`; switch later
by re-running.

### Self-host (GitHub)

Skills live in a private GitHub repo under your account. No knack.ai
sign-in. Requires `gh` to be authenticated (`gh auth status` should show
you logged in).

    knack init --self-host \
        --github-repo <your-handle>/<your-skills-repo> \
        --visibility private

What this does, in one shot:

  - Creates github.com/<your-handle>/<your-skills-repo> if it doesn't exist
  - Clones it locally to ~/<your-skills-repo> (override with --local-path)
  - Scaffolds skills/, runs/, knack.yaml, README.md, .gitignore
  - Makes the initial commit and pushes to the repo's default branch
    (fresh gh-created repos default to `main`; subsequent telemetry
    pushes resolve the remote/branch per-repo, so `master` repos and
    fork workflows work without configuration)
  - Writes ~/.knack/config.yaml so future commands know to use github

### Knack Cloud

Skills live on api.getknack.ai. Public marketplace, team features
(sharing, roles, audit log, SSO).

    knack init --cloud
    knack auth login        # opens browser; signs you in / signs you up

### Don't run bare `knack init` from a non-TTY shell

In a TTY, `knack init` asks "self-host or cloud?". In agents'
non-interactive shells, it would block forever, so it errors with
`NEEDS_FLAGS` instead. Always pass `--self-host` or `--cloud` from
scripts and agents.

## First skill in 60 seconds

    knack create my-skill --name "My Skill"
    # ... your agent runs the interview, fills in SKILL.md (including its
    #     ## Intuition section: ### Always / ### Except when / ### Edge cases) ...
    knack validate my-skill
    knack publish my-skill
    knack run my-skill --input ./example.txt --input ./other.txt
    knack mark <run-id> succeeded --output ./out/result.md --note "clean"
    knack list

## Workspace layout

`knack init` writes a `.knack/` directory next to where you ran it. In
github mode, your skills also live in the clone at the path your config
points at (e.g. `~/<your-skills-repo>/skills/`).

    .knack/
    ├── skills/        # `knack pull` writes here (consume)
    ├── drafts/        # `knack create` writes here in cloud mode
    ├── .gitignore
    └── README.md

In github self-host mode, `knack create` writes directly into
`<your-clone>/skills/<slug>/` (no drafts/ indirection; the clone IS the
workspace).

Workspace discovery walks up the directory tree git-style. Flags that
override the default:

  * `--target <path>` — write to a specific directory
  * `--global` — use `~/.knack/skills/` (the HOME-shared pool)
  * `KNACK_SKILLS_DIR=<path>` env — same as `--global` with a custom path

## Pulling skills

In github mode `knack pull` resolves from your own clone by default:

    knack pull email-triage             # latest tag
    knack pull email-triage@0.2.1       # specific version

Or pull a skill from any other user's public knack-skills repo:

    knack pull @other-user/their-skill
    knack pull @other-user/their-skill@0.1.0
    knack pull @other-user/their-repo:their-skill        # custom repo name

External pulls use the GitHub Contents API; no full clone needed.

## Publishing in github mode

A version is a git tag on your registry repo: `<slug>/vX.Y.Z`. Publish
resolves its source folder (`--from` wins; otherwise workspace drafts →
workspace skills → HOME pool → the registry clone itself), syncs that
folder into `<clone>/skills/<slug>/` (deletions included), commits,
tags, verifies the tagged tree byte-matches the source, and pushes.
The pull → edit → publish loop therefore ships your edited workspace
copy — same as cloud mode. The output's `from` line names the folder
that shipped.
