# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

# Multi-stage Dockerfile for `lakko-svc-witness` (ADR-0020).
#
# Mirrors services/transparency/Dockerfile: a full-toolchain builder stage
# compiles the one binary; a bookworm-slim runtime carries only it + the CA
# store + libssl3 + curl (healthcheck) + a tini PID-1. Migrations are embedded
# (sqlx::migrate!) and all SQL is runtime-checked, so the build needs NO
# database. Built by ci/recovery-e2e/docker-compose.yml; the build context MUST
# be the workspace root.

# -----------------------------------------------------------
# Stage 1 — builder
# -----------------------------------------------------------
FROM rust:1.84-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .
RUN cargo build --release --bin lakko-svc-witness

# -----------------------------------------------------------
# Stage 2 — runtime
# -----------------------------------------------------------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    libssl3 \
    tini \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 10001 lakko \
    && useradd  --uid 10001 --gid 10001 --shell /sbin/nologin --no-create-home lakko

COPY --from=builder /build/target/release/lakko-svc-witness /usr/local/bin/lakko-svc-witness

ENV LAKKO_BIND=0.0.0.0:8088 \
    RUST_LOG=info,lakko_svc_witness=info

EXPOSE 8088

USER lakko:lakko

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/lakko-svc-witness"]
