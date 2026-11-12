#!/bin/sh
# Install Crate from a source checkout for the current user (no root): the
# binary, a launcher, the icon and a desktop entry, so it shows up in the
# app launcher. Idempotent; rerun after a build.
#
#   cargo build --release && packaging/install.sh
#   packaging/install.sh --uninstall
set -eu
cd "$(dirname "$0")/.."

LIBDIR="$HOME/.local/lib/crate-player"
BIN="$LIBDIR/crate-player"
WRAPPER="$HOME/.local/bin/crate-player"
ICON="$HOME/.local/share/icons/hicolor/scalable/apps/crate-player.svg"
DESKTOP="$HOME/.local/share/applications/crate-player.desktop"

# A file this installer (or install.sh) wrote.
is_ours() {
    grep -q 'Crate launcher' "$1" 2>/dev/null || grep -q '^StartupWMClass=crate$' "$1" 2>/dev/null
}
place() {
    cp "$1" "$2.new.$$"
    chmod "$3" "$2.new.$$"
    mv -f "$2.new.$$" "$2"
}
desktop_quote() {
    printf '%s' "$1" | sed -e 's/[\\"`$]/\\&/g' -e 's/\\/\\\\/g' -e 's/%/%%/g'
}
refresh_launcher() {
    if command -v update-desktop-database >/dev/null; then update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true; fi
    if command -v gtk-update-icon-cache >/dev/null; then gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true; fi
}

if [ "${1:-}" = "--uninstall" ]; then
    for f in "$WRAPPER" "$DESKTOP"; do
        if [ -e "$f" ]; then
            if is_ours "$f"; then rm -f "$f"; else echo "Leaving $f: it isn't Crate's."; fi
        fi
    done
    rm -f "$BIN" "$ICON"
    rmdir "$LIBDIR" 2>/dev/null || true
    refresh_launcher
    echo "Crate uninstalled (your settings in ~/.config/crate and caches in ~/.cache/crate are kept)."
    exit 0
fi

[ -x target/release/crate-player ] || { echo "build first: cargo build --release" >&2; exit 1; }
if [ -e "$WRAPPER" ] && ! is_ours "$WRAPPER"; then
    echo "$WRAPPER exists and isn't Crate's launcher; not overwriting it" >&2
    exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$LIBDIR" "$(dirname "$WRAPPER")" "$(dirname "$ICON")" "$(dirname "$DESKTOP")"
place target/release/crate-player "$BIN" 755
place assets/crate.svg "$ICON" 644
cat > "$tmp/wrapper" <<'SH'
#!/bin/sh
# Crate launcher. Crate keeps its own Plex sign-in (~/.config/crate/account.toml).
exec "$HOME/.local/lib/crate-player/crate-player" "$@"
SH
place "$tmp/wrapper" "$WRAPPER" 755
grep -v '^Exec=' packaging/crate-player.desktop > "$tmp/desktop"
printf 'Exec="%s"\n' "$(desktop_quote "$WRAPPER")" >> "$tmp/desktop"
place "$tmp/desktop" "$DESKTOP" 644

# The install from before the rename (command `crate`), when it's Crate's.
old="$HOME/.local/bin/crate"
if [ -f "$old" ] && grep -q 'Crate launcher' "$old" 2>/dev/null; then
    rm -f "$old" "$HOME/.local/lib/crate/crate"
    rmdir "$HOME/.local/lib/crate" 2>/dev/null || true
fi
olddesk="$HOME/.local/share/applications/crate.desktop"
if [ -f "$olddesk" ] && grep -q '^StartupWMClass=crate$' "$olddesk"; then rm -f "$olddesk"; fi

refresh_launcher
echo "Crate installed: $WRAPPER (launcher entry: $DESKTOP)"
