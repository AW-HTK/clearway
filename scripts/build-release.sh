#!/bin/sh
# Builds the release files into dist/: clearway.exe, clearway-x86_64-linux (static), the two install scripts,
# clearway-setup.exe (which embeds the four of them) and SHA256SUMS. Needs cargo-zigbuild and the targets x86_64-pc-windows-gnu and x86_64-unknown-linux-musl.
set -eu
cd "$(dirname "$0")/.."
cargo zigbuild --release -j 4 --target x86_64-pc-windows-gnu --bin clearway
cargo zigbuild --release -j 4 --target x86_64-unknown-linux-musl --bin clearway
mkdir -p dist
cp target/x86_64-pc-windows-gnu/release/clearway.exe dist/clearway.exe
cp target/x86_64-unknown-linux-musl/release/clearway dist/clearway-x86_64-linux
cp windows/install.ps1 install.sh dist/
(cd dist && sha256sum clearway.exe clearway-x86_64-linux install.ps1 install.sh > SHA256SUMS)
cargo zigbuild --release -j 4 --target x86_64-pc-windows-gnu --features setup --bin clearway-setup
cp target/x86_64-pc-windows-gnu/release/clearway-setup.exe dist/
(cd dist && sha256sum clearway-setup.exe >> SHA256SUMS)
cat dist/SHA256SUMS
