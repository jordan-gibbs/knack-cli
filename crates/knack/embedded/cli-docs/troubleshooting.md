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

### Behind a corporate proxy

Set standard env vars:

    export HTTPS_PROXY=http://proxy.corp:3128
    export HTTP_PROXY=http://proxy.corp:3128

### `AUTH_REQUIRED` despite being logged in

Token expired. Refresh:

    knack auth login

### Bug reports

    knack debug              # dumps env, config, last 10 commands (redacted)

Send the output to support — never includes your file contents.
