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
   [`crates/lakko-calls/src/link_invite.rs`](../../crates/lakko-calls/src/link_invite.rs) —
   holds C-24..C-27 in [`calls.md`](./calls.md). Ed25519
   signature over the canonical bytes, embedded one-time
   KeyPackage with bounded size, schema-version + length
   gates, owner-pubkey-splicing attack rejected, and the
   joiner-bootstrap path preserves the cross-party CallKey
   seed agreement (C-1 re-asserted across the link flow).
   V1 is **single-joiner**; multi-joiner support deferred
   to 3c3-server (see G-21 / G-22 in
   [`known-gaps.md`](./known-gaps.md)).

Audit scope **out**: censorship-resistance transports
(REALITY / MASQUE / Shadowsocks-2022) — these land in
Phase 5 and get their own audit then. SFU-side surfaces
(LiveKit fork, JWT validation, RPC) audit alongside the
3b1 fork tree.

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
