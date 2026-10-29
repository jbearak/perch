#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Seeds fuzz/corpus/restore from the fidelity fixtures' streams: each one,
# cut to 64 KiB, behind the five header bytes `check_fuzz_input` reads
# (model and client sizes, mode and split). Run from the repository root.
set -eu
out=fuzz/corpus/restore
mkdir -p "$out"
for f in tests/fidelity/fixtures/*/stream.bin; do
    name=$(basename "$(dirname "$f")")
    { printf '\046\026\046\026\100'; head -c 65536 "$f"; } >"$out/$name"
done
echo "seeded $(ls "$out" | wc -l | tr -d ' ') inputs in $out"
