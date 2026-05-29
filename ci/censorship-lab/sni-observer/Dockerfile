# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors

# Multi-stage Dockerfile for the G5-7 SNI observer.
#
# Shares the same shape as `services/sfu/Dockerfile`:
# rust:1.84-bookworm builder → debian:bookworm-slim
# runtime with `tini` PID-1 + a fixed non-root user.

FROM rust:1.84-bookworm AS builder

WORKDIR /build
COPY Cargo.toml ./
COPY src ./src

RUN cargo build --release --bin sni-observer

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tini \
    netcat-openbsd \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 10001 lakko \
    && useradd  --uid 10001 --gid 10001 --shell /sbin/nologin --no-create-home lakko

COPY --from=builder /build/target/release/sni-observer /usr/local/bin/sni-observer

# The compose stack mounts a host directory at
# `/var/log/sni-observer/` so the test harness can read
# the log file out-of-band. The default `LAKKO_SNI_LOG`
# points at a file under that path.
RUN mkdir -p /var/log/sni-observer \
    && chown lakko:lakko /var/log/sni-observer

ENV LAKKO_SNI_BIND=0.0.0.0:8443 \
    LAKKO_SNI_LOG=/var/log/sni-observer/observed.jsonl \
    RUST_LOG=info

EXPOSE 8443

USER lakko:lakko

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/sni-observer"]
