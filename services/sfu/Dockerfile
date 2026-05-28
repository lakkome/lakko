# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

# Multi-stage Dockerfile for `lakko-svc-sfu` (G3-4).
#
# Stage 1 (`builder`) — full rust toolchain on Debian
# bookworm. We compile the entire workspace once and strip
# everything down to the single binary the runtime stage
# needs.
#
# Stage 2 (`runtime`) — bookworm-slim with only the
# certificate store + libssl3 (the latter is pulled in by
# rustls-with-system-cas + tokio-rustls' optional native
# verifier; SFU itself uses ring, but transitive deps like
# `reqwest` for the JWKS fetcher want it on the runtime
# path).
#
# This image is the artefact `ci/sfu-e2e/docker-compose.yml`
# brings up + the production deployment (`infra/`) starts
# under systemd's container runtime when the migration
# from bare-metal lands.

# -----------------------------------------------------------
# Stage 1 — builder
# -----------------------------------------------------------
FROM rust:1.84-bookworm AS builder

# Required for some transitive crates that link against
# system libs at build time (sqlx-mariadb, openssl-sys
# fallback). Kept minimal — anything more bloats the final
# image's build context.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Workspace layout — we want a build that does NOT redownload
# the registry on every iteration. The canonical
# cargo-chef pattern is the cleanest but introduces an extra
# dependency; for this single-binary image the simpler
# "copy everything, build once" works fine and keeps the
# Dockerfile auditable on first read.
COPY . .

# Build the binary in release mode. `--bin lakko-svc-sfu`
# selects just the one we care about; the workspace is
# only present so transitive deps can resolve.
RUN cargo build --release --bin lakko-svc-sfu

# -----------------------------------------------------------
# Stage 2 — runtime
# -----------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# `ca-certificates` is required for the JWKS HTTPS fetch.
# `libssl3` is the dynamic OpenSSL runtime; pulled by some
# transitive crates that still go through openssl-sys.
# `tini` gives us a PID-1 init that reaps zombies properly
# — without it, ^C from `docker compose down` doesn't
# propagate cleanly to the tokio runtime.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    tini \
    && rm -rf /var/lib/apt/lists/*

# Non-root runtime user. The UID/GID is fixed at 10001 so
# host-side bind-mounts (production: `/etc/lakko/lakko.env`)
# have a stable target.
RUN groupadd --gid 10001 lakko \
    && useradd  --uid 10001 --gid 10001 --shell /sbin/nologin --no-create-home lakko

COPY --from=builder /build/target/release/lakko-svc-sfu /usr/local/bin/lakko-svc-sfu

# Default env vars — overridable at `docker run -e ...` or
# via compose's `environment:` block. The bind addresses
# default to `0.0.0.0` (vs the binary's local-only
# `127.0.0.1` default) because inside a container the bind
# *is* the boundary the orchestrator exposes.
ENV LAKKO_SFU_BIND=0.0.0.0:7880 \
    LAKKO_SFU_METRICS_BIND=0.0.0.0:9641 \
    RUST_LOG=info,lakko_svc_sfu=info

EXPOSE 7880 9641

USER lakko:lakko

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/lakko-svc-sfu"]
