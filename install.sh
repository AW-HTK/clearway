#!/bin/sh
# Installs or updates the clearway client of a WSL distribution in ~/.local/bin. clearway-setup.exe runs it; to run
# it by hand, point CLEARWAY_SOURCE at a dist/ folder (scripts/build-release.sh).
# Other variables: CLEARWAY_BIN_DIR, CLEARWAY_LANG (en, fr).
set -eu

bin=${CLEARWAY_BIN_DIR:-$HOME/.local/bin}
case ${CLEARWAY_LANG:-${LC_ALL:-${LANG:-}}} in fr*) fr=1 ;; *) fr= ;; esac
say() { if [ -n "$fr" ]; then printf '%s\n' "$2"; else printf '%s\n' "$1"; fi; }
source=${CLEARWAY_SOURCE:-}
if [ -z "$source" ]; then
    say "Run clearway-setup.exe on Windows (https://github.com/AW-HTK/clearway/releases/latest), or set CLEARWAY_SOURCE to a dist/ folder." \
        "Lancez clearway-setup.exe sous Windows (https://github.com/AW-HTK/clearway/releases/latest), ou définissez CLEARWAY_SOURCE vers un dossier dist/." >&2
    exit 2
fi

case $(uname -m) in
    x86_64 | amd64) asset=clearway-x86_64-linux ;;
    *) say "clearway: unsupported architecture $(uname -m)" "clearway : architecture $(uname -m) non prise en charge" >&2; exit 1 ;;
esac

fetch() {
    case $source in
        http://* | https://*)
            if command -v curl >/dev/null 2>&1; then curl -fsSL "$source/$1" -o "$2"
            else wget -qO "$2" "$source/$1"; fi ;;
        *) cp "$source/$1" "$2" ;;
    esac
}

tmp=$(mktemp -d)
trap 'rm -f "$tmp/$asset" "$tmp/SHA256SUMS"; rmdir "$tmp"' EXIT
fetch "$asset" "$tmp/$asset"
fetch SHA256SUMS "$tmp/SHA256SUMS"
(cd "$tmp" && grep " \*\{0,1\}$asset\$" SHA256SUMS | sha256sum -c --status) || {
    say "clearway: SHA-256 check failed for $asset" "clearway : échec du contrôle SHA-256 de $asset" >&2
    exit 1
}

mkdir -p "$bin"
# Replacing through a rename leaves running copies (MCP servers of open sessions) untouched.
install -m 755 "$tmp/$asset" "$bin/.clearway.new"
mv -f "$bin/.clearway.new" "$bin/clearway"
say "clearway installed in $bin." "clearway installé dans $bin."

case :$PATH: in
    *:"$bin":*) ;;
    *) say "Add $bin to your PATH (for example in ~/.profile), then open a new terminal." \
           "Ajoutez $bin à votre PATH (par exemple dans ~/.profile), puis ouvrez un nouveau terminal." ;;
esac
if ! ls /mnt/c/Users/*/AppData/Local/Programs/clearway/clearway.exe >/dev/null 2>&1 && [ -z "${CLEARWAY_EXE:-}" ]; then
    say "clearway.exe is not installed on Windows yet: run clearway-setup.exe from
  https://github.com/AW-HTK/clearway/releases/latest" \
        "clearway.exe n'est pas encore installé côté Windows : lancez clearway-setup.exe, à télécharger sur
  https://github.com/AW-HTK/clearway/releases/latest"
fi
