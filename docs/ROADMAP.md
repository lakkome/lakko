<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko — Roadmap

Full phase decomposition — what we build and in what order. The strategy lives in `~/.claude/plans/1-nested-wreath.md`; the ADRs in `docs/adr/` are the authoritative source for architectural decisions; this document is the **feature-development tactics**.

---

## Execution context

**This roadmap is not a calendar.** No deliverable is tied to weeks / months / years.

- Code is written by an AI agent (Claude Code) inside sessions started by the maintainer
- A **sprint** = an atomic deliverable that fits into 1-3 coherent sessions and ends with a green CI
- The pace of phases is driven by when the maintainer sits down to work
- **Phase N+1 does not start** until Phase N's exit criteria are green — that is the only dependency
- Stack: Rust core + native UI per platform; MariaDB; Hetzner Cloud as hosting

Decisions about launch / audit / branding / pricing are the maintainer's. This roadmap covers **only what to write, and in what order**.

---

## Phase summary

| Phase | What is added |
|---|---|
| **0** Foundations | Workspace, ADRs, CI, MLS end-to-end in tests, FFI surface, fuzz harnesses |
| **1** 1×1 Messaging | services/auth, services/ds, services/storage, services/discovery, services/push-gw, lakko-ffi-wasm, apps/ios, apps/android, apps/web |
| **2** Groups + Multi-device | X.509 + KT log, DeviceGroup, encrypted backup, ≤50-member groups, apps/desktop, apps/macos |
| **3** Calls | lakko-calls (SFrame), services/sfu (self-hosted LiveKit), P2P + group calls (link and in-chat) + audio rooms, CallKit / ConnectionService, coturn |
| **4** Channels / Broadcasts | PublisherGroup, sender-key broadcast, services/edge, medium-scale broadcasts |
| **5** Anti-censorship + Scale | REALITY, MASQUE, Shadowsocks-2022, transport-rendezvous, multi-region edge cascade, apps/watchos, localization |

---

## Cross-cutting

### Code quality gates

| Gate | Starting at phase | Type |
|---|---|---|
| `cargo fmt --check` | 0 | blocking PR |
| `cargo clippy -- -D warnings` | 0 | blocking PR |
| `cargo test --workspace` | 0 | blocking PR |
| `cargo doc -D warnings` | 0 | blocking PR |
| `cargo deny check` | 0 | blocking PR |
| `reuse lint` | 0 | blocking PR |
| `cargo audit` | 0 | blocking PR (vulnerability) |
| `cargo fuzz` on parsers | 0.4 | continuous (OSS-Fuzz integration) |
| `kani` model checking on the MLS state machine | 2 | nightly |
| Coverage ≥80% (crypto + mls) | 2 | blocking PR |
| Mutation testing on crypto (`cargo-mutants`) | 2 | nightly |
| Hax / F* formal verification of critical paths | 4+ | aspirational |
| SLSA L3 build provenance | 1 | release |
| Reproducible build verifier (dual runner) | 0.4 | release |

### Hosting / infra

Everything self-hosted on Hetzner Cloud. We start with one VPS and split things out only when we hit a ceiling (CPU / RAM / bandwidth). Object storage — Hetzner Storage Box or local disk.

| Phase | Infra requirement |
|---|---|
| 0 | — (no prod) |
| 1 | 1× VPS with all services + MariaDB + nginx + coturn co-located |
| 2 | scale-up VPS, or 2× VPS (backend + DB) once we hit a wall |
| 3 | + 1× CCX-tier VPS for the SFU (LiveKit / mediasoup-rs / pion / str0m), 20 TB transfer |
| 4 | scale up the SFU as broadcast viewer load grows |
| 5 | multi-region edges (for 100k+ viewer broadcasts with low latency globally) |

---

## Phase 0 — Foundations

### Goal

From an empty repo to a workspace where MLS groups work end-to-end in tests and the FFI surface is stable enough to generate Swift / Kotlin bindings.

### Pre-flight

— (starting phase).

### Sprint breakdown

| Sprint | Contents | Status |
|---|---|---|
| **0.0** | Cargo workspace + ADR-0001…0006 + CI workflow + Nix flake + justfile + REUSE compliance | ✅ done |
| **0.1** | CI gates on GitHub Actions + lakko-proto codegen (protox + prost-build) + lakko-crypto primitives | ✅ done |
| **0.2** | lakko-mls on top of OpenMLS (identity / key_package / group / exporter) + end-to-end Alice ↔ Bob ↔ Charlie | ✅ done |
| **0.3** | lakko-transport (QUIC + in-memory) + lakko-ffi-uniffi (Identity / Group surface) | ✅ done |
| **0.4** | Fuzz harnesses (cargo-fuzz) on parsers (MLS wire, prost); reproducible build verifier (dual runner in CI). OSS-Fuzz upstream PR — when the first real integration lands | ✅ done |
| **0.5** | KT log skeleton — `lakko-transparency` crate (Merkle RFC 6962 + BLAKE3, SignedRoot Ed25519, InclusionProof, InMemoryLog, rollback detection). X.509 credentials + FIPS suite unblock — deferred to Phase 1 (no `openmls_x509_credential` crate in the registry, requires upstream contribution or manual TLS-codec encoding) | ✅ done (KT) / deferred (X.509+FIPS) |

### Exit criteria
- ✅ MLS groups end-to-end in integration tests
- ✅ FFI surface stable (lakko-ffi-uniffi compiles, three-party FFI test green)
- ✅ All CI gates green
- ✅ Fuzz harnesses for every wire-format parser (MLS message, KeyPackage, prost wire/mls_app/media)
- ✅ Reproducible build verifier — CI job exists (continue-on-error while we tune flags for byte identicality)
- ✅ KT log skeleton — `lakko-transparency` crate (production storage in Phase 2)
- 🟡 X.509 credentials + FIPS — deferred to Phase 1; needs either an upstream `openmls_x509_credential` crate or manual TLS-codec encoding of the cert chain

### Top risks
- **OpenMLS upstream churn** — mitigated: fast-follow upgrades (we already did 0.6 → 0.8 when the hpke-rs CVE landed)
- **uniffi 0.28 stale deps** (`paste`, `bincode`) — flagged in `deny.toml` as ignore with a link to upstream

---

## Phase 1 — 1×1 Messaging

### Goal

The first real functional slice: three devices (iOS, Android, web) exchanging 1×1 messages through a real backend, self-hosted on a single Hetzner VPS. The web client is a parallel track to native mobile, so dogfooding does not depend on Apple / Google accounts and review pipelines.

### Pre-flight
- Phase 0 exit criteria green (including 0.4 fuzz and 0.5 X.509 + KT skeleton)

### Sprint breakdown

#### Backend infra
- **1.0** Hetzner deployment skeleton (✅ done): migration from the lakko_me Node.js stack → the new Rust stack on the SAME VPS. systemd units for 5 services (auth/ds/storage/discovery/push-gw) + group target; system-nginx config with reverse proxy to 127.0.0.1:808X for the existing lakko.me / api.lakko.me domains; system-MariaDB (drop the old `lakko_me` DB, create a fresh `lakko`); `/etc/lakko/lakko.env` for shared config; `infra/scripts/deploy.sh` (git pull → cargo build --release → install → systemctl restart); a step-by-step migration guide in `infra/README.md`. Containerisation (podman / compose) is intentionally deferred — overkill for solo + 1 VPS, we'll add it when horizontal scaling actually matters
- **1.1** `services/auth` — OPAQUE-PAKE password auth (primary), MariaDB-backed signup / login without storing PII; BIP39 mnemonic recovery; PoW (Equix) + Privacy Pass (RFC 9576) anti-spam. WebAuthn passkey is **deferred to Phase 2+** as an opt-in second factor — passkey UX in 2026 is still problematic for the mass market (cross-ecosystem sync issues, recovery friction, mental-model confusion); the password flow covers 100% of users without friction. Sub-sprints: 1.1a (✅ axum + sqlx + probes), 1.1b (✅ OPAQUE registration), 1.1c (✅ OPAQUE login), 1.1d (✅ BIP39 recovery), 1.1e (✅ Equix PoW; Privacy Pass deferred — needs App Attest / Play Integrity issuer)
- **1.2** ✅ `services/storage` — content-addressable ciphertext blob proxy. Filesystem-backed (`LAKKO_BLOB_DIR`, two-level sharded layout `<root>/<hex[0:2]>/<hex[2:4]>/<hex>`). PUT verifies SHA-256 streaming-style; idempotent re-PUT. GET/HEAD streaming download. 50 MB per-blob ceiling. No auth/quota in Phase 1 (server-as-blind-CDN). Owner tracking, quota, DELETE, PoW gating, SeaweedFS/S3 backend — Phase 2+ hardening
- **1.3** `services/ds` skeleton — MariaDB-backed append-only journal per group (FIFO by `(epoch, seq)`), mailbox per device, axum + tokio
- **1.4** `services/ds` production — fan-out, long-poll or QUIC stream push for real-time delivery
- **1.5** `services/discovery` — username table + OPRF (RFC 9497) blinded lookup, exact-match only, PoW per query, MariaDB-backed
- **1.6** `services/push-gw` — APNs + FCM + Web Push (VAPID) envelope routing, payload limited to `(group_pseudonym, envelope_pointer)`; UnifiedPush self-host (on the same VPS, prepping for CN/RU)

#### Crates (extension)
- **1.7** `lakko-storage` — SQLCipher-backed local store (native), IndexedDB-backed for web; key derived from device root key via HKDF (Secure Enclave / StrongBox / non-extractable WebCrypto)
- **1.8** `lakko-transport` production wiring — QUIC client (native) + WebTransport fallback / WebSocket (web) talking to `services/ds`, cert pinning skeleton
- **1.9** `lakko-contacts` — ContactRequest end-to-end, capability-based invite links (see ADR-0005); minimal pseudonym lifecycle
- **1.10** `lakko-push` — encrypted push payload framing (APNs / FCM / Web Push formats)
- **1.11** `lakko-presence` — blinded per-conversation presence tokens (minimal version)
- **1.12** `lakko-sync` — hybrid logical clock + draft sync (single-device variant)
- **1.13** `lakko-ffi-wasm` — wasm-bindgen surface mirroring UniFFI (Identity / Group / KeyPackage / message processing)

#### Native apps (iOS + Android)
- **1.14** `apps/ios` skeleton — SwiftUI: signup / signin (OPAQUE-PAKE), contact list UI, 1×1 chat UI; integration with the XCFramework from lakko-ffi-uniffi
- **1.15** `apps/android` skeleton — Compose: same surface; AAR integration

#### Web app
- **1.16** `apps/web` skeleton — Solid.js + WASM core (via lakko-ffi-wasm); signup / signin (OPAQUE-PAKE via WebCrypto-derived OPRF), contact list UI, 1×1 chat UI; service worker for offline; bundle target < 2 MB gzipped
- **1.17** `apps/web` static-host pipeline — GitHub Pages or Hetzner static behind nginx; SRI hashes for security; reproducible build artefacts

#### Cross-platform features
- **1.18** Apps: ContactRequest end-to-end via DS, invite-link flow (iOS + Android + Web)
- **1.19** Apps: 1×1 chat — encrypted text + attachments via the storage service
- **1.20** Apps: push notifications — APNs (iOS) / FCM (Android) / Web Push VAPID (browsers); encrypted envelopes everywhere
- **1.21** Apps: recovery flow — BIP39 24-word mnemonic, optional Shamir 3-of-5 social recovery (if there's capacity left in this phase)

#### Readiness
- **1.22** Staging deployment — one Hetzner VPS, nginx + MariaDB + all services co-located, Let's Encrypt; the web client deployed on the same domain or on static hosting

### Exit criteria
- 1×1 messaging works end-to-end on real iOS + Android devices **and in the browser** through the Hetzner backend
- Cross-platform interop: the web client and mobile decrypt each other's messages identically
- Push delivery p50 < 5s (across all three channels: APNs / FCM / Web Push)
- Message send → delivered p50 < 1s regional
- DS sustains 1k concurrent connections in a local load test
- Recovery: a device can be lost and the account restored from the mnemonic
- Reproducible build artefacts verified (XCFramework + AAR + WASM bundle byte-identical between runners)
- Web bundle < 2 MB gzipped

### Top risks
- **iOS background limitations** — APNs delivery reliability; MLS state machine recovery after a long sleep
- **OPAQUE production maturity** — the RFC is still draft; track upstream
- **Hardware-backed key APIs differ** between iOS / Android / WebCrypto — different failure modes
- **WASM bundle size** for web — OpenMLS + RustCrypto can produce a heavy bundle; be ready for code splitting and lazy-loading of rare paths
- **Web Push VAPID setup** — needs VAPID keys + server-side push calls via `services/push-gw`

---

## Phase 2 — Groups + Multi-device + Desktop

### Goal

Groups up to ~50 members (vanilla MLS), multi-device sync without plaintext on the server, plus desktop (Windows / Linux) and macOS clients added. iOS / Android / Web are already shipping from Phase 1.

### Pre-flight
- Phase 1 exit criteria green

### Sprint breakdown

#### Crypto / protocol
- **2.0** X.509 credentials production-grade (full validation) + KT log production (CONIKS-style append-only with signed roots)
- **2.1** Identity rotation via KT log supersession proof + 30-day bridge for old invite links
- **2.2** FIPS ciphersuite (P-384 + AES-256-GCM + ECDSA-P384) wired through X.509 credentials end-to-end

#### Multi-device
- **2.3** DeviceGroup in lakko-mls + lakko-sync — every device = a leaf in the user's personal MLS group
- **2.4** Multi-device sync substrate — read receipts, drafts, conversation order via DeviceGroup application messages
- **2.5** History catch-up bundle exchange — HPKE-encrypted ciphertext from the old device to the new one when adding a device
- **2.6** Encrypted backup on Hetzner Storage Box, KEK = Argon2id from the recovery passphrase
- **2.7** Optional Shamir 3-of-5 social recovery (if not closed in Phase 1)

#### Groups
- **2.8** Group creation with ≥3 members (vanilla MLS up to ~50)
- **2.9** Group invite links (capability-based, ADR-0005) + approval flow for private groups
- **2.10** Group settings: encrypted name / avatar / description

#### Discovery / privacy
- **2.11** All 6 levels of discoverability scope: Open / ContactsOfContacts / Contacts / InviteOnly / Allowlist / VerifiedNetwork
- **2.12** Mutual-contact attestation chain for the ContactsOfContacts scope
- **2.13** Block list (encrypted under identity, the server applies a hashed match)

#### Apps (new)
- **2.14** `apps/desktop` — Tauri 2.x + Solid.js (UI reused from apps/web), Windows / Linux native binaries
- **2.15** `apps/macos` — separate build via SwiftUI XCFramework (shared core with iOS)
- **2.16** Extend apps/web for the group UX (member list, group settings, etc.)

#### Verification & QR
- **2.17** QR safety numbers — Signal-style fingerprint scan, OOB-secure contact addition
- **2.18** TOFU pinning via KT log + QR override

#### Hardening
- **2.19** `kani` model checking on the MLS state machine
- **2.20** `cargo-mutants` mutation testing on lakko-crypto + lakko-mls
- **2.21** Coverage ≥80% on lakko-crypto + lakko-mls
- **2.22** End-to-end tests on iOS / Android / Web in CI (matrix)

### Exit criteria
- Feature-complete messaging on 5 platforms (iOS / Android / macOS / Linux / Web)
- A 50-member group is stable in a load test (Add/Remove under load, 100+ commits per hour)
- Multi-device sync convergent at p95 < 5s between two devices
- ≥80% statement coverage on lakko-crypto + lakko-mls
- WASM bundle < 2 MB gzipped
- KT log validates inclusion proofs end-to-end

### Top risks
- **KT log spec is still draft** — possible fork / patch upstream
- **WASM bundle size** for web (target < 2 MB gzipped) — may require code splitting
- **Cross-device sync convergence** under flaky networks
- **OpenMLS X.509 credential maturity** — may need upstream contributions

---

## Phase 3 — Calls

### Goal

P2P 1×1 + group audio/video calls + audio rooms. Self-hosted SFU (LiveKit upstream without a fork to start with, or mediasoup-rs / pion / str0m) on a single Hetzner VPS. Media keys from MLS exporter, SFrame for frame-level encryption.

Multi-region edge cascade and 100k+ viewer broadcasts are Phase 5 (those genuinely require geographically distributed edges).

### Pre-flight
- Phase 2 exit criteria green

### Sprint breakdown

#### Backend
- **3.0** coturn self-hosted on the same Hetzner VPS — STUN + TURN-over-TLS
- **3.1** Signaling for call setup via `services/ds` (a special MLS application-message type inside the call group)
- **3.2** `services/sfu` — self-hosted LiveKit (Apache 2.0 upstream) or alternative (mediasoup-rs / pion / str0m); JWT-auth integration with `services/auth`; on a separate VPS

#### Crates
- **3.3** `lakko-calls` skeleton — SFrame (RFC 9605) implementation, jitter buffer (NetEQ-style), codec negotiation
- **3.4** `lakko-calls` ICE/STUN/TURN client integration (via webrtc-rs or str0m)
- **3.5** `lakko-mls::group` exporter API extended for call media keys (`mls_exporter("lakko/call/v1", call_id, 32)`)

#### Topologies
- **3.6** P2P 1×1 setup: WebRTC + ICE/STUN/TURN, no SFU; media keys from the 1×1 MLS group exporter
- **3.7** Group call via link: SFU + ad-hoc MLS group, joining via embedded one-time KeyPackage
- **3.8** Group call inside a chat: SFU + reuse of the existing chat MLS group; exporter context = `call_id`
- **3.9** Audio room: Speakers MLS subgroup (vanilla, ≤50 speakers) + Audience via SFU subscribe-only
- **3.10** Encrypted media: SFrame keys from the exporter, AES-128-GCM per frame, hardware AES-NI / ARM Crypto Extensions; key rotation on Add/Remove with 1s overlap

#### Codecs
- **3.11** Audio: Opus 32-64 kbps + DTX + FEC
- **3.12** Video: AV1-SVC (modern), VP9-SVC (fallback), H.264 (Apple legacy)
- **3.13** Adaptive bitrate based on receiver BWE feedback (LiveKit / SFU-side logic)

#### Apple integration
- **3.14** CallKit for native incoming-call UI
- **3.15** PushKit for wake-up on incoming call
- **3.16** Live Activities for in-progress call indicator

#### Android integration
- **3.17** ConnectionService for system-managed incoming UI
- **3.18** MediaSession for controls
- **3.19** ForegroundService for long-running calls

#### Extras
- **3.20** Screen sharing — single screen producer, pinned layout
- **3.21** In-call chat — regular MLS application messages in the call group (existing flow)
- **3.22** Promotion / demotion in audio rooms: Add/Remove on the Speakers MLS subgroup + key rotation

### Exit criteria
- P2P 1×1 audio + video call works end-to-end on iOS + Android + Web + Desktop
- Group call (link and in-chat) — up to 30 participants through a single self-hosted SFU
- Audio room — up to 30 speakers + ≤500 listeners through a single SFU
- Audio quality A/B-tested against Telegram / WhatsApp / Discord in lab; not worse than p50
- Audio p50 latency < 100 ms regional (P2P) / < 150 ms (via SFU)
- Screen sharing works on macOS / Linux / Windows
- E2EE is strict — no silent fallback (including media in group calls)
- CallKit / ConnectionService integration correct (incoming UI works in background)
- The SFU sustains 5 concurrent group calls of 10 participants each in a local load test

### Out of scope for Phase 3
- Channels / sender-key broadcast (Phase 4)
- 100k+ viewer broadcasts (Phase 5 — needs multi-region cascade)
- Recording (separate deferred question — policy clarity)

### Top risks
- **iOS background CPU limits** during long calls
- **AV1 hardware decode** on older devices (fallback to H.264)
- **TURN abuse** — self-hosted coturn needs rate limits + auth
- **CallKit / ConnectionService** API differences create UX inconsistency between iOS and Android
- **SFU bandwidth ceiling** — a single CCX-tier VPS gives ~60 Mbps sustained; ~30 concurrent 4-person video calls; scale up as needed

---

## Phase 4 — Channels + Medium Broadcasts

### Goal

Channels (sender-key broadcast via DS) and medium-scale broadcasts (≤10k concurrent viewers via a single SFU).

Audio rooms are already done in Phase 3. Multi-region edge cascade for 100k-1M viewers is Phase 5 (that really needs geographically distributed edges and multi-CDN).

### Pre-flight
- Phase 3 exit criteria green

### Sprint breakdown

#### Channels (sender-key broadcast)
Vanilla MLS does not scale to 1M — we implement a **two-tier** architecture (see plan §3 / ADR-0001):

- **4.0** PublisherGroup in lakko-mls — vanilla MLS, ≤500 mods/admins, content-key derivation API
- **4.1** Sender-key envelope distribution via DS — HPKE-to-KeyPackage envelope for each new subscriber
- **4.2** Content-key rotation — on schedule (24h / N messages) + on PublisherGroup membership change
- **4.3** Channel subscribe / unsubscribe flow + best-effort revoke
- **4.4** Reactions + comments on channel posts (per-post small MLS group, dynamically created)
- **4.5** Public channel discovery (`@channel-username`) via a `services/discovery` extension

#### Medium broadcasts (single-region SFU)
- **4.6** `services/edge` — broadcast origin handler; reuses the SFU from Phase 3 for the media pipe
- **4.7** Late-joiner flow for broadcasts — a viewer receives the current content key via an HPKE envelope
- **4.8** Reactions from viewers (via the same sender-key broadcast back-channel)

#### Public posture
- **4.9** Document publicly: backward-secrecy property of channels (the same as Telegram / Slack / Discord; we run rotation aggressively)

#### Load testing
- **4.10** A 50k-subscriber channel is stable (rotation event + delivery)
- **4.11** A 10k-viewer broadcast end-to-end through a single SFU

### Exit criteria
- A channel up to 50k subscribers is stable (rotation events do not topple DS)
- A broadcast up to 10k viewers through a single SFU is stable (scale up the VPS if needed)
- Public channels are findable by `@username`
- Documented publicly: the backward-secrecy property of channels

### Out of scope for Phase 4
- 100k-1M viewer broadcasts (Phase 5 — needs multi-region edge cascade)
- Channel federation (out of roadmap)
- Recording channels / broadcasts (separate policy question, not functional)

### Top risks
- **Sender-key distribution scaling** — a large channel = many HPKE envelopes on rotation; DS bandwidth budget; mitigated by staggered rollout
- **Revocation after content-key distribution** — best-effort until rotation; community reaction to that fact
- **DS load** at rotation events for large channels (needs throttling)
- **Single-SFU broadcast ceiling** — past ~10k viewers degradation kicks in, beyond that we need cascade (Phase 5)

---

## Phase 5 — Anti-censorship + Scale

### Goal

Anti-censorship transport stack (for users in RU / IR / CN) + multi-region edge cascade for broadcasts of 100k+ concurrent viewers + watchOS + localization.

### Pre-flight
- Phase 4 exit criteria green

### Sprint breakdown

#### Pluggable transports (see ADR-0004)
- **5.0** REALITY (xtls/REALITY) in lakko-transport — TLS-in-TLS hiding behind a foreign certificate
- **5.1** MASQUE / CONNECT-UDP (RFC 9298) — tunnels QUIC inside HTTP/3
- **5.2** Shadowsocks-2022 (AEAD-2022)
- **5.3** VLESS + XTLS-Vision
- **5.4** WebTransport for the web client
- **5.5** obfs4 — legacy compatibility

#### Bootstrap rendezvous
- **5.6** ECH (RFC 9460/9525) on edge endpoints
- **5.7** `services/transport-rendezvous` — produces signed bootstrap configs (Ed25519, JSON: endpoints + working PT configs)
- **5.8** DoH-resolved (Cloudflare 1.1.1.1, Google, Quad9, Mullvad) rotating pool of innocuous-looking domains
- **5.9** TUF (The Update Framework) signed transport configs — new transports as **data**, not code
- **5.10** Auto-discovery: happy-eyeballs racing, top-2 working ones kept warm, budget ≤10s cold start

#### Mobile transport integration
- **5.11** iOS: pluggable transports embedded in URLSession + QUIC (not via NEPacketTunnelProvider)
- **5.12** Android: VpnService available optionally, default — in-app

#### Multi-region broadcast cascade (for 100k+ viewer broadcasts)
- **5.13** `services/edge` extension — origin SFU → edge SFU cascade with region-affinity routing
- **5.14** Edge nodes deployed in several Hetzner regions (Helsinki / Falkenstein / Nuremberg / Ashburn) or multi-CDN
- **5.15** Late-joiner flow for cascade broadcasts
- **5.16** Load testing: 100k+ concurrent viewers spread across regions

#### watchOS
- **5.17** `apps/watchos` — thin presenter, paired secure channel with the phone

#### Localization
- **5.18** Translation pipeline (Crowdin or self-hosted) for all clients
- **5.19** 10+ languages (ru / en / fa / zh-CN / ar / es / pt / de / fr / ja / ko / uk / …)
- **5.20** RTL layouts

#### Polish
- **5.21** Onboarding flow (minimum friction, secure-by-default)
- **5.22** Accessibility (full VoiceOver / TalkBack support)
- **5.23** Animations, transitions

### Exit criteria
- Successful connections from ≥5 censorship-emulating environments (GFW / TSPU / IR DPI / corporate firewall) in lab
- Broadcast 100k+ concurrent viewers stable through cascade
- WatchOS app works
- Localization for 10+ languages with native review
- Auto-discovery of transports picks a working one in ≤10 s cold start

### Top risks
- **Censorship arms race** — needs fast TUF-signed transport rollouts (new transports as data, not code)
- **iOS Network Extension limitations** restrict anti-censorship — fallback to in-app transports
- **Hetzner multi-region** — all Hetzner DCs are in EU; for US / APAC users we either accept latency or onboard other providers
- **DPRK-style allowlist-only internet** — service unavailable there; **abandon plan**: we do not engineer for it

---

## Post-Phase-5 (out of scope of this roadmap)

After Phase 5 it is normal product evolution. Technical directions worth tracking:

- **Federation** between Lakko instances
- **Post-quantum** — MLS extensions for ML-KEM are being worked on at IETF; migrate when stable and available in OpenMLS
- **libcrux replacement** — switch to a formally verified crypto provider
- **OpenMLS upstream contributions** — reserve capacity for upstream
- **SDK split** — separate `lakko-mls` / `lakko-crypto` / `lakko-proto` under Apache 2.0 into a sub-repo if the industry asks for an embedded MLS implementation; thanks to the CLA this is possible

---

## Things to keep in mind

- **The roadmap is not a calendar** — a sprint = an atomic deliverable, not a time slot
- **Phase N+1 does not start** until Phase N's exit criteria are green — the only hard rule
- **The document is updated** — after each phase closes, we add a `### Retrospective` section with what went as expected vs what did not
