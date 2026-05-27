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
| [0003](docs/adr/0003-livekit-fork.md) | LiveKit fork as the SFU; SFrame (RFC 9605) for media — **superseded by 0011** (SFU choice); SFrame + MLS-exporter parts still in force |
| [0004](docs/adr/0004-anti-censorship.md) | REALITY + MASQUE + Shadowsocks-2022 + WebTransport |
| [0005](docs/adr/0005-contact-invitations.md) | Capability-based invites + 6 scenarios + pseudonym lifecycle |
| [0006](docs/adr/0006-licensing.md) | AGPLv3 monorepo + CLA |
| [0007](docs/adr/0007-multi-device-identity.md) | Two-tier identity (account-level deterministic + per-device random) for multi-device |
| [0008](docs/adr/0008-livekit-fork-strategy.md) | LiveKit fork — operational strategy — **superseded by 0011**; `CallJwtClaims` shape survives in `crates/lakko-call-jwt` |
| [0009](docs/adr/0009-link-based-call-invitations.md) | Link-based call invitations protocol (ADR-0005 scenario E expansion + welcome-ferry + multi-party KP-forward) |
| [0010](docs/adr/0010-channels-two-tier-broadcast.md) | Channels — two-tier PublisherGroup + sender-key broadcast for >50k subscribers (Phase 4 design) |
| [0011](docs/adr/0011-rust-sfu.md) | Rust SFU on `webrtc-rs` / `str0m` — supersedes 0003 (SFU choice) + 0008. Stack winner decided in S-3-B-spike |

The full strategic plan and trade-offs live in `~/.claude/plans/1-nested-wreath.md`.
The tactical decomposition of all 6 phases with sprint breakdowns lives in [`docs/ROADMAP.md`](docs/ROADMAP.md).
The Phase 3 closeout punch list — gap inventory + sprint waves with exit criteria — lives in [`docs/ROADMAP-PHASE3-CLOSEOUT.md`](docs/ROADMAP-PHASE3-CLOSEOUT.md).

## Module map

### `crates/` — Rust core

| Crate | Purpose |
|---|---|
| `lakko-core` | Umbrella, re-exports the public API |
| `lakko-crypto` | HPKE, HKDF, AEAD, Argon2id, OPRF, BLAKE3 |
| `lakko-mls` | OpenMLS wrapper, group state, exporters for calls |
| `lakko-proto` | Generated types from `schemas/`, shared with the server |
| `lakko-transport` | quinn (QUIC) + tungstenite (WS) + pluggable obfs trait + signed bootstrap rendezvous + DoH fetcher + chunked AEAD stream + SS-2022 TCP transport + ECH config list parser (Phase 5: G5-5 + G5-5-doh + G5-aead-stream + G5-3-header + S-5-shadowsocks-tcp + G5-6 all done; G5-3 complete; G5-6 parser-side complete, rustls wire-up pending live ECH endpoint). `DohFetcher` resolves bootstrap domains over JSON-DoH (Cloudflare / Google / Quad9 / AdGuard), then IP-pinned HTTPS GET + 4-stage `BootstrapConfig::verify`. `DohFetcher::fetch_pinned_bytes(domain, path, max_bytes)` exposes the same IP-pin pipeline as a generic `Vec<u8>` getter with body-size cap — downstream callers (TUF metadata fetch via `lakko_tuf::urls`, KeyPackage bundles, signed identity blobs) reuse it rather than re-implementing IP pinning; refuses payloads exceeding the cap with `DohError::BodyTooLarge`. `AeadStreamWriter` / `AeadStreamReader` ship chunked ChaCha20-Poly1305 framing (16 KiB max chunk). `ShadowsocksTransport::connect` + `accept_inbound` deliver a working SS-2022 client/server stack over TCP with full handshake (salt + sealed prelude) + per-direction subkey separation + AEAD body. `parse_ech_config_list` decodes draft-ietf-tls-esni-18 §4 wire format with strict version pinning (0xfe0d only), bounded allocations (16 KiB outer / 16 inner configs / 4 KiB per config), full field validation (cipher_suites non-empty + 4-byte aligned, public_name 1..=255 + UTF-8, max_name_length non-zero). REALITY v2 Path C — pre-shared-key model (G5-1-v2): publisher distributes 32-byte `tag_secret` per `(client, short_id)` via signed bootstrap config; first two pieces of `S-5-reality-driver-v2` landed: (1) `reality::cert::AcceptAnyCertVerifier` (pub(crate)-scoped, REALITY-aware safety contract: PKI is bypassed because the cryptographic auth lives in session_id + ServerHello.Random HMAC chain, not in TLS PKI); (2) `reality::driver` with `build_reality_client_config` + `drive_handshake` async byte-intercept loop manually driving `rustls::ClientConnection` via `write_tls`/`read_tls`, splicing `patch_client_hello_session_id` on the first outgoing batch + `extract_server_hello_random` + `verify_server_response` on the first inbound record, refusing with typed `RealityDriverError::ServerAuth` if HMAC doesn't match (active-probe detection). 6 unit tests on the HandshakeState extraction-readiness logic. client builds session_id auth as `HMAC(HKDF(tag_secret, "lakko/reality/v2"), short_id || ts)` truncated to 16B. Mutual auth: server proves tag_secret knowledge via `HMAC(mac_key, "v2-server-response" || short_id || ts)` in TLS 1.3 ServerHello.Random (extracted by client via `verify_server_response`). v2 dropped ECDH from v1 because rustls 0.23 doesn't expose TLS 1.3 ephemeral-X25519 hook — Path C unblocks driver work to use stock rustls; cost is wire-incompatibility with upstream xtls/REALITY (Lakko closed ecosystem) + loss of authenticator-level forward-secrecy (cover TLS FS preserved, mitigated by tag_secret rotation through bootstrap config TTL). Strongly-typed `RealityConfig` (with `tag_secret_b64` instead of v1's `server_pubkey_b64`) / `RealityTagSecret` / `RealityShortId`, eager parse-time validation. TLS wire-format parsers (G5-1-wire) `patch_client_hello_session_id` + `extract_server_hello_random` per RFC 8446 §4.1.2 / §4.1.3 / §5.1. 39 unit tests in `crates/lakko-transport/src/reality/` (13 authenticator + 7 config + 5 transport + 13 handshake-wire). `RealityTransport::connect` still returns `NotImplemented` pending `S-5-reality-driver-v2` (assemble cert verifier + async handshake driver + post-handshake Connection wrapper + wire connect). Now buildable with stock rustls. WebTransport real impl (G5-4): `WebTransportPt` over `web-transport-quinn` 0.11, cert-hash pinning (W3C `serverCertificateHashes` mode, SHA-256 of full DER cert) + system-roots fallback, u32-BE length-prefixed framing with `MAX_FRAME_BYTES` enforcement, 8 unit tests. `cert_hashes_b64` is the wire-config field name (renamed from `spki_pins_b64` for spec alignment — cert-hash ≠ SPKI). MASQUE foundation (G5-2): `MasqueConfig` parser (eager validation of proxy https + inner_target host:port + bearer), `build_connect_udp_request` emitting RFC 9298 §3 pseudo-headers + RFC 9297 §3.4 `capsule-protocol`, HTTP Datagram codec per RFC 9297 §3 + RFC 9298 §3 (QUIC varint Context ID, ctx=0 for CONNECT-UDP, 65 KiB hard cap). 28 unit tests. `MasqueTransport::connect` still returns `NotImplemented` pending h3+quinn handshake sub-sprint. See [`docs/security/transport.md`](docs/security/transport.md) T-1..T-15 |
| `lakko-storage` | SQLCipher abstraction, blob store, WAL |
| `lakko-sync` | Multi-device sync, hybrid logical clock, edit CRDT |
| `lakko-calls` | SFrame, jitter buffer, codec negotiation, ICE/TURN |
| `lakko-channels` | Two-tier broadcast. Publisher path: PublisherGroup exporter seed + per-message AEAD + Ed25519 signature on every post. Subscriber path: HPKE seed envelope (`seal_seed` / `open_seed`) — full surface exposed via UniFFI + wasm-bindgen (G4-3c). See [ADR-0010](docs/adr/0010-channels-two-tier-broadcast.md) |
| `lakko-contacts` | Username discovery (OPRF), invite capabilities, pseudonym lifecycle |
| `lakko-presence` | Blinded per-conversation presence tokens |
| `lakko-push` | Encrypted APNs / FCM / UnifiedPush envelopes |
| `lakko-transparency` | CONIKS-style key transparency log (Merkle tree, signed roots, inclusion proofs) |
| `lakko-call-jwt` | Call-admission JWT (Phase 3b4) — JWS Compact + EdDSA-Ed25519 + JWKS publication, shared between `services/auth` (mint) and FFI crates (parse / verify) |
| `lakko-tuf` | TUF v1.1 client-side metadata verifier (GA-5 scaffold + rotation + target-bytes verifier + defense-in-depth delegation rejection + URL conventions module). Root / Snapshot / Targets / Timestamp Ed25519-signed envelopes, chain-of-trust walk via `verify_chain`, version-rollback detection, expiry check, threshold-of-keys per role. `verify_root_chain` walks the root version chain — each new root must be signed by the previous root's `root` role, anchoring at a binary-pinned trust-anchor key id set. `verify_target_bytes` closes the last metadata-side gate — given a `TargetMeta` from `verify_chain` + the actual fetched binary, confirms length + SHA-256 digest match; unknown hash algos skipped (forward-compat); refuses if no known algo got verified. `Targets.delegations` parsed as opaque `serde_json::Value`; `verify_chain` actively refuses any non-None delegations block (the scaffold cannot yet walk a delegation tree, so an attacker who compromised the top-level targets signing key could otherwise indirect through a weaker delegated key — refuse rather than ignore). `urls` module pins the TUF v1.1 §5 non-consistent-snapshot URL convention (`{base}/{version}.root.json` / `{base}/timestamp.json` / `{base}/snapshot.json` / `{base}/targets.json` / `{base}/targets/{name}`) so platform-specific install glue does not re-derive it from the spec; `target_artifact_url` carries the path-traversal defence (refuses empty name, NUL, `/`, `\\`, `..`). Network fetch + delegated targets walk + mirror selection + platform-side install glue (atomic-swap binary replacement) are out-of-scope follow-ups |
| `lakko-ffi-uniffi` | UniFFI surface for Swift / Kotlin bindings |
| `lakko-ffi-wasm` | wasm-bindgen surface for Web / Tauri |

### `services/` — Backend (Rust)

| Service | Purpose |
|---|---|
| `auth` | OPAQUE-PAKE + WebAuthn passkey, no PII stored |
| `ds` | MLS Delivery Service (FIFO journal per group) |
| `storage` | Ciphertext blob proxy in front of SeaweedFS |
| `channels` | Phase 4 channel-broadcast backend. Publisher / subscriber / rotation paths complete. `ChannelStore` trait with two impls: `InMemoryChannelStore` (test default, dev fallback) + `SqlChannelStore` (MariaDB, production; migrations under `services/channels/migrations/000{1,2}_*.sql`). Toggled at boot via `LAKKO_CHANNELS_DATABASE_URL`. Background prune loop covers both posts (30-day TTL) + stale subscribers (90-day TTL via `last_active_at`, FK cascade drops envelope), hourly cadence; toggles: `LAKKO_CHANNELS_RETENTION_SECS` / `LAKKO_CHANNELS_SUBSCRIBER_RETENTION_SECS` / `LAKKO_CHANNELS_PRUNE_INTERVAL_SECS`. Server stays opaque — never decrypts `ChannelPost` bytes or HPKE seed envelopes; `lakko-channels` crate is `dev-dep` only. See [`docs/security/calls.md`](docs/security/calls.md) C-40..C-42 |
| `edge` | Broadcast cascade + audio room signaling |
| `discovery` | Username lookup via OPRF |
| `push-gw` | Fan-out APNs / FCM / UnifiedPush |
| `transport-rendezvous` | Signed bootstrap config for anti-censorship |
| `sfu` | Minimal Rust SFU on `str0m` (stack chosen in `S-3-B-spike`). **Signaling half shipped** — Lakko-native typed wire protocol (welcome / peer-joined / peer-left / signal-relay / ping/pong / error), `lakko-call-jwt` admission, per-peer mpsc fanout, cross-call isolation, 32 unit + 9 integration tests. **Media half pending** (`S-3-B-routing-media`) — gated on a working WebRTC pod on at least one client. See [`services/sfu/README.md`](services/sfu/README.md) + [ADR-0011](docs/adr/0011-rust-sfu.md) + [`docs/security/calls.md`](docs/security/calls.md) C-39 |

### Other

- `schemas/` — protobuf schemas; **single source of truth** for the wire format
- `apps/` — clients (iOS / iPadOS / macOS / watchOS / Android / desktop / web), arrive in Phase 1
- `infra/` — single-VPS Hetzner deployment via systemd + system-nginx + system-MariaDB; see `infra/README.md` (migration from lakko_me and daily ops)
- `docs/adr/` — Architecture Decision Records
- `docs/ROADMAP.md` — tactical decomposition of every phase
- `docs/security/` — audit-readiness pack: threat model, crypto overview, MLS state machine invariants (I-1..I-8), call-side cryptography (C-1..C-44 — media-key pipeline + call-admission JWT + link-based invite + owner fingerprint + server-handler drift sentinels + URL discrimination + composer/acceptor pipelines + Welcome-ferry envelope + KP-forward envelope + incoming-envelope dispatcher + 1×1 and multi-party E2E chain + owner-handover protocol + SFrame keying math + RFC 9605 wire-header round-trip + SFU signaling-layer zero-payload-inspection / cross-call isolation / backpressure semantics + channels-backend publisher-path / subscriber-handshake opacity + atomic rotation + FFI subscriber-side surface opacity + MLS-derived channel seed FFI surface), transport layer (T-1..T-12 — pluggable wire identifiers + scaffold loud-fail + selector race + 4-stage rendezvous verify + canonical signing bytes + kebab-case kind tag + binary-pinned trust anchor + DoH-bootstrap chain integrity + chunked AEAD stream primitive + SS-2022 outer header layer + SS-2022 TCP wiring + ECH config list parser), known gaps (G-1..G-26 — including the cargo-audit baseline for transitive deps + RFC 9605 wire-header interop gap **now partially closed**). Entry: [`docs/security/README.md`](docs/security/README.md); consolidated invariant + gap cross-reference: [`docs/security/invariant-index.md`](docs/security/invariant-index.md)
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
just fuzz <fuzz_dir> <target>  # run a specific target on nightly + cargo-fuzz
just fuzz-quick                # run EVERY target for 60s each (local mirror of nightly CI workflow)
just fuzz-list                 # list all available fuzz targets
just deploy                    # build + install + systemctl restart on the VPS (ssh lakkome@lakko.me)
just restart                   # systemctl restart lakko.target
just restart-svc <svc>         # systemctl restart lakko-svc-<svc>
just logs <svc>                # journalctl -u lakko-svc-<svc> -f
just status                    # systemctl status lakko.target
just ci                        # everything CI does, locally
```

## Fuzz harnesses

`crates/lakko-mls/fuzz/`, `crates/lakko-proto/fuzz/`, `crates/lakko-contacts/fuzz/`, `crates/lakko-channels/fuzz/`, `crates/lakko-transport/fuzz/`, `crates/lakko-tuf/fuzz/`, `crates/lakko-call-jwt/fuzz/`, `crates/lakko-backup/fuzz/`, and `services/sfu/fuzz/` are separate crates outside the main workspace (they need nightly + libfuzzer-sys). Targets live in `fuzz/fuzz_targets/*.rs`.

Current targets:
- **`lakko-mls/mls_message`** — `MlsMessageIn` wire-format parser (TLS codec)
- **`lakko-mls/key_package`** — `KeyPackage` wire-format parser
- **`lakko-proto/wire_envelope`** — `wire::v1::Envelope` prost decoder
- **`lakko-proto/mls_app_payload`** — `mls_app::v1::ApplicationPayload` prost decoder
- **`lakko-proto/media_call_key`** — `media::v1::CallKeyEpoch` + `CodecCapability` decoders
- **`lakko-contacts/contact_request_decode_and_verify`** — outermost stranger-inbox parser (signature + HPKE-sealed envelope)
- **`lakko-contacts/contact_ack_decode_and_verify`** — ContactAck parser (Bob → Alice reply after accept)
- **`lakko-contacts/invite_capability_decode_and_verify`** — contact-invite (Phase 1.18) capability parser
- **`lakko-contacts/call_invite_capability_decode_and_verify`** — call-invite (Phase 3c3) capability parser
- **`lakko-contacts/call_welcome_unwrap`** — owner-side welcome-envelope domain-separator parser
- **`lakko-contacts/call_kp_forward_unwrap`** — owner-side KP-forward envelope parser (subsequent joiners)
- **`lakko-contacts/incoming_envelope_dispatch`** — composite owner-side polling-loop dispatcher
- **`lakko-channels/post_decode`** — wire-format parser for `ChannelPost` (Phase 4a)
- **`lakko-channels/post_verify_and_decrypt`** — full receiver pipeline (decode → Ed25519 verify → AEAD open)
- **`lakko-channels/subscribe_open_seed`** — subscriber-side HPKE seed-envelope opener (Phase 4b)
- **`lakko-channels/epoch_id_from_be_bytes`** — 4-byte big-endian epoch-id parser
- **`lakko-transport/shadowsocks_decode_request`** — SS-2022 client-to-server outer-header parser (Phase 5, T-10 surface; fixed-zero PSK + fresh ReplayWindow)
- **`lakko-transport/shadowsocks_decode_response`** — SS-2022 server-to-client outer-header parser (fixed-zero PSK + fixed expected request-salt; exercises the `WrongResponseSalt` branch)
- **`lakko-transport/rendezvous_decode_and_verify`** — full attacker-controlled rendezvous pipeline: `serde_json::from_slice::<BootstrapConfig>` → on Ok, `BootstrapConfig::verify` with fixed trust anchor + fixed `now` (T-4 / T-5 / T-7 / T-8 surfaces)
- **`lakko-transport/ech_decode`** — ECH config list parser (T-12 surface; DNS HTTPS RR bytes are attacker-controlled if the DNS path is poisoned)
- **`lakko-backup/envelope_v2_header_of`** — v2 backup envelope plaintext chain-header parse (G-11 surface; cheapest attacker-touchable path, no recovery phrase needed)
- **`lakko-backup/envelope_v2_open`** — full v2 AEAD-open path under a fixed phrase (covers schema-version gate + salt/nonce slicing + chain-header decode + ct_len arithmetic + Argon2id KEK + AEAD-open)
- **`lakko-svc-sfu/client_frame_decode`** — `serde_json::from_slice::<ClientFrame>` on attacker-controlled WS text frame bytes (C-39 surface; admitted-but-malicious peer reaches this parser post-JWT, pre-routing)
- **`lakko-tuf/signed_root_from_json`** — outermost TUF metadata parser for the `Root` role envelope (GA-5)
- **`lakko-tuf/signed_snapshot_from_json`** — outermost TUF metadata parser for the `Snapshot` role envelope
- **`lakko-tuf/signed_targets_from_json`** — outermost TUF metadata parser for the `Targets` role envelope (widest deserialiser surface; carries the targets `BTreeMap`)
- **`lakko-tuf/signed_timestamp_from_json`** — outermost TUF metadata parser for the `Timestamp` role envelope
- **`lakko-call-jwt/jwt_parse_unverified`** — JWS Compact parser (3 dot-segments → base64url-decode → JSON-decode header + claims). UTF-8 gate at the harness edge keeps coverage focused on the base64 / JSON parser branches
- **`lakko-call-jwt/jwt_verify_with_jwks`** — full JWS pipeline: signature path + kid lookup + algorithm-confusion check + base64 of signature + `Signature::from_bytes` length check + `exp` clock comparison. Fixed all-zero-seed single-key JWKS + fixed `now_secs` so the harness is reproducible and the fuzzer can learn the deterministic `kid`
- **`lakko-call-jwt/jwks_parse`** — `parse_jwks(doc_bytes)` JSON decoder; the auth service publishes the JWKS at `/.well-known/jwks.json` and a compromised publish endpoint can serve arbitrary bytes

The `fuzz_api` module in `lakko-mls` (marked `#[doc(hidden)]`) is the only public entry point for harnesses; it is not part of the supported public API.

Fuzzing corpora are not committed (`fuzz/corpus/`, `fuzz/artifacts/` are gitignored); they are spun up locally.

The `.github/workflows/fuzz-quick.yml` workflow actually RUNS every fuzz target for a bounded budget (60s default, configurable via `workflow_dispatch`) on nightly cron (03:00 UTC) — `just fuzz-check` only smoke-builds. Matrix runs one job per fuzz crate (9 jobs, ~30 targets total); crashes upload to the workflow run page as 14-day-retention artifacts. Concurrency limited to 1 so overlapping nightly runs don't pile up.

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
- **Russian-chat commit-message rule** — when the interactive chat is in Russian and the turn produced changes in code or documentation (anything `git status` would show as modified / added / deleted), the assistant MUST end its reply with a ready-to-paste English commit message in a fenced block. Skip the block only when there are no file changes.
- **No AI signature lines in commit messages.** Never append `Co-Authored-By: Claude …`, `Generated with Claude …`, or any similar attribution footer to commit messages or PR descriptions. The maintainer authors the commit.
- **No emoji / icons in commit messages or logs.** Commit messages, PR descriptions, and every log line emitted via the `tracing` crate from **any** client app (web / iOS / iPadOS / macOS / watchOS / Android / desktop) **and** every backend service (`services/*`) MUST be plain text — no emoji, no Unicode pictographs, no decorative icons. ASCII-only structured prefixes (`[auth]`, `OK`, `WARN`, `ERR`) are fine; emoji like `✅` / `❌` / `🔒` / `🚀` are not. Applies to user-visible release notes the maintainer drafts from these as well.

## Technical invariants

- **Rust edition 2021**, MSRV pinned in `rust-toolchain.toml`
- **Cargo workspace** — a single `Cargo.toml` at the root, all crates and services live inside it
- **Workspace dependencies** — versions of shared deps pinned in `[workspace.dependencies]`, crates inherit via `dep.workspace = true`
- **Reproducible builds** — the Nix flake pins the entire toolchain; CI checks byte-identicality between runners
