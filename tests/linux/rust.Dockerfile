# SPDX-License-Identifier: GPL-3.0-only
#
# Ubuntu with rustup, the native musl target and the pinned Zig: the local
# route for building and testing Perch's Linux targets. The build context is
# perch-vt/, for its zig-toolchain.toml. From the repository root:
#
#   docker build -f tests/linux/rust.Dockerfile -t perch-rust perch-vt
#   docker run --rm -v "$PWD:/src" -w /src perch-rust sh tests/linux/musl-check.sh

FROM ubuntu:24.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl file git xz-utils \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/opt/zig:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable -c rustfmt,clippy \
    && rustup target add "$(uname -m)-unknown-linux-musl"

COPY zig-toolchain.toml /tmp/zig-toolchain.toml
RUN set -eu; \
    key="$(uname -m)-linux"; \
    field() { awk -v s="[tarballs.$key]" -v n="$1" '$0 == s { f = 1; next } /^\[/ { f = 0 } f && $1 == n { gsub(/"/, "", $3); print $3 }' /tmp/zig-toolchain.toml; }; \
    curl -fsSL "$(field url)" -o /tmp/zig.tar.xz; \
    echo "$(field sha256)  /tmp/zig.tar.xz" | sha256sum -c -; \
    mkdir /opt/zig; tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1; \
    rm /tmp/zig.tar.xz; zig version
