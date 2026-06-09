# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

# Multi-target Dockerfile for the Lakko core HTTP services. One shared
# `builder` stage compiles every service binary from the workspace ONCE;
# thin per-service runtime stages each carry a single binary. `docker
# compose` selects a stage with `target:`, so buildkit reuses the common
# builder layer across all images instead of rebuilding the workspace once
# per service.
#
# Covers: auth, ds, discovery, storage, push-gw, channels. The SFU has its
# own image (services/sfu/Dockerfile) because it carries a different runtime
# (UDP media plane); the local compose references that one directly. edge +
# transport-rendezvous are later-phase services and are not wired into the
# local compose yet.
#
# Consumed by infra/docker/docker-compose.yml. Mirrors services/sfu/Dockerfile.

# -----------------------------------------------------------
# Stage 1 - builder (shared across all runtime targets)
# -----------------------------------------------------------
FROM rust:1.84-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .

# Build every core service binary in one invocation so the workspace is
# compiled once and the dependency graph is shared across targets.
RUN cargo build --release \
    --bin lakko-svc-auth \
    --bin lakko-svc-ds \
    --bin lakko-svc-discovery \
    --bin lakko-svc-storage \
    --bin lakko-svc-push-gw \
    --bin lakko-svc-channels

# -----------------------------------------------------------
# Shared runtime base
# -----------------------------------------------------------
FROM debian:bookworm-slim AS runtime-base

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    tini \
    curl \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 10001 lakko \
    && useradd  --uid 10001 --gid 10001 --shell /sbin/nologin --no-create-home lakko

ENV RUST_LOG=info,sqlx=warn
USER lakko:lakko

# -----------------------------------------------------------
# Per-service runtime stages
# -----------------------------------------------------------
FROM runtime-base AS auth-runtime
COPY --from=builder /build/target/release/lakko-svc-auth /usr/local/bin/lakko-svc-auth
ENV LAKKO_BIND=0.0.0.0:8081
EXPOSE 8081
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/lakko-svc-auth"]

FROM runtime-base AS ds-runtime
COPY --from=builder /build/target/release/lakko-svc-ds /usr/local/bin/lakko-svc-ds
ENV LAKKO_BIND=0.0.0.0:8082
EXPOSE 8082
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/lakko-svc-ds"]

FROM runtime-base AS storage-runtime
COPY --from=builder /build/target/release/lakko-svc-storage /usr/local/bin/lakko-svc-storage
ENV LAKKO_BIND=0.0.0.0:8083 \
    LAKKO_BLOB_DIR=/var/lib/lakko/blobs
EXPOSE 8083
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/lakko-svc-storage"]

FROM runtime-base AS discovery-runtime
COPY --from=builder /build/target/release/lakko-svc-discovery /usr/local/bin/lakko-svc-discovery
ENV LAKKO_BIND=0.0.0.0:8084
EXPOSE 8084
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/lakko-svc-discovery"]

FROM runtime-base AS push-gw-runtime
COPY --from=builder /build/target/release/lakko-svc-push-gw /usr/local/bin/lakko-svc-push-gw
ENV LAKKO_BIND=0.0.0.0:8085
EXPOSE 8085
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/lakko-svc-push-gw"]

FROM runtime-base AS channels-runtime
COPY --from=builder /build/target/release/lakko-svc-channels /usr/local/bin/lakko-svc-channels
ENV LAKKO_CHANNELS_BIND=0.0.0.0:8086
EXPOSE 8086
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/lakko-svc-channels"]
