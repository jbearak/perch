#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Inside tests/linux/rust.Dockerfile: test on the native musl target, build
# the release binary and check it the way CI does. Uses a target directory
# inside the container so the host's target/ is left alone.
set -eu

musl="$(uname -m)-unknown-linux-musl"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/perch-target}"
export ZIG_GLOBAL_CACHE_DIR="${ZIG_GLOBAL_CACHE_DIR:-/tmp/zig-global}"

cargo test --workspace --target "$musl"
cargo test --workspace --target "$musl" --features perch/test-hooks
cargo build --release -p perch --target "$musl"

bin="$CARGO_TARGET_DIR/$musl/release/perch"
"$bin" version --json
file "$bin"
file "$bin" | grep -E '(statically|static-pie) linked' >/dev/null
! file "$bin" | grep interpreter >/dev/null
dups=$(nm --defined-only "$bin" | awk 'NF == 3 && $2 ~ /^[TDBR]$/ { print $3 }' | sort | uniq -d)
if [ -n "$dups" ]; then
    echo "strong symbols defined more than once:" >&2
    echo "$dups" >&2
    exit 1
fi
echo "musl-check: ok"
