# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

# Multi-stage Dockerfile for `lakko-svc-transparency`.
#
# Mirrors `services/sfu/Dockerfile`: a full-toolchain builder stage compiles
# the one binary; a `bookworm-slim` runtime stage carries only it + the CA
# store + libssl3 + a tini PID-1. Migrations are embedded at compile time
# (`sqlx::migrate!("./migrations")`) and all SQL is runtime-checked
# (`sqlx::query`, not the `query!` macro), so the build needs NO database.
#
# Brought up by `ci/recovery-e2e/docker-compose.yml` (the recovery + KT-log
# live e2e). The build context MUST be the workspace root so the `COPY . .`
# step sees every workspace crate.

# -----------------------------------------------------------
# Stage 1 — builder
# -----------------------------------------------------------
FROM rust:1.84-bookworm AS builder

# Transitive build-time system libs (sqlx-mariadb, openssl-sys fallback).
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .
RUN cargo build --release --bin lakko-svc-transparency

# -----------------------------------------------------------
# Stage 2 — runtime
# -----------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# `ca-certificates` + `libssl3` for transitive crates on openssl-sys; `tini`
# is a PID-1 that reaps zombies + propagates `docker compose down` to tokio.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    libssl3 \
    tini \
    && rm -rf /var/lib/apt/lists/*

# Non-root runtime user, fixed UID/GID for stable bind-mount targets.
RUN groupadd --gid 10001 lakko \
    && useradd  --uid 10001 --gid 10001 --shell /sbin/nologin --no-create-home lakko

COPY --from=builder /build/target/release/lakko-svc-transparency /usr/local/bin/lakko-svc-transparency

# Inside a container the bind IS the orchestrator boundary, so default to
# 0.0.0.0 (vs the binary's local-only default). Overridable via compose env.
ENV LAKKO_BIND=0.0.0.0:8087 \
    RUST_LOG=info,lakko_svc_transparency=info

EXPOSE 8087

USER lakko:lakko

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/lakko-svc-transparency"]
