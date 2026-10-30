#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Runs a perch binary through `new -d`, `list` and `kill` in a runtime
# directory of its own under /tmp, then removes it. POSIX sh, so it runs in
# an alpine:3 container too.
#
#   scripts/release-smoke.sh path/to/perch
set -eu

bin=$1
rt=$(mktemp -d /tmp/perch-smoke.XXXXXX)
export PERCH_RUNTIME_DIR="$rt"
trap 'rm -rf "$rt"' EXIT

"$bin" version
id=$("$bin" new -d -- sleep 1000)
echo "created $id"
"$bin" list
"$bin" list | grep "^$id .* running " >/dev/null || {
    echo "release-smoke: $id is not listed as running" >&2
    exit 1
}
"$bin" kill "$id"
# The supervisor ends the program and removes the session; give it 10 s.
i=0
while "$bin" list | grep "^$id " >/dev/null; do
    i=$((i + 1))
    if [ $i -ge 100 ]; then
        echo "release-smoke: $id still listed after kill" >&2
        "$bin" list >&2
        exit 1
    fi
    sleep 0.1
done
echo "release-smoke: ok ($bin)"
