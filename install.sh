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
    case "$(uname -m)" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) die "there's no prebuilt release for $(uname -m); build from source (see the README)." ;;
    esac
    ASSET="crate-player-$arch-linux.tar.gz"
    command -v curl >/dev/null || die "curl is needed."
    command -v tar >/dev/null || die "tar is needed."
    for f in "$WRAPPER" "$DESKTOP"; do
        if [ -e "$f" ] && ! is_ours "$f"; then
            die "$f exists and isn't Crate's; not touching it."
        fi
    done
    check_libraries

    # One release for both downloads: resolve "latest" to its tag first.
    if [ -n "${CRATE_RELEASE_URL:-}" ]; then
        URL="$CRATE_RELEASE_URL"
        need_sum=""
    else
        latest=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") || die "couldn't reach GitHub."
        tag=${latest##*/}
        case "$tag" in v*) ;; *) die "couldn't find the latest release (got $latest)." ;; esac
        URL="https://github.com/$REPO/releases/download/$tag/$ASSET"
        need_sum=1
    fi

    tmp=$(mktemp -d)
    trap cleanup EXIT
    say "Downloading Crate…"
    if ! curl -fsSL "$URL" -o "$tmp/crate.tar.gz"; then
        [ -n "$need_sum" ] && die "couldn't download $URL (if $tag has no $arch build, build from source; see the README)."
        die "couldn't download $URL"
    fi
    # Every release publishes a checksum next to the archive: it must match
    # (only a CRATE_RELEASE_URL test download may come without one).
    if curl -fsSL "$URL.sha256" -o "$tmp/crate.sha256" 2>/dev/null; then
        command -v sha256sum >/dev/null || die "sha256sum is needed to check the download."
        want=$(cut -d' ' -f1 < "$tmp/crate.sha256")
        have=$(sha256sum "$tmp/crate.tar.gz" | cut -d' ' -f1)
        [ "$want" = "$have" ] || die "the download's checksum doesn't match the release's; not installing."
    elif [ -n "$need_sum" ]; then
        die "couldn't download the release's checksum; not installing (try again)."
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

    # All checked: build every file, stage them next to their places, then
    # swap them all in together (or none).
    cat > "$tmp/wrapper" <<'SH'
#!/bin/sh
# Crate launcher. Crate keeps its own Plex sign-in (~/.config/crate/account.toml).
exec "$HOME/.local/lib/crate-player/crate-player" "$@"
SH
    grep -v '^Exec=' "$tmp/x/crate-player.desktop" > "$tmp/desktop"
    printf 'Exec="%s"\n' "$(desktop_quote "$WRAPPER")" >> "$tmp/desktop"
    stage "$tmp/x/crate-player" "$BIN" 755
    stage "$tmp/x/crate-player.svg" "$ICON" 644
    stage "$tmp/wrapper" "$WRAPPER" 755
    stage "$tmp/desktop" "$DESKTOP" 644
    commit
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

# All-or-nothing update. `stage SRC DEST MODE` copies SRC into a new,
# exclusively created file next to DEST (nothing is followed or
# overwritten); `commit` renames every staged file into place, backing up
# what was there first, and puts it all back if any step fails.
check_dest() {
    if [ -L "$1" ] || [ -d "$1" ]; then die "$1 is a directory or a link; not touching it."; fi
}
stage() {
    check_dest "$2"
    mkdir -p "$(dirname "$2")" || die "couldn't create $(dirname "$2")."
    t=$(mktemp "$(dirname "$2")/.crate-player-new.XXXXXX") || die "couldn't write next to $2."
    printf '%s\t%s\n' "$t" "$2" >> "$tmp/plan"
    cp "$1" "$t" && chmod "$3" "$t" || die "couldn't write next to $2."
}
commit() {
    : > "$tmp/done"
    while IFS='	' read -r t d; do
        b=""
        if [ -e "$d" ]; then
            if ! b=$(mktemp "$(dirname "$d")/.crate-player-old.XXXXXX") || ! cp -p "$d" "$b"; then
                failed=$d
                if [ -n "$b" ]; then rm -f "$b"; fi
                rollback
                die "couldn't back up $failed; the previous install is back in place."
            fi
        fi
        if ! mv -f -T "$t" "$d"; then
            # This one is untouched; undo the ones already swapped in.
            failed=$d
            if [ -n "$b" ]; then rm -f "$b"; fi
            rollback
            die "couldn't write $failed; the previous install is back in place."
        fi
        printf '%s\t%s\t%s\n' "$t" "$d" "$b" >> "$tmp/done"
    done < "$tmp/plan"
    while IFS='	' read -r t d b; do
        if [ -n "$b" ]; then rm -f "$b"; fi
    done < "$tmp/done"
}
rollback() {
    while IFS='	' read -r t d b; do
        if [ -n "$b" ]; then mv -f -T "$b" "$d" 2>/dev/null || true; else rm -f "$d"; fi
    done < "$tmp/done"
}
# On any exit: staged files that weren't committed, and the work dir.
cleanup() {
    if [ -f "$tmp/plan" ]; then
        while IFS='	' read -r t d; do rm -f "$t"; done < "$tmp/plan"
    fi
    rm -rf "$tmp"
}

# A path as a desktop-entry Exec argument inside double quotes: escape
# \ " ` $ for the argument, then backslashes again and % for the string.
desktop_quote() {
    printf '%s' "$1" | sed -e 's/[\\"`$]/\\&/g' -e 's/\\/\\\\/g' -e 's/%/%%/g'
}

check_libraries() {
    # ldconfig lives in /sbin on some systems (not on a user's PATH).
    ldc=""
    for c in /sbin/ldconfig /usr/sbin/ldconfig "$(command -v ldconfig || true)"; do
        if [ -n "$c" ] && [ -x "$c" ]; then ldc=$c; break; fi
    done
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
