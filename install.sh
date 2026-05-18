#!/usr/bin/env bash
# triage-cli installer for macOS and Linux.
# Usage:  curl -fsSL https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.sh | sh
# Flags:  --version v0.2.0       Pin to a specific release tag.
#         --channel prerelease   Allow prereleases when picking "latest".
#         --dry-run              Print actions without executing them.

set -euo pipefail

REPO="midwestman35/triage-cli"
VERSION=""
CHANNEL="stable"
DRY_RUN=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --channel) CHANNEL="$2"; shift 2 ;;
        --dry-run) DRY_RUN="1"; shift ;;
        *) echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
done

step() {
    if [ -n "$DRY_RUN" ]; then printf '\033[33m[dry-run]\033[0m %s\n' "$*"
    else                       printf '\033[36m%s\033[0m\n' "$*"
    fi
}

# 1. Detect OS + arch.
uname_s="$(uname -s)"
uname_m="$(uname -m)"
case "$uname_s/$uname_m" in
    Darwin/arm64)         TARGET="aarch64-macos";   ARCHIVE="triage-cli-aarch64-macos.tar.gz" ;;
    Darwin/x86_64)        TARGET="x86_64-macos";    ARCHIVE="triage-cli-x86_64-macos.tar.gz" ;;
    Linux/x86_64)         TARGET="x86_64-linux";    ARCHIVE="triage-cli-x86_64-linux.tar.gz" ;;
    *) echo "Unsupported platform: $uname_s/$uname_m" >&2; exit 1 ;;
esac

# 2. Resolve install dirs.
BIN_DIR="$HOME/.local/bin"
if [ -n "${TRIAGE_HOME:-}" ]; then
    DATA_DIR="$TRIAGE_HOME"
elif [ "$uname_s" = "Darwin" ]; then
    DATA_DIR="$HOME/Library/Application Support/triage-cli"
else
    DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/triage-cli"
fi

# 3. Resolve release.
if [ -n "$VERSION" ]; then
    API="https://api.github.com/repos/$REPO/releases/tags/$VERSION"
elif [ "$CHANNEL" = "prerelease" ]; then
    API="https://api.github.com/repos/$REPO/releases"
else
    API="https://api.github.com/repos/$REPO/releases/latest"
fi
step "Querying $API"
release_json="$(curl -fsSL "$API")"
if [ "$CHANNEL" = "prerelease" ]; then
    # Take the first element of the array.
    TAG="$(printf '%s' "$release_json" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
else
    TAG="$(printf '%s' "$release_json" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
fi
[ -n "$TAG" ] || { echo "Could not resolve release tag" >&2; exit 1; }
echo "Installing $TAG"

# 4. Download archive + SHA256SUMS.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
ARCHIVE_URL="https://github.com/$REPO/releases/download/$TAG/$ARCHIVE"
SUMS_URL="https://github.com/$REPO/releases/download/$TAG/SHA256SUMS"
step "Downloading $ARCHIVE"
[ -n "$DRY_RUN" ] || curl -fsSL "$ARCHIVE_URL" -o "$TMP/$ARCHIVE"
step "Downloading SHA256SUMS"
[ -n "$DRY_RUN" ] || curl -fsSL "$SUMS_URL" -o "$TMP/SHA256SUMS"

# 5. Verify SHA256.
if [ -z "$DRY_RUN" ]; then
    if command -v shasum >/dev/null 2>&1; then
        ACTUAL="$(shasum -a 256 "$TMP/$ARCHIVE" | awk '{print $1}')"
    elif command -v sha256sum >/dev/null 2>&1; then
        ACTUAL="$(sha256sum "$TMP/$ARCHIVE" | awk '{print $1}')"
    else
        echo "Neither shasum nor sha256sum available; cannot verify download." >&2
        exit 1
    fi
    EXPECTED="$(awk -v n="$ARCHIVE" '$2 == n { print $1 }' "$TMP/SHA256SUMS")"
    [ -n "$EXPECTED" ] || { echo "SHA256SUMS missing line for $ARCHIVE" >&2; exit 1; }
    if [ "$ACTUAL" != "$EXPECTED" ]; then
        echo "SHA256 mismatch for $ARCHIVE: expected $EXPECTED, got $ACTUAL" >&2
        exit 1
    fi
    step "SHA256 verified: $EXPECTED"
fi

# 6. Unpack + install binary atomically.
step "Installing binary to $BIN_DIR/triage-cli"
if [ -z "$DRY_RUN" ]; then
    mkdir -p "$BIN_DIR"
    mkdir -p "$TMP/unpack"
    tar -C "$TMP/unpack" -xzf "$TMP/$ARCHIVE"
    BIN_DEST="$BIN_DIR/triage-cli"
    BIN_NEW="$BIN_DEST.new"
    cp "$TMP/unpack/triage-cli" "$BIN_NEW"
    chmod +x "$BIN_NEW"
    mv "$BIN_NEW" "$BIN_DEST"  # atomic rename; survives running binary
fi

# 7. Seed data dir.
step "Seeding data dir at $DATA_DIR"
if [ -z "$DRY_RUN" ]; then
    mkdir -p "$DATA_DIR"
    INV_SRC="$TMP/unpack/apex-cnc-inventory.md"
    INV_DST="$DATA_DIR/apex-cnc-inventory.md"
    INV_VER="$DATA_DIR/.inventory-version"
    if [ ! -f "$INV_DST" ]; then
        cp "$INV_SRC" "$INV_DST"
        ( cd "$DATA_DIR" && (shasum -a 256 apex-cnc-inventory.md 2>/dev/null || sha256sum apex-cnc-inventory.md) | awk '{print $1}' ) > "$INV_VER"
    else
        SHIPPED_HASH="$( (shasum -a 256 "$INV_SRC" 2>/dev/null || sha256sum "$INV_SRC") | awk '{print $1}' )"
        PREV_HASH="$( [ -f "$INV_VER" ] && cat "$INV_VER" | tr -d '[:space:]' || echo "" )"
        LOCAL_HASH="$( (shasum -a 256 "$INV_DST" 2>/dev/null || sha256sum "$INV_DST") | awk '{print $1}' )"
        if [ "$LOCAL_HASH" = "$PREV_HASH" ]; then
            cp "$INV_SRC" "$INV_DST"
            echo "$SHIPPED_HASH" > "$INV_VER"
        else
            cp "$INV_SRC" "$INV_DST.new"
            echo "warning: existing apex-cnc-inventory.md has local edits; new copy saved as apex-cnc-inventory.md.new" >&2
        fi
    fi

    # Seed .env.example if not already present.
    ENVEX_SRC="$TMP/unpack/.env.example"
    ENVEX_DST="$DATA_DIR/.env.example"
    if [ -f "$ENVEX_SRC" ] && [ ! -f "$ENVEX_DST" ]; then
        cp "$ENVEX_SRC" "$ENVEX_DST"
    fi
fi

# 8. PATH hint (don't auto-edit rc files).
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        echo ""
        echo "note: $BIN_DIR is not on your \$PATH."
        SHELL_NAME="$(basename "${SHELL:-/bin/sh}")"
        case "$SHELL_NAME" in
            zsh)  RC="$HOME/.zshrc" ;;
            bash) RC="$HOME/.bashrc" ;;
            *)    RC="your shell's rc file" ;;
        esac
        echo "Add this line to $RC and open a new terminal:"
        echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
        ;;
esac

# 9. Final output.
echo ""
echo "triage-cli installed ($TAG)."
echo "Run: triage-cli setup    # to enter your Zendesk and provider credentials"
echo "Run: triage-cli doctor   # to verify everything works"
