#!/usr/bin/env bash
#
# wal-rust installer: copies the binary into a user bin dir and registers it
# in PATH across bash / zsh / fish (whichever rc files exist).
#
# Usage:
#   ./install.sh [BINARY_PATH] [INSTALL_DIR]
#
#   BINARY_PATH  path to the wal-rust binary (default: auto-detect the
#                zigbuild release build, then plain target/release)
#   INSTALL_DIR  where to install (default: ~/.local/bin, or ~/bin if that
#                is already in PATH, or /usr/local/bin when run as root)
#
set -euo pipefail

log()  { printf '\033[1;32m%s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m%s\033[0m\n' "$*" >&2; }

# ---------- locate the binary ----------
BIN="${WAL_RUST_BIN:-}"
if [[ -z "$BIN" && $# -ge 1 ]]; then BIN="$1"; fi
if [[ -z "$BIN" ]]; then
    for cand in \
        target/x86_64-unknown-linux-gnu/release/wal-rust \
        target/release/wal-rust; do
        if [[ -x "$cand" ]]; then BIN="$cand"; break; fi
    done
fi
if [[ -z "$BIN" || ! -x "$BIN" ]]; then
    echo "error: wal-rust binary not found. Build it first:" >&2
    echo "  cargo build --release   (or: cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.35)" >&2
    exit 1
fi
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
log "using binary: $BIN"

# ---------- pick install dir ----------
INSTALL_DIR=""
if [[ $# -ge 2 ]]; then
    INSTALL_DIR="$2"
elif [[ "$(id -u)" == "0" && -d /usr/local/bin && -w /usr/local/bin ]]; then
    INSTALL_DIR="/usr/local/bin"
else
    if [[ ":$PATH:" == *":$HOME/bin:"* ]]; then
        INSTALL_DIR="$HOME/bin"
    else
        INSTALL_DIR="$HOME/.local/bin"
    fi
fi
mkdir -p "$INSTALL_DIR"
if [[ ! -w "$INSTALL_DIR" ]]; then
    echo "error: install dir $INSTALL_DIR is not writable" >&2
    exit 1
fi
log "install dir: $INSTALL_DIR"

# ---------- copy binary ----------
cp "$BIN" "$INSTALL_DIR/wal-rust"
chmod +x "$INSTALL_DIR/wal-rust"
log "installed $INSTALL_DIR/wal-rust"

# ---------- PATH registration ----------
path_contains() {
    local dir; IFS=':' read -ra dirs <<<"$PATH"
    for d in "${dirs[@]}"; do
        [[ "$d" == "$1" ]] && return 0
    done
    return 1
}

rc_updated=0
ensure_in_rc() { # rcfile, line
    if ! grep -qsF "$2" "$1" 2>/dev/null; then
        mkdir -p "$(dirname "$1")"
        printf '\n# wal-rust\n%s\n' "$2" >> "$1"
        log "registered PATH in $1"
        rc_updated=1
    fi
}

if path_contains "$INSTALL_DIR"; then
    log "PATH already contains $INSTALL_DIR - nothing to register"
else
    case "${SHELL:-}" in
        *fish)
            ensure_in_rc "$HOME/.config/fish/config.fish" \
                "set -gx PATH $INSTALL_DIR \$PATH"
            ;;
        *zsh)
            ensure_in_rc "$HOME/.zshrc" \
                "export PATH=\"$INSTALL_DIR:\$PATH\""
            ensure_in_rc "$HOME/.zshenv" \
                "export PATH=\"$INSTALL_DIR:\$PATH\""
            ;;
        *)
            ensure_in_rc "$HOME/.bashrc" \
                "export PATH=\"$INSTALL_DIR:\$PATH\""
            ensure_in_rc "$HOME/.profile" \
                "export PATH=\"$INSTALL_DIR:\$PATH\""
            ;;
    esac
fi

# ---------- verify ----------
export PATH="$INSTALL_DIR:$PATH"
if "$INSTALL_DIR/wal-rust" --version >/dev/null 2>&1; then
    VER="$("$INSTALL_DIR/wal-rust" --version)"
    log "done: $VER available now (new shells will pick it up automatically)"
    if [[ "$rc_updated" == "1" ]]; then
        warn "PATH registration added to shell rc files - run 'source ~/.bashrc' or open a new terminal"
    fi
else
    echo "error: installed binary does not run" >&2
    exit 1
fi
