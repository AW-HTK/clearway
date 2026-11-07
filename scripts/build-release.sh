#!/bin/sh
# Builds the release files into dist/: clearway.exe, clearway-x86_64-linux (static), the two install scripts, VERSION,
# clearway-setup.exe (which embeds them), SHA256SUMS and its Ed25519 signature SHA256SUMS.sig.
# Needs cargo-zigbuild, the targets x86_64-pc-windows-gnu and x86_64-unknown-linux-musl, and the signing seed in
# CLEARWAY_RELEASE_SIGNING_KEY (for instance: bws-get run -- scripts/build-release.sh).
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
plugin_version() { sed -n 's/.*"version": "\([^"]*\)".*/\1/p' | head -1; }
claude=$(plugin_version < plugins/clearway/.claude-plugin/plugin.json)
codex=$(plugin_version < plugins/clearway/.codex-plugin/plugin.json)
[ "$claude" = "$codex" ] || { echo "plugin versions differ: Claude $claude, Codex $codex" >&2; exit 1; }
# Claude Code only sees a new plugin when its version changes.
last=$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)
if [ -n "$last" ] && ! git diff --quiet "$last" -- plugins .claude-plugin; then
    before=$(git show "$last:plugins/clearway/.claude-plugin/plugin.json" | plugin_version)
    [ "$before" != "$claude" ] || { echo "plugins/ changed since $last: raise the plugin version ($claude)" >&2; exit 1; }
fi
[ -n "${CLEARWAY_RELEASE_SIGNING_KEY:-}" ] || { echo "CLEARWAY_RELEASE_SIGNING_KEY missing" >&2; exit 1; }

cargo zigbuild --release -j 4 --target x86_64-pc-windows-gnu --bin clearway
cargo zigbuild --release -j 4 --target x86_64-unknown-linux-musl --bin clearway
cargo build --release -j 4 --example sign_release
mkdir -p dist
cp target/x86_64-pc-windows-gnu/release/clearway.exe dist/clearway.exe
cp target/x86_64-unknown-linux-musl/release/clearway dist/clearway-x86_64-linux
cp windows/install.ps1 install.sh dist/
printf '%s\n' "$version" > dist/VERSION
(cd dist && sha256sum clearway.exe clearway-x86_64-linux install.ps1 install.sh VERSION > SHA256SUMS)
cargo zigbuild --release -j 4 --target x86_64-pc-windows-gnu --features setup --bin clearway-setup
cp target/x86_64-pc-windows-gnu/release/clearway-setup.exe dist/
(cd dist && sha256sum clearway-setup.exe >> SHA256SUMS)
target/release/examples/sign_release sign dist/SHA256SUMS
cat dist/SHA256SUMS
