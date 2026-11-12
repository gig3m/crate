#!/bin/sh
# Install Crate from a source checkout for the current user (no root): the
# binary, a launcher, the icon and a desktop entry, so it shows up in the
# app launcher. Idempotent; rerun after a build. Replaces an older install
# that used the command name `crate`.
#
#   cargo build --release && packaging/install.sh
#   packaging/install.sh --uninstall
set -eu
cd "$(dirname "$0")/.."

BIN="$HOME/.local/lib/crate-player/crate-player"
WRAPPER="$HOME/.local/bin/crate-player"
ICON="$HOME/.local/share/icons/hicolor/scalable/apps/crate-player.svg"
DESKTOP="$HOME/.local/share/applications/crate-player.desktop"

# The install before the rename (command `crate`): only Crate's own files.
remove_old() {
    old="$HOME/.local/bin/crate"
    if [ -f "$old" ] && grep -q 'Crate launcher' "$old" 2>/dev/null; then rm -f "$old"; fi
    rm -f "$HOME/.local/lib/crate/crate" "$HOME/.local/share/icons/hicolor/scalable/apps/crate.svg"
    rmdir "$HOME/.local/lib/crate" 2>/dev/null || true
    if [ -f "$HOME/.local/share/applications/crate.desktop" ] && grep -q '^Name=Crate$' "$HOME/.local/share/applications/crate.desktop"; then
        rm -f "$HOME/.local/share/applications/crate.desktop"
    fi
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$BIN" "$WRAPPER" "$ICON" "$DESKTOP"
    rmdir "$HOME/.local/lib/crate-player" 2>/dev/null || true
    remove_old
    command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" || true
    echo "Crate uninstalled (your settings in ~/.config/crate and caches in ~/.cache/crate are kept)."
    exit 0
fi

[ -x target/release/crate-player ] || { echo "build first: cargo build --release" >&2; exit 1; }

install -Dm755 target/release/crate-player "$BIN"
install -Dm644 assets/crate.svg "$ICON"

mkdir -p "$(dirname "$WRAPPER")"
if [ -e "$WRAPPER" ] && ! grep -q 'Crate launcher' "$WRAPPER" 2>/dev/null; then
    echo "$WRAPPER exists and isn't Crate's launcher; not overwriting it" >&2
    exit 1
fi
cat > "$WRAPPER" <<'SH'
#!/bin/sh
# Crate launcher. Crate keeps its own Plex sign-in (~/.config/crate/account.toml).
exec "$HOME/.local/lib/crate-player/crate-player" "$@"
SH
chmod 755 "$WRAPPER"

mkdir -p "$(dirname "$DESKTOP")"
# Desktop-entry Exec: quote the path and escape \ " ` $ (paths with spaces work).
exec_path=$(printf '%s' "$WRAPPER" | sed -e 's/[\\"`$]/\\&/g')
grep -v '^Exec=' packaging/crate-player.desktop > "$DESKTOP.tmp"
printf 'Exec="%s"\n' "$exec_path" >> "$DESKTOP.tmp"
mv "$DESKTOP.tmp" "$DESKTOP"

remove_old
command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
echo "Crate installed: $WRAPPER (launcher entry: $DESKTOP)"
