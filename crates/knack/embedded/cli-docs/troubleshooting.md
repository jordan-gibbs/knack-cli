## Troubleshooting

### `knack: command not found`

The installer adds `knack` to your PATH but you may need a fresh shell:

    macOS / Linux:  source ~/.zshrc  (or restart terminal)
    Windows:        restart PowerShell

If still missing:

    which knack                      # macOS / Linux
    Get-Command knack                # Windows

### Keyring errors at login

- macOS: open Keychain Access; if locked, unlock the login keychain
- Linux: install `gnome-keyring` or `kwallet`; or set `KNACK_AUTH_TOKEN` env var
- Windows: should Just Work; if not, run as your normal user (not Administrator)

### First step for any network error: run the connectivity check

    knack debug --connectivity

It probes every host knack needs and tells you exactly which one fails
and how (DNS/TCP, TLS, or timeout) — no login required. A `TLS` result
means a TLS-inspecting proxy (see below); a `CONNECT` result usually
means that host is firewalled.

### Behind a corporate proxy

knack honors the standard proxy env vars:

    export HTTPS_PROXY=http://proxy.corp:3128
    export HTTP_PROXY=http://proxy.corp:3128
    # with credentials:
    export HTTPS_PROXY=http://user:pass@proxy.corp:3128

If your proxy requires NTLM or Kerberos (Negotiate) auth, knack can't
speak those directly. Run a local auth-handling proxy such as `cntlm` or
`px` and point knack at it:

    export HTTPS_PROXY=http://localhost:3128

### Allowlist these hosts

If your firewall allowlists by hostname, knack needs ALL of these, not
just the API — a common trap is allowlisting `api.getknack.ai` only, then
seeing `publish`/`pull` fail because skill bundles live on a different
(storage) host:

    api.getknack.ai      API
    getknack.ai          web sign-in + docs
    cli.getknack.ai      install / upgrade
    <bundle storage>     publish / pull   (run `knack debug --connectivity`
                                           to print the exact host)

`knack debug --connectivity` always shows the current, authoritative list.

### `NETWORK` error on a TLS-inspecting network (Netskope / Zscaler / etc.)

If `curl https://api.getknack.ai/` works but knack fails with a generic
`network error ... error sending request`, your network is intercepting
TLS and presenting a certificate signed by a corporate CA.

knack trusts the OS trust store by default, so if your CA is installed in
the system keychain (the usual case) this should already work. If the CA
lives only in a file, point knack at it:

    knack --cacert /path/to/corp-ca.pem auth login

or set it once for the session (also honors the standard SSL_CERT_FILE):

    export KNACK_CA_BUNDLE=/path/to/corp-ca.pem

The bundle is trusted IN ADDITION to the normal roots, so all commands
(auth, list, publish, run) work. As a last resort only:

    export KNACK_INSECURE=1     # disables cert verification — avoid

Self-host mode (`mode: github`) routes through `gh` and is unaffected.

### `AUTH_REQUIRED` despite being logged in (cloud mode)

Token expired. PATs default to a 90-day TTL (since v0.7.8), so plan on
re-running `knack auth login` quarterly. To mint a no-expiry token for
unattended CI:

    knack auth login --never-expires

To explicitly set a TTL (1..730 days):

    knack auth login --expires-in-days 365

The CLI prints an `AuthRequired` warning one day before expiry, so a
re-login is rarely a surprise.

### `AUTH_REQUIRED` in self-host mode

That shouldn't happen any more in v0.7.0+. If it does, you might have an
old config file. Check:

    cat ~/.knack/config.yaml          # macOS / Linux
    type %USERPROFILE%\.knack\config.yaml  # Windows

It should show `mode: github`. If it says `mode: cloud` and you wanted
github, re-run init:

    knack init --self-host --github-repo <you>/<repo> --visibility private

### `knack init` hangs

You're in a non-TTY shell (an agent's shell tool, CI, a pipe). The
interactive prompt has nowhere to read from. Pass flags explicitly:

    knack init --self-host --github-repo <you>/<repo> --visibility private
    knack init --cloud

v0.7.1 and newer error out with `NEEDS_FLAGS` immediately instead of
hanging.

### `gh` not authenticated for self-host

Self-host mode uses your existing `gh` credential helper. If
`gh auth status` shows you logged out, fix that first:

    gh auth login

Then re-run whatever knack command failed.

### `knack publish` complains about uncommitted changes

In self-host mode, publish refuses to commit if you have edits OUTSIDE
the skill folder being published. Stash or commit them first:

    git stash
    knack publish my-skill
    git stash pop

### `knack publish` fails with "tag does not match the content being published"

Self-host publish re-reads the tagged tree after committing and
byte-compares it against the source folder before anything is pushed.
A mismatch almost always means a `.gitignore` rule in your registry
repo is excluding skill files (e.g. `*.csv`, `examples/`). Remove or
narrow the rule, delete the local tag it names, and re-publish.
Nothing was pushed, so the remote is untouched.

### Not sure which folder a publish shipped

Publish prints a `from <path>` line (and a `"from"` field in `--json`)
naming the exact source folder it synced. Verify any past release from
first principles:

    git -C <registry-clone> show "<slug>/v<X.Y.Z>:skills/<slug>/SKILL.md"

### `knack pull @owner/slug` returns 404

The external pull uses the GitHub Contents API and assumes the repo is
named `knack-skills` by convention. If the owner uses a different repo
name, pass it explicitly:

    knack pull @owner/their-repo-name:slug

For private repos, your `gh` token needs read access.

### Bug reports

    knack debug              # dumps env, config, last 10 commands (redacted)

Send the output to support — never includes your file contents.
