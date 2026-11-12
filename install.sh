#!/bin/sh
# Install (or update) Crate from the latest GitHub release, for the current
# user, without root:
#
#   curl -fsSL https://raw.githubusercontent.com/gig3m/crate/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/gig3m/crate/main/install.sh | sh -s -- --uninstall
#
# It puts the program in ~/.local/lib/crate-player, the command
# `crate-player` in ~/.local/bin, and an icon and launcher entry so Crate
# shows up in your app launcher. Settings stay in ~/.config/crate.
set -eu

REPO="gig3m/crate"
ASSET="crate-player-x86_64-linux.tar.gz"
URL="${CRATE_RELEASE_URL:-https://github.com/$REPO/releases/latest/download/$ASSET}"

BIN="$HOME/.local/lib/crate-player/crate-player"
WRAPPER="$HOME/.local/bin/crate-player"
ICON="$HOME/.local/share/icons/hicolor/scalable/apps/crate-player.svg"
DESKTOP="$HOME/.local/share/applications/crate-player.desktop"

say() { printf '%s\n' "$*"; }
die() { printf 'crate install: %s\n' "$*" >&2; exit 1; }

refresh_launcher() {
    command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$BIN" "$WRAPPER" "$ICON" "$DESKTOP"
    rmdir "$HOME/.local/lib/crate-player" 2>/dev/null || true
    refresh_launcher
    say "Crate uninstalled. Your settings (~/.config/crate) and caches (~/.cache/crate) are still there; delete them to remove everything."
    exit 0
fi

[ "$(uname -s)" = Linux ] || die "Crate runs on Linux."
[ "$(uname -m)" = x86_64 ] || die "the prebuilt release is for x86_64; on $(uname -m), build from source (see the README)."
command -v curl >/dev/null || die "curl is needed."
command -v tar >/dev/null || die "tar is needed."

# Runtime libraries Crate links against.
missing=""
for lib in libpipewire-0.3.so.0 libopus.so.0 libxkbcommon.so.0 libxkbcommon-x11.so.0 libxcb.so.1; do
    if ! ldconfig -p 2>/dev/null | grep -q "$lib"; then missing="$missing $lib"; fi
done
if [ -n "$missing" ]; then
    say "Note: these libraries weren't found:$missing"
    say "  Arch: sudo pacman -S pipewire opus libxkbcommon libxkbcommon-x11 libxcb"
    say "  Debian/Ubuntu: sudo apt install libpipewire-0.3-0 libopus0 libxkbcommon0 libxkbcommon-x11-0 libxcb1"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
say "Downloading Crate…"
curl -fsSL "$URL" -o "$tmp/crate.tar.gz" || die "couldn't download $URL"
tar -xzf "$tmp/crate.tar.gz" -C "$tmp" || die "the download isn't a valid archive"
[ -x "$tmp/crate-player" ] || die "the archive has no crate-player"

if [ -e "$WRAPPER" ] && ! grep -q 'Crate launcher' "$WRAPPER" 2>/dev/null; then
    die "$WRAPPER exists and isn't Crate's; not overwriting it"
fi

install -Dm755 "$tmp/crate-player" "$BIN"
install -Dm644 "$tmp/crate-player.svg" "$ICON"
mkdir -p "$(dirname "$WRAPPER")" "$(dirname "$DESKTOP")"
cat > "$WRAPPER" <<'SH'
#!/bin/sh
# Crate launcher. Crate keeps its own Plex sign-in (~/.config/crate/account.toml).
exec "$HOME/.local/lib/crate-player/crate-player" "$@"
SH
chmod 755 "$WRAPPER"
exec_path=$(printf '%s' "$WRAPPER" | sed -e 's/[\\"`$]/\\&/g')
grep -v '^Exec=' "$tmp/crate-player.desktop" > "$DESKTOP.tmp"
printf 'Exec="%s"\n' "$exec_path" >> "$DESKTOP.tmp"
mv "$DESKTOP.tmp" "$DESKTOP"
refresh_launcher

version=$("$BIN" --version 2>/dev/null || true)
say "Crate installed${version:+ ($version)}. Open it from your app launcher, or run: crate-player"
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) say "(~/.local/bin isn't on your PATH; the launcher entry works either way.)" ;;
esac
