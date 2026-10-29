#!/usr/bin/env bash
# Build the x86_64 release tarball in a Debian 12 container (glibc 2.36), so the binary also
# runs on distros older than the build machine, e.g. Fedora 41 (glibc 2.40) when built on Arch.
# Output: dist/quicksnap-<version>-x86_64-linux.tar.gz and its .sha256.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
NAME="quicksnap-$VERSION-x86_64-linux"
ENGINE=$(command -v podman || command -v docker)

"$ENGINE" run --rm -v "$PWD:/src" -w /src -e CARGO_TARGET_DIR=/src/target/release-container \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" docker.io/library/rust:1-bookworm sh -c '
        apt-get update -qq && apt-get install -y -qq libxkbcommon-dev >/dev/null &&
        cargo build --release --locked &&
        chown -R "$HOST_UID:$HOST_GID" target/release-container'

BIN=target/release-container/release/quicksnap
echo "needs $(objdump -T "$BIN" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -n1)"

rm -rf "dist/$NAME"
mkdir -p "dist/$NAME"
cp "$BIN" install.sh README.md LICENSE "dist/$NAME/"
tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
(cd dist && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
echo "dist/$NAME.tar.gz"
