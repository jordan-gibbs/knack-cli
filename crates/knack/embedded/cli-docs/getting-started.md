## Install

macOS / Linux:

    curl -fsSL https://getknack.ai/install | sh

Windows (PowerShell):

    irm https://getknack.ai/install.ps1 | iex

Anywhere with Node:

    npm install -g @knack/cli

Anywhere with Python:

    pipx install knack-cli

## First skill in 60 seconds

    knack auth login                              # opens browser, sign in once
    knack init                                    # scaffold .knack/ in cwd
    knack list                                    # see your skills (empty at first)
    knack create my-skill --name "My Skill"       # bootstrap + scaffold .knack/drafts/my-skill/
    # Author SKILL.md in .knack/drafts/my-skill/ with your agent
    # (rules go inside it under ## Intuition; scripts/ assets/
    # references/ are optional subdirs), then:
    knack publish my-skill                        # push the draft folder
    knack run my-skill --input ./example.xlsx     # use the skill
    knack mark <run_id> succeeded                 # close the feedback loop

## Workspace layout

`knack init` creates a `.knack/` directory in the current folder:

    .knack/
    ├── skills/        # `knack pull` writes here (consume)
    ├── drafts/        # `knack create` writes here (author in progress)
    ├── .gitignore     # ignores everything in .knack/ by default
    └── README.md

Workspace discovery walks up the directory tree git-style — running
`knack pull` from `<repo>/src/foo/` uses the `.knack/` at `<repo>/`.

Flags that override the default:

* `--target <path>` — write to a specific directory
* `--global` — use `~/.knack/skills/` (the legacy HOME-shared pool)
* `KNACK_SKILLS_DIR=<path>` env — same as `--global` with a custom path
