<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Threat model

## Assets

| Asset | Location | Protection |
|---|---|---|
| Plaintext messages | Sender + recipient devices only | MLS group epoch keys (RFC 9420); local at-rest under the device root key |
| Group state (ratchet trees, init keys) | Each member's OpenMLS provider | At-rest under the device root key; never serialised to the wire in plaintext |
| Identity signing keypair (Ed25519) | Each device | Hardware-backed wrap key + (1) deterministic from OPAQUE exportKey for the AccountIdentity, (2) random per-device for the DeviceIdentity (ADR-0007) |
| Account password | User's head + on-server OPAQUE envelope only | OPAQUE-PAKE (RFC-draft) — server never sees plaintext password |
| Phone / email / address book | NOT collected | Never sent to the server; the `discovery` service knows usernames only, OPRF-blinded |
| Social graph | Client-side only | Add-contact = encrypted envelope to a per-recipient pseudonym; presence = blinded per-conversation tokens |
| Plaintext attachment blobs | Each device's local store + content-addressable encrypted blobs in `services/storage` | Per-attachment ChaCha20-Poly1305 CEK in the wire envelope; blob_id = SHA-256 of the ciphertext |
| Recovery phrase (BIP39 12-word, 128-bit entropy; 24 words until ADR-0072, 2026-10-02) | User's head + a copy on every device that holds the account's keys, wrapped under that device's hardware-backed key (ADR-0072 decision 8; built on web, iOS and Android in progress) + optionally, when the user opts in per passkey, a server-held blob sealed under a key derived from that passkey's PRF output | Argon2id-derived KEK over the backup envelope; never sent to the server |

## Adversaries

The model assumes a **fully compromised server**. The
operator (or an attacker who took over the operator's keys)
can read every byte the server holds and serve crafted bytes
back to clients. The model is what survives that.

| Adversary | Capability assumed | What we MUST survive |
|---|---|---|
| **Compromised DS / discovery / storage / push-gw servers** | Read every row + every blob + every transparency-log entry; serve arbitrary bytes back | Plaintext message contents, identity material, social graph |
| **Network-level adversary (state ISP, TSPU, GFW)** | Inspect / drop / inject TLS records, MITM the TLS certificate | All E2EE invariants survive; censorship-resistance transports (Phase 5) shield connectivity but are out of scope for this audit |
| **Cloud-backup attacker** (iCloud / Google Drive snapshot) | Pull the user's iCloud / Google backup blob | MLS state + per-account key material excluded from cloud backup via OS-level `noBackup` / `isExcludedFromBackup` flags |
| **Local-disk attacker (cold device theft)** | Read raw flash / `/data/data` | Every at-rest piece is wrapped under a hardware-backed key (Secure Enclave / StrongBox / non-extractable WebCrypto). Plaintext is unreachable without the device's biometric / passcode |
| **Live-device attacker (root / jailbreak / browser extension)** | Code execution inside the app's process | OUT OF SCOPE for Phase 2 — the app cannot defend against a hostile OS / browser. We document this in `known-gaps.md` |
| **Coerced-maintainer attacker** | Compel the operator to publish malicious code / keys | Reproducible builds + transparency log + cosign-signed releases. A backdoored binary cannot ride the release channel without forking the public hash record |
| **Mass-correlation attacker** | Pattern-match across all server logs | Pseudonyms rotated per relationship; no global "online" surface; presence blinded per-conversation; discovery via OPRF so the server never sees the lookup string |
| **Misrouted-Welcome attacker** | A different user's DeviceGroup Welcome reaches us | Deterministic DeviceGroup `group_id = BLAKE3("lakko-device-group/v1" \|\| account_id)` lets the receiver reject before any state advance (ADR-0007 §"deterministic group_id") |
| **Stolen-session attacker** (an access token or refresh credential lifted from a device, without the password, the phrase or the device key) | Every bearer route of `auth`, `discovery`, `storage` and `gif-proxy` for the embedded account until the credential expires or the account's key generation moves; since [ADR-0073](../adr/0073-account-key-reset.md) that includes **resetting the account keys** (stripping every passkey, superseding every other session, re-rooting the DeviceGroup journal) | No key material and no plaintext: the server holds none (invariant #2), the reset mints a NEW identity rather than exposing the old one, and the old devices' conversation leaves stay until a contact who administers each group evicts them (ADR-0073 phase 2, in code since 2026-10-04; G-45 for the cases that stay open). The reset is **loud** (every device of the owner shows the signed-out screen within the access TTL, 900 s by default) and reversible by the owner, who still holds the password or the authenticator and can reset back; it is bounded by `Scope::RecoveryReplace` + `Scope::KeyReset`. A leaked **access** token is good for at most the access TTL everywhere; a leaked **refresh** credential for up to 30 days against `auth` only, until the next reset. Residual tracked as G-46 |

## Invariants — MUST NOT be violated

These are the same constraints CLAUDE.md lists for the AI
agents. Reviewers should verify each:

1. **No plaintext PII on the server.** Phone / email never
   stored — not even encrypted. Only OPAQUE-blinded records
   and per-user hashes for recovery.
2. **The server only sees ciphertext + public artefacts**
   (KeyPackages, transparency-log entries). No message
   keys, no chain keys, no root keys, no media keys.
3. **The MLS state machine is sacrosanct.** Any change to
   `crates/lakko-mls/` requires an ADR + property tests +
   external crypto review (this audit pass).
4. **No fallback to non-E2EE.** If the handshake does not
   complete, the connection does not open. No silent
   downgrade path.
5. **The social graph is not materialised on the server.**
   Add-contact = encrypted envelope to a pseudonym;
   presence = blinded per-conversation tokens; discovery =
   OPRF blinded lookup.
6. **Identity rotation only via the KT log supersession
   proof.** Old keys do not come back.
7. **Hardware-backed device keys are mandatory** on every
   platform (Secure Enclave / StrongBox / non-extractable
   WebCrypto).

## Trust boundaries

```text
                ┌────────────────────────────────────┐
                │              Server side           │
                │  (assumed compromised — UNTRUSTED) │
                │                                    │
                │   auth (OPAQUE only) ◀────┐        │
                │   ds (FIFO ciphertext)    │        │
                │   storage (blob ciphert.) │        │
                │   discovery (OPRF)        │        │
                │   push-gw (opaque wakes)  │        │
                └────────────┬──────────────┘        │
                             │                       │
       ─── TLS 1.3 + ECH + pluggable transport ──────┤
                             │                       │
                ┌────────────▼──────────────┐        │
                │           Client          │        │
                │      (TRUSTED — this is   │        │
                │       the user's device)  │        │
                │                           │        │
                │  shared lakko-* Rust core │        │
                │  native UI per platform   │        │
                │  hardware-backed key store│        │
                │                           │        │
                │  USER (assumed correct)   │        │
                └───────────────────────────┘        │
                                                     │
```

The hard boundary is `(client wasm/uniffi)` ↔ `(server gRPC /
HTTP)`. Everything below the boundary is trusted; everything
above is the assumed-compromised adversary surface.

## Out of scope

These adversary classes are explicitly NOT defended against
in Phase 2 — they get their own treatment in later phases:

- **Hostile OS / hostile browser** — see `known-gaps.md`.
  Mitigation: app-level integrity attestation in Phase 5.
- **Side channels** (cache timing, power, EM) — physical
  hardware attacks. We rely on the platform's standard
  countermeasures (constant-time crypto inside `lakko-crypto`,
  `subtle`-crate primitives, no hand-rolled crypto).
- **Quantum adversaries** — MLS cipher suites pinned to
  classical curves today. PQ migration follows the MLS WG's
  hybrid suites once the IETF lands them.
