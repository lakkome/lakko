# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# Linux x86_64 build image for the test stack. Matches the test server
# (Ubuntu 22.04: glibc 2.35, libssl3), so the binaries link against the same
# system libraries they run with. `lakko-svc-auth` needs libssl (webauthn-rs)
# and `lakko-svc-sfu` builds aws-lc-sys (str0m), hence the C toolchain.
FROM --platform=linux/amd64 ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config libssl-dev cmake clang perl git curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
# Keep in lock-step with rust-toolchain.toml.
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.95.0 \
    && rustup component add rustfmt clippy rust-src --toolchain 1.95.0
