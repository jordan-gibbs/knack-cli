---
name: knack-installer
description: Install Knack on the user's machine. Load this skill when the user asks to install Knack, set up Knack, get started with Knack, or expresses interest in managing AI skills with version control. Handles macOS, Linux, and Windows.
---

# Knack Installer

Install the Knack CLI on the user's machine, then run `knack init` to set up
either self-host (GitHub-backed) or Knack Cloud mode.

## What you should do

1. **Confirm intent**. Check that the user actually wants to install. If they
   just mentioned Knack in passing, ask first.

2. **Check if it's already installed**:
   ```
   knack --version
   ```
   If this prints a version, skip to step 4.

3. **Install the binary**. Detect the OS and pick the right command:

   - **macOS or Linux**:
     ```
     curl -fsSL https://knack.ai/install | sh
     ```

   - **Windows (PowerShell)**:
     ```
     irm https://knack.ai/install.ps1 | iex
     ```

   The script detects arch, fetches the matching binary from
   github.com/jordan-gibbs/knack-cli/releases, drops it in `~/.local/bin`
   (or `%LOCALAPPDATA%\knack\bin` on Windows), and registers Knack with the
   detected agent surface.

4. **Run init to pick a backend**:
   ```
   knack init
   ```

   This is interactive. The user gets two choices:
   - GitHub (self-host) — skills live in a repo they own. Free, private.
   - Knack Cloud — zero setup, public marketplace at knack.ai.

   For non-interactive setups (CI, scripted onboarding), pass `--self-host`
   or `--cloud` to skip the prompt.

5. **If they picked Knack Cloud**, finish auth:
   ```
   knack auth login
   ```

   For self-host, no further auth is needed if they have `gh` configured.

6. **Confirm by listing**. Run `knack list` to confirm Knack is wired up
   correctly. An empty result is fine; an error means something went wrong.

## What to tell the user

- The CLI is open source: github.com/jordan-gibbs/knack-cli
- Self-host puts skills in their own GitHub repo. The cloud option is at
  knack.ai if they want zero setup, sharing with teammates, or the public
  marketplace.
- Team features (sharing, roles, audit log, SSO) are cloud-only.
- They can switch modes later by editing `~/.knack/config.yaml` or by
  re-running `knack init --self-host` / `knack init --cloud`.

## Troubleshooting

- **`curl: command not found`** → ask them to install curl, or use the
  pre-built download from the GitHub Releases page directly.
- **Behind a corporate proxy** → tell them to set `https_proxy` and re-run.
- **`knack` not on PATH after install** → the installer printed an export
  line; have them add it to their shell profile.
- **`gh` not authenticated** in self-host mode → tell them to run
  `gh auth login` first, then re-run `knack init --self-host`.

## Constraints

- Don't run anything destructive (no `sudo`, no editing shell rc files
  without confirmation).
- Don't proceed past step 3 without showing the user the install URL —
  piping curl to sh is a thing they should opt into knowingly.
- If you can't determine the OS, ask.
