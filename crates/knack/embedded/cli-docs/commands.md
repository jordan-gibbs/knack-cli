## Commands

The CLI is a thin skill-store client. Authoring the SKILL.md folder happens in
chat with your agent (see `agent-integration` topic); the CLI publishes the
result. Surface:

    knack auth login [--no-browser] [--account NAME]
    knack auth logout [--account NAME]
    knack auth status

    knack list [--scope=personal|team|public] [--folder NAME] [--unfiled]
    knack pull <slug>[@<semver>] [--target DIR]
    knack diff <slug>@<a> <slug>@<b>

    knack create <slug> --name "..." [--scope personal|team|public] [--team-id UUID]
    knack publish <slug> [--from DIR] [--major|--minor|--patch] [--as-version X.Y.Z]

    knack run <slug> --input PATH [--runtime claude-code|cowork|raw] [--dry] [--no-exec]
    knack mark <run_id> succeeded|failed [--note "..."]

    knack folder create <name> [--team-id UUID]
    knack folder list [--scope personal|team] [--team-id UUID]
    knack folder rename <id-or-name> <new-name>
    knack folder delete <id-or-name>
    knack folder mv <skill-slug> <folder-name>
    knack folder mv <skill-slug> --unfiled

    knack docs [<topic>]
    knack introspect
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
