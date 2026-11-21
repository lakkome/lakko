<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: codespeak behavioral spec for crates/lakko-contacts/src/x25519_binding.rs; keep in lock-step with the source (just spec-check). -->

# `x25519_binding` — the account's X25519 key, signed by its identity key (spec)

**Module:** `crates/lakko-contacts/src/x25519_binding.rs`   **One line:** the
signature that ties an account's sealing key to the Ed25519 identity key the directory,
the KT log and every pin check (security review 2026-11-21).

## Why this module exists

The X25519 key every sealed-to-account envelope is sealed to is derived from a seed of
its own and was served by the directory beside the identity key with nothing binding
the two, so a hostile directory could put its own sealing key beside a genuine
identity key. Now the identity key signs it, the directory refuses an upload without a
valid signature, and a client verifies it before sealing.

## Public contract

- `sign_x25519_binding(identity, x25519_pub)`: a 64-byte Ed25519 signature over
  `"lakko/account/x25519-binding/v1\0" ‖ ed25519_pub ‖ x25519_pub`. Deterministic.
  Golden: seed `[0x42; 32]` over `[0x24; 32]` gives `390fd885...a600`.
- `verify_x25519_binding(binding, ed25519_pub, x25519_pub)`: `BadLength` unless 64
  bytes, `BadKey` for an unparseable or weak identity key or an all-zero X25519 key,
  `BadSignature` otherwise when the signature does not verify.

## Invariants upheld

- A signature over another X25519 key, by another identity key, or for another
  purpose (a mailbox binding, a plain signature over the two keys) does not verify.
- Both keys are public: the directory may verify, and stores only a public artefact
  (invariant #2).

## Failure modes

`X25519BindingError::{BadLength, BadKey, BadSignature}`, each with a frozen `code()`.
No panic on any input.

## What this module does NOT do

It does not decide whether the identity key is trusted (the pin, the KT log and the
supersession gates do); it only says whether that key vouches for the X25519 key.

## Attacker exposure

The binding is served by the directory; fuzz target `x25519_binding_verify` and a
proptest of the same oracle.

## References

Security review 2026-11-21; `services/discovery/src/identity_bundle.rs` (required on
upload, migration `0041`); `mailbox.rs` (the template).
