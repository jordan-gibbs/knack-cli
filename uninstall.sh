#!/usr/bin/env sh
# knack CLI uninstaller for macOS and Linux.
#
# Usage:
#   curl -fsSL https://getknack.ai/uninstall.sh | sh
#   curl -fsSL https://getknack.ai/uninstall.sh | sh -s -- --bin-dir /custom/path
#
# Companion to install.sh. Removes the binary, the per-user ~/.knack
# cache directory, and (best-effort) the agent shims via `knack
# uninstall` if the binary is still present.
#
# We never auto-edit your shell rc. If install.sh asked you to add
# ~/.local/bin to PATH, you can leave that line in your rc; it's a
# no-op once the binary is gone.

set -eu

BIN_DIR="${KNACK_BIN_DIR:-$HOME/.local/bin}"

while [ $# -gt 0 ]; do
    case "$1" in
        --bin-dir) BIN_DIR="$2"; shift 2 ;;
        -h|--help)
            cat <<EOF
knack uninstaller

Options:
  --bin-dir <dir>   binary location (default: \$HOME/.local/bin)
EOF
            exit 0
            ;;
        *) echo "unknown flag: $1" >&2; exit 64 ;;
    esac
done

KNACK_HOME="$HOME/.knack"
KNACK_BIN="$BIN_DIR/knack"

# 1. Best-effort: ask the CLI to strip every detected agent config
#    block + shim file before we delete it. --keep-auth so we don't
#    double-clear; step 3 wipes ~/.knack wholesale.
if [ -x "$KNACK_BIN" ]; then
    "$KNACK_BIN" uninstall --yes --quiet --keep-auth >/dev/null 2>&1 || true
fi

# 2. Remove the binary itself.
if [ -e "$KNACK_BIN" ]; then
    rm -f "$KNACK_BIN" && echo "[OK] removed $KNACK_BIN"
fi

# 3. Remove the per-user cache (~/.knack with auth.json, installed.json,
#    update-check.json). May already be partly gone after step 1.
if [ -d "$KNACK_HOME" ]; then
    rm -rf "$KNACK_HOME" && echo "[OK] removed $KNACK_HOME"
fi

echo
echo "knack removed."
echo "Your shell rc may still mention ~/.local/bin; safe to leave."
