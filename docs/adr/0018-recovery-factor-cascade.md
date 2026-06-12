<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0018: Recovery factor cascade — ACCESS-only factors

- **Status:** Accepted — **built + reviewed** (Part 2 of the account-recovery
  overhaul). All four factors landed end-to-end (server + web + iOS + Android);
  `lakko-security` passed on factor (a) (APPROVE) and on (c)+(d) (APPROVE-WITH-NOTES,
  finding closed). 2026-06-09 (designed) → 2026-06-12 (built)
- **Date:** 2026-06-09
- **Context:** ADR-0017 decoupled the AccountIdentity from the password so a
  routine recovery preserves identity. This ADR adds the **access**-recovery side:
  a user who lost their device regains ACCESS to their account through a cascade
  of self-service factors — no other people, no out-of-band contact
  re-verification. It is the companion to ADR-0017.

## The load-bearing distinction: ACCESS vs IDENTITY

Recovery is two separate things, and the zero-trust posture forces them apart:

- **ACCESS** — a session token (the right to talk to the server). The server CAN
  grant this: password / passkey / TOTP / phone-or-email OTP all authenticate the
  user and mint a session token.
- **IDENTITY** — the E2EE AccountIdentity secret (the key contacts know the user
  by). The server **never** holds it (invariant #2), so it **cannot** restore it.
  Identity comes only from (i) the recovery phrase (user-held, ADR-0017) or
  (ii) another device (has it cached).

## Decision

Add a four-factor ACCESS-recovery cascade, presented to the user in order:

1. **another device** the user owns,
2. **passkey**,
3. **TOTP / authenticator code**,
4. **phone / email one-time code** gated on a stored hash.

**Load-bearing invariant — factors recover ACCESS only.** None of them transports
or restores E2EE key material through the server. After a factor grants ACCESS,
the client runs the ADR-0017 identity-acquisition state machine: cached phrase
seed → use it; else phrase entry / another-device transfer; else (genuinely lost
everything) a fresh identity, and contacts re-verify once via the existing
TOFU / safety-number path. Only factor (a) **also** carries identity — because the
authorizing device already has it and can transfer it over an E2EE channel.

All factors issue the **same** `(account_id, session_token, session_expires_at_ms)`
triple the existing OPAQUE/WebAuthn flows return, are **PoW-gated**, and are
**enumeration-safe** (an unknown account / unconfigured factor fails
indistinguishably from a wrong credential).

### Server-held verifiers are ACCESS verifiers, not key material

Factors (c) and (d) require the server to hold a verifier (a TOTP secret; a
salted/peppered hash of a phone/email). These are in the **same trust class as the
OPAQUE password envelope**: they authenticate, they never decrypt. Holding them
does **not** violate invariant #2 (no message/chain/root/media keys are involved).
This classification is recorded explicitly so the distinction is auditable.

## Factor (a) — another device (implementing now)

An already-authenticated device authorizes a fresh device AND transfers the
account identity to it, over the **existing** E2EE pairing / rendezvous channel —
so the fresh device gets both ACCESS and IDENTITY with no password or phrase entry.

The pairing substrate already exists (`crates/lakko-mls/src/pairing.rs` QR +
`crates/lakko-mls/src/device_group.rs` add_device + the DS rendezvous mailbox +
the `apps/web/src/devices/welcome-courier.ts` courier). Today the fresh device
joins the DeviceGroup but gets **no session token**. This factor adds two halves:

### ACCESS — a single-use device-authorization grant (server)

- `device_grants(account_id BINARY(16), grant_token BINARY(32), expires_at)` —
  modelled on `webauthn_action_grants` (ADR-0014, migration 0009); next migration
  `0011`. Short TTL; FK-cascade; swept by the existing state pruner.
- `POST /device/authorize/start` — **bearer-auth** (the already-authenticated
  existing device) + **PoW**. Mints a random 32-byte `grant_token` bound to the
  session's `account_id`, stores it, returns it to the existing device.
- `POST /device/authorize/redeem` — **anonymous** (the fresh device has no token
  yet) + **PoW**. Submits the `grant_token`; the server **atomically consumes** it
  (`DELETE … WHERE account_id=? AND grant_token=? AND expires_at>NOW(3)`,
  `rows_affected==1`) and issues the session-token triple. Single-use, time-bound,
  account-bound.

Why a server grant and not "trust the MLS Welcome": the DS is opaque and the auth
service is never in the MLS path, so only an **already-authenticated** device can
authorize the auth service to mint a token for a new device. The grant is that
authorization — short-lived, single-use, PoW-gated.

### IDENTITY — the phrase seed sealed over the rendezvous channel (clients)

The existing device seals `(grant_token ‖ 64-byte phrase seed)` to the fresh
device's `Identity.x25519_public_key()` using the existing HPKE-Base envelope
(`crates/lakko-contacts/src/envelope.rs`, wire `eph_pub(32) ‖ ciphertext`), and
publishes it as a **new FIFO row in the rendezvous mailbox** (after the Welcome) —
mirroring the existing history-bundle courier pattern, no QR/Welcome wire change.
The fresh device, after joining the DeviceGroup, polls that row, opens it with its
own X25519 secret, **redeems** the grant for a session token (ACCESS), and
**caches** the phrase seed in the ADR-0017 `account-identity-seed-store`
(IDENTITY). It never enters a password or phrase.

**Server opacity (load-bearing):** the DS stores only opaque bytes at a pseudonym;
the phrase seed crosses the wire **only** HPKE-sealed to the fresh device's key. A
property/integration test asserts the seed never appears in plaintext on the DS.
This is a **sealed side-payload, not a new MLS message** — the DeviceGroup state
machine is unchanged (no `lakko-mls` ADR/property-test burden beyond the envelope).

## Factors (b)/(c)/(d) — designed, built later

- **(b) Passkey — already exists.** `/webauthn/auth/{start,finish}` issues a
  session token. No server work; only the post-ACCESS step is the ADR-0017
  identity acquisition (cached seed, else phrase/another-device).
- **(c) TOTP — RFC 6238.** `totp_secrets(account_id PK, secret_enc, …)`; enroll
  bearer-authed (QR `otpauth://`); recover anon + PoW with a ±1-step window +
  replay protection (last-accepted-step). The secret is an ACCESS verifier
  (above), stored encrypted under a server-static key.
- **(d) Phone/email OTP — invariant #1 preserved.** Store only a salted/peppered
  **hash**; the plaintext is supplied **transiently** at enroll and at recover —
  used only to dispatch the code, **never stored or logged**. `recovery_contacts`
  (hash) + `otp_challenges` (code hash, TTL, attempt-cap) migrations; recover is
  anon + PoW, attempt-capped, "code sent" claimed indistinguishably on no-match.
  Needs an external SMS/email provider. Weakest factor → last in the cascade.

## Implementation status (built + reviewed)

All four factors landed (server + 3 clients), each build/test-verified:

- **(a) Another device.** `device_grants` (migration `0011`, hash-at-rest, atomic
  single-use `DELETE … rows_affected==1`) + `POST /device/authorize/{start,redeem}`.
  IDENTITY via **Path B (MLS exporter)**: at the post-add/post-join epoch both devices
  derive the same key via `DeviceGroup::export_secret`; the existing device seals
  `(grant_token ‖ phrase seed)` with `seal_with_key` (ChaCha20-Poly1305) into a
  magic-tagged rendezvous mailbox row; the fresh device opens it, redeems the grant
  for ACCESS, caches the seed for IDENTITY. Server stays opaque (the seed crosses the
  wire only sealed). `lakko-security`: **APPROVE**.
- **(b) Passkey.** Already issued a session token; the only change is the post-ACCESS
  ADR-0017 identity acquisition.
- **(c) TOTP — RFC 4226/6238, in-house (no new dep).** `totp_secrets` (migration
  `0012`, secret AEAD-encrypted at rest under `LAKKO_TOTP_ENC_KEY`); enroll bearer +
  recover anon + PoW; ±1-step window, replay via strictly-newer matched step + atomic
  CAS, 5-in-15-min lockout, enumeration-safe 401 with a dummy verify, 503 when
  unconfigured.
- **(d) Phone/email OTP — invariant #1 preserved.** `recovery_contacts` +
  `otp_challenges` (migration `0013`) store **only** a pepper-keyed BLAKE3 hash
  (purpose-byte-separated for contact vs code); the plaintext is transient, never
  stored or logged. Delivery behind a pluggable `OtpSender` (`LogOnlyOtpSender` default
  + a real `SmtpOtpSender` (lettre) + a Twilio-shaped `SmsOtpSender`, resolved per kind
  from env). recover/start is enumeration-safe (`{sent:true}` regardless, send only on
  a confirmed match); recover/finish is single-use + attempt-capped; expired challenges
  swept by the state pruner. `lakko-security` (c)+(d): **APPROVE-WITH-NOTES** — both
  load-bearing claims hold (ACCESS-only / never key material; plaintext never
  stored-or-logged); the one finding (an inaccurate pruner comment) was fixed.

The cross-cutting property holds: every factor lands the client in the **same**
post-ACCESS ADR-0017 identity-acquisition state machine. The lost-everything tail of
that machine (fresh-identity fallback) landed in Phase 3 — see ADR-0017
§"Implementation status".

**Deferred (out of scope here):** a live cross-platform 2-device e2e (no deployed
environment); the optional (d) SMS adapter is a config slot armed by deploy-time creds.

## Relationship to prior ADRs

- Companion to **ADR-0017** (phrase-seeded identity): the cascade restores ACCESS;
  ADR-0017's state machine restores IDENTITY. Factor (a) bridges both.
- Extends **ADR-0007** pairing (a sealed side-payload on the rendezvous channel)
  and reuses the **ADR-0014** single-use-grant pattern (`webauthn_action_grants`).

## Consequences

- Self-service recovery with no other people and no OOB contact re-verification on
  the common paths (another-device / phrase).
- New server surface: `device_grants` + 2 endpoints now; `totp_secrets` /
  `recovery_contacts` / `otp_challenges` + endpoints later. All ACCESS-only.
- Gate this ADR + each factor's implementation through `lakko-security` —
  especially factor (a)'s sealed identity transfer and the server-held-verifier
  classification for (c)/(d).
