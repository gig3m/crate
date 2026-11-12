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
#
# Everything is downloaded, checked (checksum, all files present, the
# program runs here) and only then swapped in, so a failed update leaves the
# old install working.
set -eu

main() {
    REPO="gig3m/crate"
    ASSET="crate-player-x86_64-linux.tar.gz"
    URL="${CRATE_RELEASE_URL:-https://github.com/$REPO/releases/latest/download/$ASSET}"

    LIBDIR="$HOME/.local/lib/crate-player"
    BIN="$LIBDIR/crate-player"
    WRAPPER="$HOME/.local/bin/crate-player"
    ICON="$HOME/.local/share/icons/hicolor/scalable/apps/crate-player.svg"
    DESKTOP="$HOME/.local/share/applications/crate-player.desktop"

    if [ "${1:-}" = "--uninstall" ]; then
        uninstall
        return
    fi

    [ "$(uname -s)" = Linux ] || die "Crate runs on Linux."
    [ "$(uname -m)" = x86_64 ] || die "the prebuilt release is for x86_64; on $(uname -m), build from source (see the README)."
    command -v curl >/dev/null || die "curl is needed."
    command -v tar >/dev/null || die "tar is needed."
    if [ -e "$WRAPPER" ] && ! is_ours "$WRAPPER"; then
        die "$WRAPPER exists and isn't Crate's; not touching it."
    fi
    check_libraries

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    say "Downloading Crate…"
    curl -fsSL "$URL" -o "$tmp/crate.tar.gz" || die "couldn't download $URL"
    # The release publishes a checksum next to the archive: check it when
    # it's there (a release without one is still accepted, with a note).
    if curl -fsSL "$URL.sha256" -o "$tmp/crate.sha256" 2>/dev/null && command -v sha256sum >/dev/null; then
        want=$(cut -d' ' -f1 < "$tmp/crate.sha256")
        have=$(sha256sum "$tmp/crate.tar.gz" | cut -d' ' -f1)
        [ "$want" = "$have" ] || die "the download's checksum doesn't match the release's; not installing."
    else
        say "(No checksum to verify against; continuing.)"
    fi
    mkdir "$tmp/x"
    tar -xzf "$tmp/crate.tar.gz" -C "$tmp/x" || die "the download isn't a valid archive."
    for f in crate-player crate-player.svg crate-player.desktop; do
        [ -f "$tmp/x/$f" ] || die "the release is missing $f; not installing."
    done
    chmod 755 "$tmp/x/crate-player"
    # It has to run on this system (a glibc that's too old, a missing library).
    if ! version=$("$tmp/x/crate-player" --version 2>"$tmp/err"); then
        say "The downloaded program doesn't run on this system:"
        sed 's/^/  /' "$tmp/err" >&2
        die "not installing. (The release needs glibc 2.39 or newer; see the README.)"
    fi

    # All checked: swap the files in (each with a rename, so none is ever
    # half written).
    mkdir -p "$LIBDIR" "$(dirname "$WRAPPER")" "$(dirname "$ICON")" "$(dirname "$DESKTOP")"
    place "$tmp/x/crate-player" "$BIN" 755
    place "$tmp/x/crate-player.svg" "$ICON" 644
    cat > "$tmp/wrapper" <<'SH'
#!/bin/sh
# Crate launcher. Crate keeps its own Plex sign-in (~/.config/crate/account.toml).
exec "$HOME/.local/lib/crate-player/crate-player" "$@"
SH
    place "$tmp/wrapper" "$WRAPPER" 755
    grep -v '^Exec=' "$tmp/x/crate-player.desktop" > "$tmp/desktop"
    printf 'Exec="%s"\n' "$(desktop_quote "$WRAPPER")" >> "$tmp/desktop"
    place "$tmp/desktop" "$DESKTOP" 644
    refresh_launcher

    say "$version installed. Open Crate from your app launcher, or run: crate-player"
    case ":$PATH:" in
        *":$HOME/.local/bin:"*) ;;
        *) say "(~/.local/bin isn't on your PATH; the launcher entry works either way.)" ;;
    esac
}

say() { printf '%s\n' "$*"; }
die() {
    printf 'crate install: %s\n' "$*" >&2
    exit 1
}

# A file this installer wrote (the launcher script or the desktop entry).
is_ours() {
    grep -q 'Crate launcher' "$1" 2>/dev/null || grep -q '^StartupWMClass=crate$' "$1" 2>/dev/null
}

# Copy $1 next to $2, then rename over it.
place() {
    cp "$1" "$2.new.$$"
    chmod "$3" "$2.new.$$"
    mv -f "$2.new.$$" "$2"
}

# A path as a desktop-entry Exec argument inside double quotes: escape
# \ " ` $ for the argument, then backslashes again and % for the string.
desktop_quote() {
    printf '%s' "$1" | sed -e 's/[\\"`$]/\\&/g' -e 's/\\/\\\\/g' -e 's/%/%%/g'
}

check_libraries() {
    # ldconfig lives in /sbin on some systems (not on a user's PATH).
    ldc=$(PATH="$PATH:/sbin:/usr/sbin" command -v ldconfig || true)
    if [ -z "$ldc" ] || ! "$ldc" -p >/dev/null 2>&1; then
        return # can't tell here; the program's own check below will
    fi
    missing=""
    for lib in libpipewire-0.3.so.0 libopus.so.0 libxkbcommon.so.0 libxkbcommon-x11.so.0 libxcb.so.1 libvulkan.so.1 libwayland-client.so.0; do
        "$ldc" -p | grep -q "$lib" || missing="$missing $lib"
    done
    if [ -n "$missing" ]; then
        say "Note: these libraries weren't found:$missing"
        say "  Arch: sudo pacman -S pipewire opus libxkbcommon-x11 libxcb vulkan-icd-loader wayland"
        say "  Debian/Ubuntu: sudo apt install libpipewire-0.3-0 libopus0 libxkbcommon-x11-0 libxcb1 libvulkan1 libwayland-client0"
    fi
}

refresh_launcher() {
    if command -v update-desktop-database >/dev/null; then update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true; fi
    if command -v gtk-update-icon-cache >/dev/null; then gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true; fi
}

uninstall() {
    # Only what this installer put there.
    if [ -e "$WRAPPER" ]; then
        if is_ours "$WRAPPER"; then rm -f "$WRAPPER"; else say "Leaving $WRAPPER: it isn't Crate's."; fi
    fi
    if [ -e "$DESKTOP" ]; then
        if is_ours "$DESKTOP"; then rm -f "$DESKTOP"; else say "Leaving $DESKTOP: it isn't Crate's."; fi
    fi
    rm -f "$BIN" "$ICON"
    rmdir "$LIBDIR" 2>/dev/null || true
    refresh_launcher
    say "Crate uninstalled. Your settings (~/.config/crate) and caches (~/.cache/crate) are still there; delete them to remove everything."
}

main "$@"
