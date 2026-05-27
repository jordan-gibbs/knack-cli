## Login

    knack auth login

Opens your default browser to a Clerk-gated approval page. After you sign in
and click Approve, the CLI receives an access token and stores it in your OS
keyring (Keychain / Credential Manager / libsecret).

## Headless / CI

    knack auth login --no-browser

Prints the verification URL to stderr instead of opening a browser. Useful for
SSH sessions and containers.

## Service accounts (CI)

Set `KNACK_AUTH_TOKEN` directly:

    export KNACK_AUTH_TOKEN=knack_xxx
    knack list

Tokens are scoped per-user. Issue them from the web app at `/settings/tokens`.

## Multiple accounts

    knack auth login --account work
    knack auth login --account personal
    knack --account work list

## Logout

    knack auth logout              # current account
    knack auth logout --account work
