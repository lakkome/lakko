<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-anon-cred/src/client.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if client.rs changes and this file does not. -->

# `client` — the holder's side of the credential (spec)

**Module:** `crates/lakko-anon-cred/src/client.rs`
**One line:** Derive the account's credential secret, register its commitment, request and accept a credential, store it, and present it for a poll.

## Why this module exists

Every device of an account must compute the same secret `s` with nothing synced, so that the account has one tag per poll whichever device votes, and a vote must reveal neither the account nor a link to the same account's votes in other polls.

## Public contract

- `CredentialSecret::from_lineage_seed(&[u8; 32])` — `s` = `BLAKE3-derive-key("lakko/anon-credential/secret/v1", lineage_seed)` in XOF mode, 64 bytes, wide-reduced (the first 32 output bytes are the ADR's derive-key value; the extension makes the reduction unbiased). Zero is `ZeroScalar`. Wiped on drop; `Debug` prints nothing.
- `commitment()` — `C = s*G` (`Commitment`, 32 bytes, never the identity).
- `register()` — `CommitmentRegistration` = `C` with a proof of knowledge of `s`.
- `issuance_request(&key)` — `IssuanceRequest` for `key`'s period: a proof of knowledge of the `s` behind `C`, bound to the period and the issuer key. `key` must come from the issuer's key route, never from a response.
- `finalize_issuance(&key, &response)` — refuses a response for another period (`WrongPeriod`) and one whose proof does not show `U'` was computed under `key` for this `s` and period (`BadProof`); returns the `Credential`.
- `present(&credential, &key, &scope, message)` — `WrongPeriod` if `key` is not the credential's period, `MessageTooLong` above `MAX_MESSAGE_LEN` (1024, chosen). Draws fresh non-zero `r`, `z0`, `z1`; `U~ = r*U`, `C_s = s*U~ + z1*H`, `C_U' = r*U' + z0*G`, `T = s*H_poll`, and proves the presentation relation. Returns a `Presentation` (wire form in `verifier`).

## Wire layouts

- `CommitmentRegistration`: `version(1) || C(32) || c(32) || r(32)` = 97 bytes.
- `IssuanceRequest`: `version(1) || period(8 BE) || c(32) || r(32)` = 73 bytes. The commitment is not repeated; the issuer uses the one it stores.
- `Credential` (client storage only): `version(1) || period(8 BE) || U(32) || U'(32)` = 73 bytes.

Each `decode` requires the exact length and version 1, canonical points (`C`, `U`, `U'` not the identity) and canonical scalars; it does not verify proofs. Anything that decodes re-encodes to the same bytes.

## Invariants upheld

- The tag `T = s*H_poll` depends only on `s` and the poll, so one account has one tag per poll in every period and on every device (ADR-0090 4.2), and tags of different polls are unlinkable under DDH.
- Two presentations of one credential share no value but the tag, and share it only for one poll; none repeats `U`, `U'` or `C` (property tests).
- A key rotation (ADR-0074) keeps the lineage seed and therefore `s`; a key reset (ADR-0073) gives a new seed, a new `s` and new tags (the residual of ADR-0090 4.6).

## Measured (release build, Apple M3 Pro, this machine under a load average of about 30 from other builds; median of 200 runs per operation, range over five runs; the spread is the load, not a confidence interval)

- register: 110-276 us; issuance request: 120-299 us; finalize issuance: 334-774 us; present: 631-1386 us.
- Presentation size: 265 bytes.

## Attacker exposure

`finalize_issuance` runs on the issuer's response; a dishonest issuer cannot make the client accept a MAC under a key other than the published one. `Credential::decode` reads the client's own storage.

## References

- ADR-0090 4.2, 4.3, 4.7; the deviation from the ADR's ElGamal wording is stated in the crate root documentation.
- Tests: `tests/protocol.rs`; fuzz target `credential_decode`.
