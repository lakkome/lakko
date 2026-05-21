<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko — AI-agent navigation

This file is loaded into the context of every session automatically. Keep it short and precise; details live in `docs/adr/` and in the plan at `~/.claude/plans/1-nested-wreath.md`.

## What Lakko is

A Telegram-class secure messenger with **E2EE by default for everything** (1×1, groups, channels, audio / video calls, broadcasts, audio rooms). Zero-trust server. All clients share a single Rust core + native UI.

## Architectural decisions (single source of truth — `docs/adr/`)

| ADR | Decision |
|---|---|
| [0001](docs/adr/0001-mls-only.md) | MLS (RFC 9420) for everything — 1×1, groups, channels, call media keys |
| [0002](docs/adr/0002-rust-core.md) | Shared Rust core + native UI on every platform |
| [0003](docs/adr/0003-livekit-fork.md) | LiveKit fork as the SFU; SFrame (RFC 9605) for media |
| [0004](docs/adr/0004-anti-censorship.md) | REALITY + MASQUE + Shadowsocks-2022 + WebTransport |
| [0005](docs/adr/0005-contact-invitations.md) | Capability-based invites + 6 scenarios + pseudonym lifecycle |
| [0006](docs/adr/0006-licensing.md) | AGPLv3 monorepo + CLA |
| [0007](docs/adr/0007-multi-device-identity.md) | Two-tier identity (account-level deterministic + per-device random) for multi-device |
| [0008](docs/adr/0008-livekit-fork-strategy.md) | LiveKit fork — operational strategy (extends 0003) |

The full strategic plan and trade-offs live in `~/.claude/plans/1-nested-wreath.md`.
The tactical decomposition of all 6 phases with sprint breakdowns lives in [`docs/ROADMAP.md`](docs/ROADMAP.md).

## Module map

### `crates/` — Rust core

| Crate | Purpose |
|---|---|
| `lakko-core` | Umbrella, re-exports the public API |
| `lakko-crypto` | HPKE, HKDF, AEAD, Argon2id, OPRF, BLAKE3 |
| `lakko-mls` | OpenMLS wrapper, group state, exporters for calls |
| `lakko-proto` | Generated types from `schemas/`, shared with the server |
| `lakko-transport` | quinn (QUIC) + tungstenite (WS) + pluggable obfs trait |
| `lakko-storage` | SQLCipher abstraction, blob store, WAL |
| `lakko-sync` | Multi-device sync, hybrid logical clock, edit CRDT |
| `lakko-calls` | SFrame, jitter buffer, codec negotiation, ICE/TURN |
| `lakko-contacts` | Username discovery (OPRF), invite capabilities, pseudonym lifecycle |
| `lakko-presence` | Blinded per-conversation presence tokens |
| `lakko-push` | Encrypted APNs / FCM / UnifiedPush envelopes |
| `lakko-transparency` | CONIKS-style key transparency log (Merkle tree, signed roots, inclusion proofs) |
| `lakko-call-jwt` | Call-admission JWT (Phase 3b4) — JWS Compact + EdDSA-Ed25519 + JWKS publication, shared between `services/auth` (mint) and FFI crates (parse / verify) |
| `lakko-ffi-uniffi` | UniFFI surface for Swift / Kotlin bindings |
| `lakko-ffi-wasm` | wasm-bindgen surface for Web / Tauri |

### `services/` — Backend (Rust)

| Service | Purpose |
|---|---|
| `auth` | OPAQUE-PAKE + WebAuthn passkey, no PII stored |
| `ds` | MLS Delivery Service (FIFO journal per group) |
| `storage` | Ciphertext blob proxy in front of SeaweedFS |
| `edge` | Broadcast cascade + audio room signaling |
| `discovery` | Username lookup via OPRF |
| `push-gw` | Fan-out APNs / FCM / UnifiedPush |
| `transport-rendezvous` | Signed bootstrap config for anti-censorship |
| `sfu` | LiveKit fork (Go, not Rust) — SFU media routing. Scaffolded in 3b1a; see [`services/sfu/README.md`](services/sfu/README.md) + ADR-0008 |

### Other

- `schemas/` — protobuf schemas; **single source of truth** for the wire format
- `apps/` — clients (iOS / iPadOS / macOS / watchOS / Android / desktop / web), arrive in Phase 1
- `infra/` — single-VPS Hetzner deployment via systemd + system-nginx + system-MariaDB; see `infra/README.md` (migration from lakko_me and daily ops)
- `docs/adr/` — Architecture Decision Records
- `docs/ROADMAP.md` — tactical decomposition of every phase
- `docs/security/` — audit-readiness pack: threat model, crypto overview, MLS state machine invariants (I-1..I-8), call-side cryptography (C-1..C-27 — media-key pipeline + call-admission JWT + link-based invite), known gaps (G-1..G-22). Entry: [`docs/security/README.md`](docs/security/README.md)
- `tools/` — codegen, fuzz harnesses, benchmarks
- `ci/` — GitHub Actions + self-hosted SLSA L3 builders

## Security invariants — must not be violated

1. **No plaintext PII on the server.** Phone / email are **never** stored — not even encrypted. Only OPAQUE-blinded records and a hash for recovery.
2. **The server only sees ciphertext + public artefacts** (KeyPackages, transparency-log entries). No message keys, chain keys, root keys, media keys.
3. **The MLS state machine is sacrosanct.** Any change to `lakko-mls` requires an ADR + property tests + (after Phase 2) an external crypto audit.
4. **No fallback to non-E2EE.** Strict E2EE only — if the handshake does not complete, the connection does not open. No silent downgrade.
5. **The social graph is not materialised on the server.** Add-contact = encrypted envelope to a pseudonym; presence — blinded per-conversation tokens; discovery — OPRF blinded lookup.
6. **Identity rotation** — only via a KT log supersession proof. Old keys do not come back.
7. **Hardware-backed device keys** are mandatory on every platform (Secure Enclave / StrongBox / TPM 2.0 / non-extractable WebCrypto).

## Code-style rules

- **Language: English only** in code, comments, doc strings, commit messages, and documentation (`docs/`, `README.md`, ADRs, migration headers). Russian is allowed only in interactive chat between the maintainer and the AI agent. AI agents must produce English when committing or editing files.
- **Every file** carries the `SPDX-License-Identifier: AGPL-3.0-or-later` header. Rust: `// SPDX-…`; Markdown: `<!-- SPDX-… -->`. No exceptions. CI blocks otherwise.
- **`#![forbid(unsafe_code)]`** on every crate, except `lakko-ffi-uniffi` and `lakko-ffi-wasm` (which carry justified unsafe — document it with a `// SAFETY:` comment).
- **No `unwrap()` / `expect()`** in production code — only in tests and benchmarks. Use `?` and `thiserror` for errors.
- **No `panic!()`** in library code (allowed in `bin` for unrecoverable startup errors).
- **No `println!` / `eprintln!`** — log via the `tracing` crate.
- **`clippy::all`, `clippy::pedantic`, `clippy::nursery`** — all warnings on. `-D warnings` in CI.
- **`cargo fmt`** is mandatory — pre-commit hook + CI gate.
- **Documentation** on public items — `missing_docs` warning on.
- **Do not add dependencies** without need. Every dep is a potential supply-chain vulnerability. `cargo deny` blocks unknown licences.

## Commands (via the justfile)

```
just check                     # cargo check --workspace --all-targets
just test                      # cargo test --workspace
just lint                      # cargo clippy --workspace -- -D warnings
just fmt                       # cargo fmt --all
just fmt-check                 # cargo fmt --all --check
just audit                     # cargo deny check
just reuse                     # reuse lint
just regen-proto               # regenerate types from schemas/
just regen-bindings            # Swift + Kotlin bindings from lakko-ffi-uniffi
just fuzz-check                # smoke-build all fuzz targets on stable
just fuzz <crate> <target>     # run a specific target on nightly + cargo-fuzz
just fuzz-list                 # list all available fuzz targets
just deploy                    # build + install + systemctl restart on the VPS (ssh lakkome@lakko.me)
just restart                   # systemctl restart lakko.target
just restart-svc <svc>         # systemctl restart lakko-svc-<svc>
just logs <svc>                # journalctl -u lakko-svc-<svc> -f
just status                    # systemctl status lakko.target
just ci                        # everything CI does, locally
```

## Fuzz harnesses

`crates/lakko-mls/fuzz/` and `crates/lakko-proto/fuzz/` are separate crates outside the main workspace (they need nightly + libfuzzer-sys). Targets live in `fuzz/fuzz_targets/*.rs`.

Current targets:
- **`lakko-mls/mls_message`** — `MlsMessageIn` wire-format parser (TLS codec)
- **`lakko-mls/key_package`** — `KeyPackage` wire-format parser
- **`lakko-proto/wire_envelope`** — `wire::v1::Envelope` prost decoder
- **`lakko-proto/mls_app_payload`** — `mls_app::v1::ApplicationPayload` prost decoder
- **`lakko-proto/media_call_key`** — `media::v1::CallKeyEpoch` + `CodecCapability` decoders

The `fuzz_api` module in `lakko-mls` (marked `#[doc(hidden)]`) is the only public entry point for harnesses; it is not part of the supported public API.

Fuzzing corpora are not committed (`fuzz/corpus/`, `fuzz/artifacts/` are gitignored); they are spun up locally.

## Local dev

### First time

```bash
# 1. Workspace .env (loaded via dotenvy in each service main() in debug)
cp .env.example .env
# tweak per-service DATABASE_URLs / RUST_LOG if needed

# 2. MariaDB locally — a shared docker container `mariadb:latest` on 3306.
# Each service has its OWN database — sqlx tracks applied migrations in a
# hardcoded `_sqlx_migrations` table per DB, so two services on the same DB
# see each other's migrations as "missing" and refuse to start.
docker exec -i mariadb mariadb -u root -p<root_pw> <<'SQL'
  CREATE DATABASE IF NOT EXISTS lakko_auth
    CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
  CREATE DATABASE IF NOT EXISTS lakko_ds
    CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
  CREATE DATABASE IF NOT EXISTS lakko_discovery
    CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
  CREATE DATABASE IF NOT EXISTS lakko_push_gw
    CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
  CREATE USER IF NOT EXISTS 'lakko'@'%' IDENTIFIED BY 'devpass';
  GRANT ALL ON lakko_auth.*      TO 'lakko'@'%';
  GRANT ALL ON lakko_ds.*        TO 'lakko'@'%';
  GRANT ALL ON lakko_discovery.* TO 'lakko'@'%';
  GRANT ALL ON lakko_push_gw.*   TO 'lakko'@'%';
  FLUSH PRIVILEGES;
SQL
# Find the root password with `docker inspect mariadb | grep ROOT_PASSWORD`.
```

### Running a service

| Method | Command |
|---|---|
| CLI / justfile | `just dev-auth` / `just dev-ds` / `just dev-storage` |
| `cargo run` directly | `cargo run -p lakko-svc-auth` |
| RustRover | local Run Config (Cargo command `run --package lakko-svc-auth`) |

All three paths are identical — env vars come from `.env` via `dotenvy::dotenv()` in `main()` (debug build) either way. RustRover Run Configurations live in `.run/` (gitignored) — every developer creates one locally via the GUI or copies the example below.

Each service reads its own `LAKKO_<NAME>_DATABASE_URL` first, falls back to the generic `DATABASE_URL` if unset. The fallback is for the case of running a single service in isolation; the canonical `.env` has all per-service URLs set.

Production uses a completely different mechanism (systemd `EnvironmentFile=` / `/etc/lakko/lakko.env`); `dotenvy` is gated by `#[cfg(debug_assertions)]` and is not called in release.

#### Example RustRover Run Config

Run → Edit Configurations → `+` → Cargo. Fill in:

| Field | Value |
|---|---|
| Name | `lakko-svc-auth (dev)` |
| Command | `run --package lakko-svc-auth --bin lakko-svc-auth` |
| Working directory | `$PROJECT_DIR$` |
| Environment variables | (optional, can stay empty — `.env` is picked up) |

If you want to duplicate env vars inline (RustRover sets them **before** dotenvy and overrides `.env`):

```
LAKKO_AUTH_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:3306/lakko_auth
LAKKO_BIND=127.0.0.1:8081
RUST_LOG=info,sqlx=warn,lakko_svc_auth=debug,lakko_mls=debug
RUST_BACKTRACE=1
```

### Verify

```bash
curl -i http://127.0.0.1:8081/health   # → 200, "ok"
curl -i http://127.0.0.1:8081/ready    # → 200, "ready" (if the DB is up)
curl -i http://127.0.0.1:8081/version  # → 200, JSON
```

## Pre-commit checklist

- [ ] `just fmt` is clean
- [ ] `just lint` is clean (zero warnings)
- [ ] `just test` is clean
- [ ] `just reuse` is clean (SPDX headers in place)
- [ ] Changes in crypto / mls / proto include an ADR reference in the commit message
- [ ] Changes in `schemas/` are backwards-compatible (proto reserved fields, never delete numbers)
- [ ] If security-relevant — a property test or fuzz target has been added

## Workflow for the AI agent

- **Plan mode** for architectural tasks (a new crate, a protocol change, a schema migration)
- **TodoWrite** for multi-step tasks (>3 steps)
- **Do not bake in future features** — write for the concrete task at hand. The spec roadmap lives in the plan; we implement phase by phase
- **When in doubt about security-critical code** — stop, ask, do not guess
- **Subagents** (Explore, Plan) — for broad research, not for trivial edits
- **Do not commit without a signature** — `commit.gpgsign = true` is required on this repo

## Technical invariants

- **Rust edition 2021**, MSRV pinned in `rust-toolchain.toml`
- **Cargo workspace** — a single `Cargo.toml` at the root, all crates and services live inside it
- **Workspace dependencies** — versions of shared deps pinned in `[workspace.dependencies]`, crates inherit via `dep.workspace = true`
- **Reproducible builds** — the Nix flake pins the entire toolchain; CI checks byte-identicality between runners
