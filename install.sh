#!/usr/bin/env sh
# knack CLI installer for macOS and Linux.
#
# Usage:
#   curl -fsSL https://knack.ai/install | sh
#   curl -fsSL https://knack.ai/install | sh -s -- --version 0.5.0
#   curl -fsSL https://knack.ai/install | sh -s -- --bin-dir /custom/path
#
# Detects OS+arch, downloads the matching binary from GitHub Releases,
# extracts it into ~/.local/bin (or $KNACK_BIN_DIR), and reminds you to
# add that directory to PATH if it's not already there. Idempotent:
# re-running upgrades in place.

set -eu

# GitHub repo for releases. Override to point at a fork or staging repo.
GH_REPO="${KNACK_GH_REPO:-jordan-gibbs/knack-cli}"
VERSION="${KNACK_VERSION:-latest}"
BIN_DIR="${KNACK_BIN_DIR:-$HOME/.local/bin}"
TMPDIR_KEEP=""

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --bin-dir) BIN_DIR="$2"; shift 2 ;;
        --keep-tmp) TMPDIR_KEEP=1; shift ;;
        -h|--help)
            cat <<EOF
knack installer

Options:
  --version <ver>   install a specific version (default: latest)
  --bin-dir <dir>   install into <dir> (default: \$HOME/.local/bin)
  --keep-tmp        leave the temp download dir behind for debugging

Environment:
  KNACK_GH_REPO     GitHub repo for releases (default: $GH_REPO)
  KNACK_VERSION     version to install
  KNACK_BIN_DIR     where to drop the binary
EOF
            exit 0
            ;;
        *) echo "unknown flag: $1" >&2; exit 64 ;;
    esac
done

need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "knack-install: need '$1' on PATH" >&2
        exit 1
    fi
}
need curl
need uname
need mktemp
need tar

# Detect OS + arch → target triple.
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
    Darwin) os_part="apple-darwin" ;;
    Linux)  os_part="unknown-linux-musl" ;;
    *)
        echo "knack-install: unsupported OS '$os'" >&2
        echo "  → use the PowerShell installer on Windows: https://knack.ai/install.ps1" >&2
        exit 1
        ;;
esac
case "$arch" in
    x86_64|amd64) arch_part="x86_64" ;;
    arm64|aarch64) arch_part="aarch64" ;;
    *)
        echo "knack-install: unsupported arch '$arch'" >&2
        exit 1
        ;;
esac
target="${arch_part}-${os_part}"

# We don't ship a native arm64 macOS binary — M-series Macs run the
# x86_64 build via Rosetta. Save 10× macOS runner minutes on the release
# matrix; re-add native arm64 if startup latency matters.
if [ "$target" = "aarch64-apple-darwin" ]; then
    target="x86_64-apple-darwin"
fi

# Resolve version via GitHub's latest-release API. The response is JSON;
# pull out tag_name without needing jq.
if [ "$VERSION" = "latest" ]; then
    tag="$(curl -fsSL "https://api.github.com/repos/${GH_REPO}/releases/latest" \
        | grep -E '"tag_name":' \
        | head -n1 \
        | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
    if [ -z "$tag" ]; then
        echo "knack-install: couldn't resolve latest release from ${GH_REPO}" >&2
        exit 1
    fi
    VERSION="${tag#v}"
fi

archive="knack-${target}.tar.gz"
url="https://github.com/${GH_REPO}/releases/download/v${VERSION}/${archive}"

echo "→ knack v${VERSION} for ${target}"
echo "→ ${url}"

tmp="$(mktemp -d -t knack-install.XXXXXX)"
trap 'if [ -z "$TMPDIR_KEEP" ]; then rm -rf "$tmp"; fi' EXIT

curl -fsSL "$url" -o "$tmp/$archive"
tar -xzf "$tmp/$archive" -C "$tmp"

# Archive layout from taiki-e/upload-rust-binary-action:
#   knack-<target>/knack[.exe]
src="$(find "$tmp" -name knack -type f | head -n1)"
if [ -z "$src" ]; then
    echo "knack-install: 'knack' binary not found in archive" >&2
    exit 1
fi

mkdir -p "$BIN_DIR"
mv "$src" "$BIN_DIR/knack"
chmod +x "$BIN_DIR/knack"

echo "[OK] installed to $BIN_DIR/knack"

# PATH check: print the export line if BIN_DIR isn't on PATH yet, but never
# auto-edit the user's shell profile (too easy to mangle).
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        echo
        echo "$BIN_DIR is not on your PATH. Add this to your shell profile:"
        echo
        echo "    export PATH=\"$BIN_DIR:\$PATH\""
        ;;
esac

"$BIN_DIR/knack" --version || true

# Register knack with the AI agent the user is running in (Claude Code,
# Codex, Cursor, ...). Best-effort: if the detector finds nothing, it
# writes the generic AGENTS.md fallback. Non-fatal.
if "$BIN_DIR/knack" install --auto >/dev/null 2>&1; then
    echo "[OK] registered with detected agent (re-run \`knack install\` to refresh)"
else
    echo "  Tip: run \`knack install\` to register the CLI with your AI agent."
fi

echo
echo "Next: run \`knack init\` to pick self-host (GitHub) or Knack Cloud."
echo "To uninstall later: curl -fsSL https://knack.ai/uninstall.sh | sh"
