#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Builds the musl release binary for Docker's native architecture inside
# tests/linux/rust.Dockerfile and copies it to the path given (default
# target/linux-hosts/perch). The local route for the host checks; CI uses
# the binary its Linux job built. Cargo's target directory and Zig's package
# cache live in Docker volumes, so the host's target/ is left alone apart
# from the copy.
set -eu

cd "$(git rev-parse --show-toplevel)"
out="${1:-target/linux-hosts/perch}"
mkdir -p "$(dirname "$out")"

docker build -q -f tests/linux/rust.Dockerfile -t perch-rust perch-vt >/dev/null
docker run --rm -v "$PWD:/src:ro" -v perch-target:/tmp/perch-target \
    -v perch-zig:/tmp/zig-global -w /src \
    -e CARGO_TARGET_DIR=/tmp/perch-target -e ZIG_GLOBAL_CACHE_DIR=/tmp/zig-global \
    perch-rust sh -c 'cargo build --release -p perch --target "$(uname -m)-unknown-linux-musl" >&2 \
        && cat "/tmp/perch-target/$(uname -m)-unknown-linux-musl/release/perch"' >"$out.tmp"
chmod +x "$out.tmp"
mv "$out.tmp" "$out"
echo "build-musl: $out"
