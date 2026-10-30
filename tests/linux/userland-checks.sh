#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Old and minimal userlands: the musl release binary creates, attaches to
# and kills a session on rockylinux:8 and alpine:3, on Docker's native
# architecture.
#
#   tests/linux/userland-checks.sh [PERCH_BINARY]
#
# The binary defaults to target/linux-hosts/perch, built by
# tests/linux/build-musl.sh if missing.
set -eu

cd "$(git rev-parse --show-toplevel)"
bin=${1:-target/linux-hosts/perch}
if [ ! -x "$bin" ]; then
    [ "$bin" = target/linux-hosts/perch ] || { echo "no binary at $bin" >&2; exit 2; }
    tests/linux/build-musl.sh "$bin"
fi

# Runs inside each image with a terminal (docker -t), as root.
inner='
set -eu
perch version
perch new -d --name u -- sh -c "echo USERLAND-\$((40 + 2))-BANNER; exec sleep 1000" >/dev/null
pid=$(perch info u --json | sed -n "s/^  \"pid\": \([0-9]*\).*/\1/p")
[ -d "/proc/$pid" ]
# Detach the client from outside once it has had time to restore.
(sleep 3; perch detach u) &
perch attach u
echo
perch list --json | grep "\"name\": \"u\"" >/dev/null
perch kill u
i=0
while [ -d "/proc/$pid" ] && [ $i -lt 50 ]; do sleep 0.2; i=$((i + 1)); done
[ ! -d "/proc/$pid" ] || { echo "the program outlived kill"; exit 1; }
[ "$(perch list --json | grep -c "\"name\"" || true)" -eq 0 ] || { echo "the session is still listed"; exit 1; }
echo USERLAND-DONE
'

name=perch-userland-$$
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

failed=0
for image in rockylinux:8 alpine:3; do
    echo "== $image"
    docker pull -q "$image" >/dev/null
    docker create -t --name "$name" "$image" sh -c "$inner" >/dev/null
    docker cp "$bin" "$name:/usr/local/bin/perch"
    out=$(docker start -a "$name" 2>&1) && rc=0 || rc=$?
    docker rm -f "$name" >/dev/null
    printf '%s\n' "$out" | tr -d '\r' | grep -E '^perch ' | sed 's/^/    /'
    if [ $rc -eq 0 ] && printf '%s' "$out" | grep -q USERLAND-42-BANNER &&
        printf '%s' "$out" | grep -q USERLAND-DONE; then
        echo "    created; attach restored USERLAND-42-BANNER; detached; killed"
        echo "PASS userland $image"
    else
        printf '%s\n' "$out" | tail -20 | cat -v
        echo "FAIL userland $image (exit $rc)"
        failed=$((failed + 1))
    fi
done
[ $failed -eq 0 ]
