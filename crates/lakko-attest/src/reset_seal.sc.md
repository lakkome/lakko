<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: codespeak behavioral spec for crates/lakko-attest/src/reset_seal.rs; keep in lock-step with the source (just spec-check). -->

# `reset_seal` — the seal announcement and the reset seal (spec)

**Module:** `crates/lakko-attest/src/reset_seal.rs`   **One line:** the two
client-signed formats that let a contact tell a reset made by the account's
owner from one the operator minted (ADR-0101 section 10, ADR-0073 R.2 rule 4).

## Why this module exists

A key reset is accepted on the auth attestation, the generation and the KT
log, all of which the operator controls. A sealed reset adds a signature by a
key the account announced under its old account key before the reset, made
from a secret the server never holds (the password's OPAQUE export key, a
passkey's WebAuthn `prf` output, or the recovery phrase). Without it a
contact's device moves nothing (ADR-0101 section 11).

## Public contract

- `password_seal_key(export_key: 64)`, `passkey_seal_key(prf_output: 32)`,
  `phrase_seal_key(phrase_seed: 64)`: Ed25519 keys from
  `BLAKE3-derive-key("lakko/account/reset-seal/{password,passkey,phrase}/v1", input)`.
  The three contexts separate them; the seed buffer is zeroed after use.
- `passkey_prf_salt()`: `SHA-256("lakko/account/reset-seal/passkey-prf/v1")`.
  `passkey_seal_tag(credential_id)`: the first 16 bytes of
  `SHA-256(credential_id)`.
- `SealAnnouncement::new` sorts entries into the canonical order; `sign`
  checks every entry rule and signs with the account identity key;
  `decode_unverified` checks every rule but the signature; `open` also
  verifies it; `lists(kind, seal_pk)` answers whether a key is announced
  under a kind.
- `ResetSeal::sign` refuses a signing key other than the named `seal_pk`,
  generations that do not follow, and unusable keys; `open` decodes and
  verifies under the key the seal names.
- `check_reset_seal(SealCheckInput)`: rule 4's signature half. `Ok(SealedReset
  { announcement_seq, kind })` only when the previous announcement verifies
  under the held key, names this account and the accepted generation, has a
  `seq` at or above the caller's floor, and the seal verifies, names that
  account, `seq`, `from = accepted`, `to = attested` and the fetched key, and
  its key is listed under its kind.

## Wire layout

- Announcement: `"lakko/account/reset-seals/v1\0" ‖ 0x01 ‖ account_ref(32) ‖
  key_generation(8 BE) ‖ seq(8 BE) ‖ issued_at_ms(8 BE) ‖ count(1..=10) ‖
  count × (kind ‖ tag(16) ‖ seal_pk(32)) ‖ sig(64)`; 200 to 641 bytes; the
  signature covers every byte before it.
- Reset seal: `"lakko/account/reset-seal/v1\0" ‖ 0x01 ‖ account_ref(32) ‖
  announcement_seq(8 BE) ‖ from_generation(8 BE) ‖ to_generation(8 BE) ‖
  new_signature_pubkey(32) ‖ kind ‖ seal_pk(32) ‖ sig(64)`; always 214 bytes.

## Invariants upheld

- Exactly one password entry, at most eight passkey entries, at most one
  phrase entry; password and phrase tags all zero; entries strictly ascending
  by `(kind, tag)` (one encoding, no repeated passkey tag).
- Every announced seal key and the new account key parse as Ed25519 and are
  not of small order.
- `to_generation == from_generation + 1`.
- Both domain separators differ from every other separator of the crate and
  prefix none (test in `donation.rs`).

## Failure modes

`ResetSealError` (`badLength`, `badDomain`, `badVersion`, `unknownKind`,
`badCount`, `badEntries`, `unexpectedTag`, `notCanonical`, `badKey`,
`badGenerations`, `badSignature`, `keyMismatch`) and `SealRefusal`
(`noAnnouncement`, `noSeal`, `announcementInvalid`, `announcementAccount`,
`announcementGeneration`, `announcementRolledBack`, `sealInvalid`,
`sealMismatch`, `sealKeyNotAnnounced`), each with a frozen `code()`. No path
panics: every read goes through a bounds-checked cursor.

## What this module does NOT do

- It does not check the KT log: "the newest announcement of `g-1` before the
  reset leaf" is `lakko_transparency::reset_seals`.
- It does not decide the verdict: `lakko_contacts::reset_verdict` combines
  this result with the attestation and the log.
- It does not keep the `seq` floor; the caller does, and raises it on a
  sealed verdict.

## Attacker exposure

Both formats are served by the operator (discovery's bundle columns, the KT
reset leaf). Length and count are bounded before any entry is read. Fuzz
targets `reset_seals_open` and `reset_seal_open` assert that whatever decodes
re-encodes to its own bytes; proptests assert round trip, refusal of any
flipped byte, and no panic on arbitrary input.

## References

ADR-0101 section 10, ADR-0073 "Amended 2026-11-26 (second)". Tests:
`a_sealed_reset_passes_for_each_kind`, `every_unsealed_case_is_refused`,
`announcement_rules_are_enforced`, `golden_pins_the_derivations_and_the_wire`.
