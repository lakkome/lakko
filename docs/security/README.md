<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Security documentation

Audit-readiness pack for the Phase 2 → 3 transition (per
[`docs/ROADMAP.md`](../ROADMAP.md) §9 exit criteria: "External
crypto + MLS state + auth audit before public beta").

The intent of this directory is to let an external reviewer
load the project state in one read pass without grepping the
codebase. Each file points at the live code paths it
summarises so the docs and the implementation stay in sync
(reviewers verify by jumping to the cited paths).

## Files in this pack

- [`threat-model.md`](./threat-model.md) — assets, adversaries,
  invariants. Single source of truth for "what must the
  protocol resist".
- [`crypto-overview.md`](./crypto-overview.md) — every
  primitive in use, where it lives in the code, which RFC /
  cipher-suite it implements.
- [`mls-state-machine.md`](./mls-state-machine.md) — wrappers
  around OpenMLS we ship, exporter labels we coined,
  invariants the state machine MUST preserve.
- [`auth-flow.md`](./auth-flow.md) — OPAQUE-PAKE registration
  / login / recover round-trips, WebAuthn passkey opt-in,
  device-key derivation chain, key-rotation paths.
- [`device-keys.md`](./device-keys.md) — hardware-backed
  device-root-key lifecycle on every platform
  (Secure Enclave / StrongBox / non-extractable WebCrypto).
- [`backup.md`](./backup.md) — encrypted backup envelope
  (v2: identity_seed + contacts + whole_storage_snapshot),
  Argon2id KEK, recovery flow, Shamir social recovery.
- [`multi-device.md`](./multi-device.md) — ADR-0007 two-tier
  identity + DeviceGroup pairing + remove-device + history
  catch-up bundle + cross-device sync.
- [`calls.md`](./calls.md) — call-side cryptography (Phase
  3a–3c + 3b4). Three layers: (1) media-key pipeline — MLS
  exporter → CallKey → ParticipantKey chain, per-participant
  SFrame keys, 1-second overlap window for mid-call rotation
  (invariants C-1..C-14); (2) call-admission JWT — Ed25519
  signing in `services/auth`, JWKS publication, bearer-auth
  on `POST /call/jwt`, issuer / audience / expiry checks
  (invariants C-15..C-23 — includes JWKS multi-key resolution +
  JWK algorithm-confusion defence); (3) link-based call-invite
  capability — Ed25519-signed `https://lakko.me/c/<slug>`
  with embedded one-time KeyPackage; joiner-side
  orchestration bootstraps a fresh MLS group + matching
  CallKey; owner-fingerprint anti-phishing surface
  (invariants C-24..C-28). What the SFU sees vs
  what stays client-side. Every invariant cites a property
  test.
- [`transport.md`](./transport.md) — anti-censorship +
  rendezvous transport surface (Phase 5). Pluggable-kind
  wire identifiers (T-1), scaffold-transport loud-fail
  (T-2), happy-eyeballs selector race semantics (T-3),
  4-stage `BootstrapConfig::verify` gate (T-4), canonical
  signing bytes (T-5), kebab-case `TransportSpec` kind
  pinning (T-6), binary-pinned trust anchor (T-7).
- [`known-gaps.md`](./known-gaps.md) — items the maintainer
  knows are not audit-ready yet; reviewers should not
  re-discover them. Each entry has a tracking issue / ROADMAP
  link.
- [`invariant-index.md`](./invariant-index.md) — consolidated
  cross-reference for every numbered invariant (I-1..I-8,
  C-1..C-44, T-1..T-11) and every tracked gap (G-1..G-26).
  Reviewer cheat-sheet: open this page, find the invariant
  ID, jump to the file + section that describes it. Also
  documents the "how to add a new invariant" maintenance
  procedure that keeps the index from drifting against the
  source files.

## Audit scope inputs

The reviewer is asked to verify:

1. The **MLS state machine** wrapper (`crates/lakko-mls/`) is
   free of state-confusion / re-key bypass bugs given the
   OpenMLS API contract. Property tests, fuzz harnesses, and
   the `mls-state-machine.md` invariant list are the
   in-scope reference.
2. The **authentication round-trips** (`services/auth/`,
   `crates/lakko-auth-client/`) implement RFC-draft OPAQUE
   correctly + leak no PII on signup / login / recover /
   password rotation. Particular focus on the password-
   change path's interaction with the device key derivation
   chain.
3. The **wire protocol** (`schemas/*.proto`) carries no
   plaintext PII or social-graph identifiers; every
   server-routable id is a pseudonym (per the
   `threat-model.md` invariants).
4. The **device-key derivation chain** keeps plaintext
   material off-disk on every platform — the
   `device-keys.md` document enumerates the at-rest material
   per layer and the key it sits behind.
5. The **encrypted backup envelope** (`crates/lakko-backup/`)
   is tamper-evident, downgrade-resistant, and decodes
   identically across platforms.
6. **Multi-device pairing** (DeviceGroup + history bundle)
   doesn't leak the receiver's plaintext to the server or to
   a misrouted Welcome.
7. The **call media-key pipeline** (MLS exporter → CallKey
   → ParticipantKey → SFrame seal/open) holds C-1..C-14 in
   [`calls.md`](./calls.md) — particular focus on
   cross-party exporter agreement, per-participant key
   distinctness, and the 1-second overlap window's
   forward-secrecy boundary on rotation.
8. The **call-admission JWT pipeline** — crypto + wire shape
   in [`crates/lakko-call-jwt/`](../../crates/lakko-call-jwt/src/lib.rs),
   server-side handlers in [`services/auth/src/call_jwt.rs`](../../services/auth/src/call_jwt.rs),
   client-side parse / self-verify via the FFI crates — holds
   C-15..C-23 in [`calls.md`](./calls.md). Hand-rolled JWS
   Compact + EdDSA over Ed25519, persistent signing key, JWKS
   publication, bearer-auth on the mint endpoint, strict
   issuer / audience / expiry / kid checks, multi-key JWKS
   resolution + JWK algorithm-confusion defence.
   The SFU-side validation (3b1c) is **out of scope for THIS
   audit pass** — it lands with the LiveKit fork. See G-19
   in [`known-gaps.md`](./known-gaps.md).
9. The **link-based call-invite pipeline** — wire codec in
   [`crates/lakko-contacts/src/call_invite.rs`](../../crates/lakko-contacts/src/call_invite.rs),
   joiner-side MLS orchestration in
   [`crates/lakko-calls/src/link_invite.rs`](../../crates/lakko-calls/src/link_invite.rs),
   owner-side dispatcher in
   [`crates/lakko-contacts/src/incoming_envelope.rs`](../../crates/lakko-contacts/src/incoming_envelope.rs) —
   holds C-24..C-36 in [`calls.md`](./calls.md). Ed25519
   signature over the canonical bytes, embedded one-time
   KeyPackage with bounded size, schema-version + length
   gates, owner-pubkey-splicing attack rejected. Both the
   single-joiner and multi-party paths are in scope (G-21
   and G-22 **closed** by C-33 KP-forward + C-35 multi-
   party E2E; the single-joiner V1 caveat in older audit-
   pack revisions is obsolete).

10. The **SFU signaling layer** — typed wire protocol in
    [`services/sfu/src/protocol.rs`](../../services/sfu/src/protocol.rs),
    per-peer fanout in
    [`services/sfu/src/signaling.rs`](../../services/sfu/src/signaling.rs) —
    holds C-39 in [`calls.md`](./calls.md). Server does
    not decode payload bytes (opacity invariant), cross-call
    isolation enforced via per-call peer registry, JWT
    admission gate at the WS upgrade. The fuzz harness at
    `services/sfu/fuzz/client_frame_decode` backs the
    "parser does not panic on attacker-controlled bytes"
    sub-property. The original LiveKit-fork SFU plan is
    **superseded** by ADR-0011; the Rust `str0m` SFU
    signaling half is shipped, the routing-media half is
    gated on G3-1/G3-2 (WebRTC client pod work).

11. The **channels broadcast backend + crypto** (Phase 4) —
    publisher-side seal in
    [`crates/lakko-channels/src/post.rs`](../../crates/lakko-channels/src/post.rs),
    subscriber-side HPKE seed envelope in
    [`crates/lakko-channels/src/subscribe.rs`](../../crates/lakko-channels/src/subscribe.rs),
    backend ciphertext-only storage at
    [`services/channels/src/`](../../services/channels/src/) —
    holds C-40..C-44 in [`calls.md`](./calls.md). Server
    handles `ChannelPost` bytes as opaque (`lakko-channels`
    crate is `dev-dep` only), atomic rotation under a SQL
    transaction, MLS-derived publisher seed via the
    `lakko/channel/v1` exporter label.

12. The **TUF v1.1 auto-update verifier** —
    [`crates/lakko-tuf/`](../../crates/lakko-tuf/). The
    crate is feature-complete: chain-of-trust walk
    (`verify_chain`), root rotation (`verify_root_chain`)
    anchored at a binary-pinned trust anchor, target-bytes
    verifier (`verify_target_bytes`), defense-in-depth
    rejection of any `Targets.delegations` block, URL
    convention module with path-traversal defence, 36 unit
    + 5 integration tests, 4 fuzz targets covering the
    JSON role envelope parsers. Network fetch glue lives
    in [`lakko-transport::DohFetcher::fetch_pinned_bytes`](../../crates/lakko-transport/src/doh_fetch.rs)
    (T-8 §7 sub-property).

13. The **anti-censorship transport surface** — pluggable
    transport trait, signed bootstrap rendezvous, DoH
    fetcher with IP pinning, chunked ChaCha20-Poly1305
    stream primitive, Shadowsocks-2022 outer header + TCP
    wiring + listener API, ECH config list parser. Holds
    T-1..T-12 in [`transport.md`](./transport.md). All
    parser surfaces have fuzz targets at
    [`crates/lakko-transport/fuzz/`](../../crates/lakko-transport/fuzz/).
    REALITY / MASQUE / WebTransport remain `scaffold`
    (`NotImplemented`), guarded by T-2 "scaffold transports
    surface NotImplemented loudly" — they get their own
    audit pass when the real implementations land.

14. The **encrypted backup tamper-evident chain** — v2
    envelope at
    [`crates/lakko-backup/src/envelope.rs`](../../crates/lakko-backup/src/envelope.rs)
    with chained `prev_hash` so a client holding any
    previous version detects server-side rollback.
    `header_of` / `open_v2` fuzz targets in
    [`crates/lakko-backup/fuzz/`](../../crates/lakko-backup/fuzz/).
    Closes G-11 for the client-with-state case; the
    cross-version Merkle log for stateless clients
    remains a known gap.

Audit scope **out**: REALITY / MASQUE / WebTransport
client-side real implementations (still scaffolds, tracked
as G5-1 / G5-2 / G5-4). SFU media half (`str0m` routing,
RTP fanout, simulcast) — gated on G3-1 / G3-2 client pods.
Censorship lab DPI emulation (G5-7) — needs deploy infra.
Reproducible-build cross-runner byte-identicality (GA-4) —
advisory in CI, flip-to-gating waits on stable artefact.
External crypto audit itself (GA-3), bug bounty programme
(GA-6) — out-of-chat process work.

## Reproducibility

Every command an audit reviewer might want to run:

```bash
just check      # cargo check --workspace --all-targets
just lint       # cargo clippy --workspace -- -D warnings
just test       # cargo test --workspace
just audit      # cargo deny check
just fuzz-check # smoke-build every fuzz harness on stable
just reuse      # SPDX-header lint
```

The Nix flake (`infra/nix/flake.nix`) pins the entire
toolchain. Two independent CI runners produce byte-identical
artefacts — see `ci/release.yml`.
