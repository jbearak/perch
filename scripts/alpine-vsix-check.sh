#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# VS Code refuses a VSIX for another platform, so an alpine VSIX cannot be
# installed on a glibc host. This unpacks it instead and runs its binary
# through scripts/release-smoke.sh in an alpine:3 container of the VSIX's
# architecture.
#
#   scripts/alpine-vsix-check.sh perch-alpine-x64-0.1.0.vsix alpine-x64
#
# The scratch directory is made under $ALPINE_CHECK_DIR (default $TMPDIR or
# /tmp); it must be a directory Docker can mount.
set -eu

vsix=$1
target=$2
case "$target" in
    alpine-x64) dir=linux-x64 platform=linux/amd64 ;;
    alpine-arm64) dir=linux-arm64 platform=linux/arm64 ;;
    *) echo "alpine-vsix-check: target must be alpine-x64 or alpine-arm64" >&2; exit 2 ;;
esac

here=$(cd "$(dirname "$0")" && pwd)
scratch=$(mktemp -d "${ALPINE_CHECK_DIR:-${TMPDIR:-/tmp}}/alpine-check.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
unzip -q "$vsix" -d "$scratch/vsix"
cp "$here/release-smoke.sh" "$scratch/release-smoke.sh"

docker run --rm --platform "$platform" -v "$scratch:/check:ro" alpine:3 \
    sh -c "cat /etc/alpine-release && sh /check/release-smoke.sh /check/vsix/extension/bin/$dir/perch"
echo "alpine-vsix-check: ok ($vsix)"
