<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0017: Phrase-seeded AccountIdentity — decouple identity from the password

- **Status:** Accepted — implement (Part 1 of the account-recovery overhaul); 2026-06-09
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
