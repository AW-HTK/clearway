#!/bin/sh
# Build a local, unsigned installer without changing dist/'s signed release.
# Requires cargo-zigbuild, Zig on PATH, and the two release targets (see README).
set -eu
cd "$(dirname "$0")/.."
bundle="$PWD/dist/local"
jobs=${CLEARWAY_BUILD_JOBS:-2}
mkdir -p "$bundle"

cargo zigbuild --offline --locked --release -j "$jobs" --target x86_64-pc-windows-gnu --bin clearway
cp target/x86_64-pc-windows-gnu/release/clearway.exe "$bundle/clearway.exe"
cargo zigbuild --offline --locked --release -j "$jobs" --target x86_64-unknown-linux-musl --bin clearway
cp target/x86_64-unknown-linux-musl/release/clearway "$bundle/clearway-x86_64-linux"
cp windows/install.ps1 windows/uninstall.ps1 install.sh assets/fonts/FONT-LICENSES.txt "$bundle/"
sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1 > "$bundle/VERSION"
(cd "$bundle" && sha256sum clearway.exe clearway-x86_64-linux install.ps1 uninstall.ps1 install.sh VERSION FONT-LICENSES.txt > SHA256SUMS)
CLEARWAY_BUNDLE_DIR="$bundle" cargo zigbuild --offline --locked --release -j "$jobs" --target x86_64-pc-windows-gnu --features setup --bin clearway-setup
cp target/x86_64-pc-windows-gnu/release/clearway-setup.exe "$bundle/clearway-setup.exe"
(cd "$bundle" && sha256sum clearway-setup.exe >> SHA256SUMS)
echo "Local installer (unsigned release): $bundle/clearway-setup.exe"
