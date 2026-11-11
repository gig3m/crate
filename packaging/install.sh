#!/bin/sh
# Install Crate for the current user (no root): the binary, a launcher
# wrapper, the icon and a desktop entry
# so it appears in the Omarchy app launcher. Idempotent; rerun after a build.
#
#   cargo build --release && packaging/install.sh
#   packaging/install.sh --uninstall
set -eu
cd "$(dirname "$0")/.."

BIN="$HOME/.local/lib/crate/crate"
WRAPPER="$HOME/.local/bin/crate"
ICON="$HOME/.local/share/icons/hicolor/scalable/apps/crate.svg"
DESKTOP="$HOME/.local/share/applications/crate.desktop"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$BIN" "$WRAPPER" "$ICON" "$DESKTOP"
    rmdir "$HOME/.local/lib/crate" 2>/dev/null || true
    command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" || true
    echo "Crate uninstalled."
    exit 0
fi

[ -x target/release/crate ] || { echo "build first: cargo build --release" >&2; exit 1; }

install -Dm755 target/release/crate "$BIN"
install -Dm644 assets/crate.svg "$ICON"

mkdir -p "$(dirname "$WRAPPER")"
cat > "$WRAPPER" <<'SH'
#!/bin/sh
# Crate launcher. Crate keeps its own Plex sign-in (~/.config/crate/account.toml).
exec "$HOME/.local/lib/crate/crate" "$@"
SH
chmod 755 "$WRAPPER"

mkdir -p "$(dirname "$DESKTOP")"
sed "s|^Exec=.*|Exec=$WRAPPER|" packaging/crate.desktop > "$DESKTOP"

command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
echo "Crate installed: $WRAPPER (launcher entry: $DESKTOP)"
