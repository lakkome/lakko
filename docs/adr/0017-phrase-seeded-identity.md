<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0017: Phrase-seeded AccountIdentity — decouple identity from the password

- **Status:** Accepted — **built + reviewed** (Part 1 of the account-recovery
  overhaul). Phrase-seeded identity + on-device seed store + fresh-device
  acquisition + G-4 emit-skip landed on web + iOS + Android; the lost-everything
  fallback landed (Phase 3, see below); `lakko-security` passed (APPROVE-WITH-NOTES,
  findings closed). 2026-06-09 (designed) → 2026-06-12 (built)
- **Date:** 2026-06-09
- **Context:** Account recovery must be self-service and must not change the
  user's long-term identity on a routine recover. Today
  `AccountIdentity = Identity::from_seed(exportKey)` where the OPAQUE `exportKey`
  is password-derived (ADR-0007), so recover (which rotates the password)
  rotates the identity, and the only honest way for contacts to trust the new
  key was an out-of-band re-verification. The maintainer rejected that: a routine
  recovery must restore the **same** identity. This ADR amends ADR-0007's
  identity-derivation rule and renders ADR-0016 (the supersession transport)
  dormant. ADR-0018 covers the companion access-factor cascade.

## Decision

Derive the **AccountIdentity** from the user's stable **BIP39 recovery-phrase
seed**, not the password `exportKey`. The recovery phrase is generated at signup,
never sent to the server, survives password changes, already seals the backup
envelope, and is already enrolled as a separate OPAQUE recovery envelope
(`opaque_registrations` kind=RECOVERY, distinct from the password envelope). The
password and its `exportKey` remain — but only as an **auth factor** and as the
**wrapping-key seed** for on-device secret storage, no longer as the identity
seed.

Consequences of the decision:

- The AccountIdentity is **stable across password changes and recovery** — the
  pubkey contacts pin never churns on a routine recover or password reset.
- The G-4 / ADR-0016 supersession machinery goes **dormant** — there is no longer
  a rotation to bridge on the recover path (see below). It is kept, not deleted.
- A user genuinely loses their identity only if they lose the recovery phrase
  **and** every device — at which point recovery via an access factor (ADR-0018)
  yields a fresh identity and contacts re-verify once (the existing
  TOFU / safety-number path). That is the only remaining identity change.

## Scope — verified to be client-side only

- `crates/lakko-mls/src/identity.rs::from_seed` is **seed-agnostic** (it
  HKDF-expands arbitrary input bytes under the pinned salt). The change is **which
  seed the clients pass**, not `lakko-mls`. **The MLS state machine is NOT
  touched** — so no MLS ADR/property-test burden beyond a determinism test, and
  no `services/auth` change (the AccountIdentity seed stays client-side,
  invariant #2 preserved).
- **Blast radius (audited):** `Identity.fromSeed(secrets.exportKey)` is the
  canonical-identity pattern used in **~30 call sites across all three clients**
  (the chat / contacts / channels / auth repositories), not only the auth
  orchestration — the clients re-derive the AccountIdentity from the cached
  `exportKey` on most operations. The clean approach is to thread a dedicated
  `accountIdentitySeed` (the phrase seed) through each client's `IdentitySecrets`
  / identity-provider and switch the `fromSeed` sites to it (keeping `exportKey`
  for the device-key derivation + passkey-resume). Mechanical but sizable, ×3.

## On-device identity-seed store (new)

Today the password `exportKey` is cached wrapped under a non-extractable hardware
device key (web `auth/export-key-store.ts`, iOS `ExportKeyStore.swift`, Android
`ExportKeyStore.kt`). Add a parallel **`account-identity-seed-store`** holding the
BIP39 phrase seed, wrapped under the **same** hardware-backed device key — the
wrapping key stays password-derived (`deriveDeviceRootKey(exportKey, accountId)`);
only the wrapped payload changes, preserving invariant #7. Copy the existing
32-byte-seed store `apps/web/src/auth/device-identity-store.ts` (+ iOS/Android
mirrors). It is the highest-sensitivity on-device secret and is wiped on
`forgetDevice` alongside the others.

## Per-flow behaviour

- **Signup:** generate phrase → `AccountIdentity = fromSeed(phraseSeed)` → cache
  the phrase seed. Device key + exportKey cache unchanged.
- **Same-device login (password or passkey):** read the cached phrase seed →
  `fromSeed(phraseSeed)`. No phrase entry — the warm common path.
- **Fresh-device login / recover (no cached seed):** obtain the seed from (i) the
  user entering the recovery phrase, or (ii) another device over the E2EE pairing
  channel (ADR-0018 §another-device) → cache it.
- **Lost-everything fallback (access via an ADR-0018 factor, no phrase, no
  device):** mint a fresh random AccountIdentity, publish a new bundle **without**
  a proof (none is mintable); contacts fail-closed → re-verify once.

### Accepted consequence: fresh-device password-login UX regression

Because the password no longer contains the identity, a fresh device that logs in
with **only** a password now also needs the recovery phrase (or a paired device)
to restore the AccountIdentity. This is unavoidable given the decoupling and is
surfaced honestly ("New device — enter your recovery phrase or pair an existing
device to restore your secure identity"). The client must **never** silently mint
a fresh identity on a fresh-device password login (that would silently break every
contact's pin); minting fresh is reserved for the explicit lost-everything path.

## Migration: pre-launch → mass re-derive, no shim

The server stores no identity key material (only OPAQUE envelopes + the public
discovery bundle). Pre-launch, the cutover simply re-derives every account's
AccountIdentity from the phrase seed; any existing local TOFU pins re-TOFU. **No
migration code.** (If a specific test account must survive a peer's pin, the
existing `mintSupersessionProof` can be run once as a one-shot drain —
old=`fromSeed(oldExportKey)`, new=`fromSeed(phraseSeed)`. Default: don't.)

## G-4 / ADR-0016 supersession → dormant (retained)

- **Verify path:** with a stable identity, a fetched bundle pubkey always equals
  the local pin → the verify-on-fetch gate (`apps/web/src/contacts/client.ts`)
  hits its "pin == fetched, proceed" branch and never calls `verifySupersession`;
  the bundle's `supersession_proof` column stays NULL. Correct by construction.
- **Emit path (active trap to fix):** `recover()` currently mints a supersession
  proof unconditionally off the exportKey. It must be rewritten to compare the
  **old vs new AccountIdentity pubkey** (both phrase-derived → equal → **skip emit
  entirely**). "old==new account pubkey ⇒ skip" becomes an explicit invariant with
  a test. Without this fix, recover would emit a spurious proof for a key nobody
  pins.
- **Retain `crates/lakko-transparency`** — it is the substrate for a future
  explicit "rotate my identity (phrase compromised)" action, which would
  deliberately re-activate the supersession + (eventual) KT-log chain path.
  Dormant is not deleted.

## Implementation status (built + reviewed)

Landed end-to-end on web + iOS + Android, each step build/test-verified:

- **Phrase-seeded identity + seed store.** `AccountIdentity = fromSeed(phraseSeed)`
  at every canonical site; the `account-identity-seed-store` (web IndexedDB AES-GCM,
  iOS Keychain, Android EncryptedSharedPreferences) wraps the 64-byte seed under the
  hardware-backed device key and is wiped on `forgetDevice`.
- **Fresh-device acquisition + hard-closed verify.** A fresh device with no cached
  seed parks `needs-recovery-phrase`; `provideRecoveryPhrase` derives the seed and
  **HARD-BLOCKS** against the published bundle (`verifySeedAgainstPublishedIdentity`):
  a mismatch / 404 / network error refuses (`SupersessionPhraseMismatch` /
  `IdentityVerificationUnavailable`), never blind-TOFU (`lakko-security` Part-1 #1).
- **G-4 emit-skip.** `recover` compares old vs new AccountIdentity pubkey and skips
  the supersession proof when equal — always, on a phrase-seeded recover — and
  fail-loud on the impossible mismatch. Supersession is dormant as designed.
- **Lost-everything fallback (Phase 3).** The explicit escape hatch for a user who
  recovered ACCESS via an ADR-0018 factor but has no phrase and no other device:
  `mintFreshIdentity` (reachable only post-ACCESS, behind an explicit warned button)
  mints a fresh random phrase + AccountIdentity, **publishes the new bundle WITHOUT a
  proof — the single sanctioned publish-without-proof — and propagates a publish
  failure before any local mutation** so a network blip never strands the account,
  shows the new phrase (acknowledge-gated), then lands the session. Contacts then
  re-verify once OOB (the accepted degradation). `lakko-security` Phase-3A:
  APPROVE-WITH-NOTES — adjudged **consistent with invariant #6** (no proof is mintable
  when the old seed is gone; the acceptance-side fail-closed gate is intact) and the
  ACCESS-hijack residual **strictly no-worse** than the pre-existing ACCESS threat
  (OOB safety-number re-verify is the backstop); the torn-state + cleanup findings
  were fixed.

**Deferred (defence-in-depth, not blockers):** a notification / audit entry on
fresh-identity mint; a server-side per-account cool-down on fresh-identity publishes;
a KT-log "fresh start" marker (needs the KT-log chain verifier — a separate work item)
to let contacts distinguish an owner reset from an active attacker.

## Relationship to prior ADRs

- **Amends ADR-0007** (multi-device identity), which defined
  `AccountIdentity = Identity::from_seed(exportKey)` as a property. The per-device
  `DeviceIdentity` (random, the actual MLS leaf, already decoupled from the
  password) is unchanged; MLS group membership and the DeviceGroup are unaffected.
  Making the account-level key stable across password changes is a net positive
  for multi-device coherence (the published account fingerprint stops churning).
- **Renders ADR-0016 dormant** for the recover path (its entire purpose was to
  carry a recover-time rotation proof). ADR-0016 is retained per above.

## Consequences

- Routine recovery and password change preserve the identity; contacts never
  re-verify on those paths.
- Cleaner invariant: AccountIdentity is tied to the recovery factor (the phrase),
  not the password.
- Cost: a sizable cross-cutting client change (~30 call sites ×3 clients + the
  `IdentitySecrets` plumbing + a new per-platform seed store) and an accepted
  fresh-device password-login UX regression. No server change; no MLS change.
- Gate this ADR + the Part-1 implementation through `lakko-security` before merge.
