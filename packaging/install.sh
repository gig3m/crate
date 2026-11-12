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
die() {
    echo "crate install: $*" >&2
    exit 1
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
for f in "$WRAPPER" "$DESKTOP"; do
    if [ -e "$f" ] && ! is_ours "$f"; then
        echo "$f exists and isn't Crate's; not overwriting it" >&2
        exit 1
    fi
done

tmp=$(mktemp -d)
trap cleanup EXIT
cat > "$tmp/wrapper" <<'SH'
#!/bin/sh
# Crate launcher. Crate keeps its own Plex sign-in (~/.config/crate/account.toml).
exec "$HOME/.local/lib/crate-player/crate-player" "$@"
SH
grep -v '^Exec=' packaging/crate-player.desktop > "$tmp/desktop"
printf 'Exec="%s"\n' "$(desktop_quote "$WRAPPER")" >> "$tmp/desktop"
stage target/release/crate-player "$BIN" 755
stage assets/crate.svg "$ICON" 644
stage "$tmp/wrapper" "$WRAPPER" 755
stage "$tmp/desktop" "$DESKTOP" 644
commit

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
