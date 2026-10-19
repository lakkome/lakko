<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0015: Device-removal authorization — threat analysis and peer-verified passkey attestation

- **Status:** Accepted — implement the peer-verified attestation design (the maintainer elected to close the on-device-malware residual rather than accept it; 2026-06-05). **All five sprints landed; G-13 is CLOSED.**
- **Date:** 2026-06-05

> **AMENDED 2026-09-02 (documentation audit).** The title said
> *"(deferred) peer-verified passkey attestation"* and the Consequences said
> *"Until sprints 3–5 land…"*. **Both were stale — the design in this ADR was
> built end to end**, and G-13 has been marked CLOSED in
> [`docs/security/known-gaps.md`](../security/known-gaps.md) since. What
> shipped, verified against the tree in this audit:
>
> - **The primitive** — [`crates/lakko-attest`](../../crates/lakko-attest/src/lib.rs)
>   `ActionAttestation::sign` / `verify_bound` (121-byte fixed layout).
> - **The signing key + mint** — `services/auth` (its own Ed25519 attestation
>   key, migration 0010, published at `/.well-known/device-attest-jwks.json`).
> - **The policy flag (sprint 4)** — `DeviceGroup::set_require_attested_removals`
>   / `require_attested_removals`
>   ([`device_group.rs`](../../crates/lakko-mls/src/device_group.rs)).
> - **The apply-path gate (sprint 3)** —
>   `DeviceGroup::process_incoming_verifying_removals`, which refuses to merge
>   an unattested removal under the policy.
> - **The FFI + all three clients (sprint 5)**, with the policy
>   auto-enabled once a passkey is registered, and a build-time key pin per
>   platform so a compromised serving tier cannot substitute the verifying key.
>
> The threat analysis below — *why* the delete-passkey grant mechanism does not
> transfer to device removal — is unchanged and is still the most useful part
> of this record.

> **AMENDED 2026-10-19 by [ADR-0081](./0081-devicegroup-commits-cannot-strand.md)
> ("Amendment (2026-10-19)").** Under the policy the apply-path gate also
> accepts a removal covered by a **self-removal proof**: a signature by the
> removed leaf's own key naming the epoch the commit leaves and the
> KeyPackage that replaces the leaf, honoured only when that commit leaves
> that epoch and adds that KeyPackage. It rides in the same envelope slot as
> an attestation (137 bytes under its own domain, against the attestation's
> 121). It lets a device that lost its DeviceGroup state
> replace its own old leaf without a passkey; it authorises no other
> removal, so every removal of a device by someone else still needs an
> attestation. C-85.
- **Context:** G-13 (`docs/security/known-gaps.md`) — the WebAuthn
  action-binding work added a *client-side* passkey gate before removing a
  device from the DeviceGroup. This ADR asks whether that gate can be made
  *server-enforced* the way delete-passkey now is (ADR-0014, single-use
  grant consumed by `DELETE /webauthn/credentials/:id`), and concludes the
  honest answer is "not with the same mechanism, and the residual threat it
  would close is narrow."

## Context

ADR-0014 made one sensitive action — deleting a passkey credential —
genuinely server-enforced: `/auth/action/finish` mints a single-use grant
and the `DELETE` endpoint (owned by `services/auth`) consumes it, so a
stolen session token alone can no longer delete a credential. The natural
question is whether the *other* gated action, **removing a device from the
DeviceGroup**, can get the same treatment.

It cannot, for a structural reason, and — more importantly — the threat
that motivated the delete-passkey enforcement **does not apply to
device-removal in the same way**. This ADR records the analysis so the
decision is auditable, and sketches the only design that *would* add
enforcement (peer-verified attestation) together with an honest cost it
would carry.

## Threat model (grounded in the code)

Device removal is **not** an auth-service HTTP call. It is an MLS commit:

1. The removing device builds an MLS `Remove` commit over the DeviceGroup,
   signed by **that device's per-device MLS signature key**
   (`crates/lakko-mls/src/device_group.rs::remove_device_at` →
   `identity.signature_keys`).
2. The commit bytes are posted to the Delivery Service journal
   (`POST /ds/group/{group_id_hex}/messages`). The DS stores/forwards
   opaque bytes — it is zero-trust and **cannot read the commit** (security
   invariant #2).
3. Every other paired device polls the journal and applies the commit
   (`device_group.rs::process_incoming`), advancing the epoch and
   ratcheting group secrets out of the removed leaf's reach.

The per-device MLS signing key is derived `Identity::fromSeed(seed)` from a
32-byte DeviceIdentity seed that is stored **encrypted under a
non-extractable device key** (web: `extractable: false` WebCrypto AES-GCM;
native: Secure Enclave / StrongBox per invariant #7). Producing a valid
`Remove` commit therefore requires *both* the wrapped seed (in on-device
storage) *and* the non-extractable device key — neither of which leaves the
device.

Consequences for the attacker model:

- **Remote attacker holding only a stolen session token: cannot remove a
  device.** They can POST arbitrary bytes to the DS journal, but they
  cannot forge a validly-signed `Remove` commit (no MLS signing key), and
  peers reject anything that fails MLS validation. So the "stolen
  bearer-token replayed against the endpoint" threat — the exact threat
  ADR-0014's grant closes for delete-passkey — **is already closed for
  device-removal by the hardware-protected MLS signing key.**
- **Residual threat: on-device malware** running with the user's
  privileges. It can unwrap the seed via the device key and drive the FFI
  to sign a `Remove` commit directly, bypassing the client UI (and thus the
  client-side passkey gate). This is the only attacker the current gate
  fails against.

## Why the obvious enforcement points don't work

- **DS-side enforcement is impossible.** The DS cannot interpret an
  `action_hash` or read which leaf a commit removes (invariant #2). Making
  it check a grant would require it to parse the ciphertext — a direct
  violation of the zero-trust posture.
- **Auth-service enforcement has nothing to gate.** Unlike delete-passkey,
  the auth service is never in the device-removal path; the effect happens
  peer-to-peer via the DS. There is no server request to refuse.

## The only design that adds enforcement: peer-verified attestation

If we want server-attested, *peer-enforced* device removal, the
verification has to move to **the receiving devices**, since they are the
only honest parties that actually read the commit:

1. **Attestation issuance.** Extend `/auth/action/finish` so that, for a
   remove-device `action_hash`, it returns a short-lived **signed
   attestation** — an Ed25519 (EdDSA) token over
   `(account_id, action_hash = H("lakko/action/remove-device/v1" ‖
   removed_leaf_signature_pubkey), issued_at)`, signed by an auth-service
   key published at the existing JWKS endpoint (reuse the
   `lakko-call-jwt` JWS machinery). The attestation can only be obtained by
   completing a real passkey gesture, which on-device malware cannot fake.
2. **Embedding.** The removing device carries the attestation alongside the
   `Remove` commit (an MLS `GroupContext` extension, or a custom proposal /
   an application message committed in the same epoch — the exact framing
   is a sub-decision).
3. **Peer verification.** `device_group.rs::process_incoming`, before
   merging a staged commit that contains a `Remove`, verifies the
   attestation's signature against the published auth key and that its
   `action_hash` matches the leaf actually being removed. If absent or
   invalid → **refuse to apply** the commit (a new invariant alongside
   I-1..I-8). This is a `lakko-mls` change and therefore requires property
   tests + (post-Phase-2) external crypto audit per invariant #3.
4. **The OPAQUE-only wrinkle.** Accounts with no passkey cannot produce an
   attestation, yet must still be able to remove devices. Peers do not know
   whether an account has a passkey (it is an auth-service concept absent
   from the MLS group). So enforcement cannot be unconditional. The clean
   fix is a **DeviceGroup policy flag** in the `GroupContext` extensions —
   "this group requires attested removals" — flipped on (via an attested
   commit) when the account first enables a passkey. Peers enforce the flag
   they see in authenticated group state; OPAQUE-only groups never set it.
   This is additional protocol surface (a policy extension + the
   set-policy flow + its own property tests).

## Cost / benefit (the honest part)

- **Cost:** a cross-cutting **protocol change** — a new MLS commit-framing
  for the attestation, attestation verification in the shared
  `lakko-mls` apply path (the audited core, invariant #3), a new
  refuse-to-apply invariant + property tests, attestation issuance + JWKS
  reuse in `services/auth`, a `GroupContext` policy extension and its
  set-policy flow, and a re-implementation of the verify path's awareness
  on every client. This is several sprints and touches the most
  security-sensitive crate in the tree.
- **Benefit:** it closes exactly one residual — **on-device malware that
  bypasses the client UI** — and only partially: an attacker already able
  to run code with the device key and drive the MLS FFI is already inside
  the device's trust boundary. The headline threat (remote token replay) is
  already closed by the hardware-backed MLS key, independent of this work.

## Decision

**Implement the peer-verified attestation design.** The analysis above
showed the remote-token-replay threat is already closed by the
hardware-backed MLS key; the maintainer elected to additionally close the
on-device-malware-bypasses-the-UI residual rather than accept it. Because
this touches the audited `lakko-mls` core, it is gated by invariant #3
(ADR — this one — plus property tests, and the apply-path change is in
scope for the Phase 2 external crypto audit).

### Attestation token (foundational primitive)

A compact, domain-separated, Ed25519-signed binary blob — NOT a JWT, to
keep the audited-core verifier trivial and fuzzable:

```text
ActionAttestation = version(1) ‖ account_id(16) ‖ action_hash(32)
                    ‖ expires_at_unix_secs(8, BE i64) ‖ Ed25519_sig(64)
signing input      = "lakko/device-attest/v1" ‖ version ‖ account_id
                     ‖ action_hash ‖ expires_at
```

`sign`/`verify` build on `lakko_crypto::signing` (the same Ed25519
primitive `lakko-call-jwt` uses), so both `services/auth` (sign) and
`lakko-mls` (verify) reuse one implementation with no JWS / JWKS-fetch
dependency in the audited core — the trusted verifying key is supplied by
the caller (the FFI / client holds it from pinned/bootstrap config). The
key SHOULD be a dedicated attestation key (key separation from the
call-JWT key), published alongside the existing JWKS.

### Sprint breakdown

1. **`lakko-attest` crate (this sprint).** The `ActionAttestation`
   format + `sign` + strict `verify_bound(bytes, vk, expected_account,
   expected_action_hash, now)` + unit/property tests (+ a later fuzz
   target on the parser). Self-contained; no MLS or auth changes.
2. **Auth issuance.** A dedicated attestation signing key in
   `services/auth` (published via JWKS), and `/auth/action/finish`
   returns the signed attestation alongside its existing response.
3. **MLS apply-path verification.** `device_group.rs` carries the
   attestation alongside the `Remove` commit (envelope framing) and
   `process_incoming` refuses to apply a `Remove` whose attestation is
   absent / invalid / unbound to the removed leaf's signature key — a new
   refuse-to-apply invariant with property tests.
4. **OPAQUE-only policy.** A DeviceGroup policy flag ("require attested
   removals") so peers know when to enforce. **Feasibility spike done
   (2026-06-06) — CONFIRMED.** `device_group.rs::spike_custom_group_context_extension_propagates_to_joiners`
   proves OpenMLS 0.8.1 accepts a custom `Extension::Unknown(0xFF01, …)`
   in the GroupContext at creation, the founder reads it back via
   `group.extensions().unknown(type_id)`, and a second device that joins
   via Welcome sees the identical flag — so peers can enforce a policy
   carried in shared, authenticated group state. The application-level
   fallback is NOT needed. The one requirement the spike surfaced: every
   device's leaf capabilities (both the founder's and each joiner's
   `KeyPackage`) must advertise `ExtensionType::Unknown(type_id)`, so
   `Identity::create_key_package` + `DeviceGroup::create_founder` gain that
   capability in the implementation. Flipping the flag on later (when an
   account first enables a passkey) uses `update_group_context_extensions`.
5. **Client embedding.** Each platform threads the `/auth/action/finish`
   attestation into the remove-device commit; the verify is already in
   the shared core.

The decision remains reversible per-sprint. The sprint-4 OpenMLS spike has
since run and **confirmed** the GroupContext policy-flag approach is
feasible on 0.8.1 (custom extensions create + read + propagate), so the
main remaining risk is retired.

## Alternatives considered

- **Replicate the delete-passkey grant mechanism.** Rejected: there is no
  server request in the device-removal path to gate, and the DS cannot
  enforce (opacity).
- **Have the DS require a grant before accepting a DeviceGroup write.**
  Rejected: the DS would have to identify the operation type / target leaf,
  which means reading the ciphertext — a zero-trust violation. It also
  could not distinguish a removal from a benign key update.
- **Unconditional peer-enforced attestation (no policy flag).** Rejected:
  breaks device-removal for OPAQUE-only accounts, who have no passkey to
  attest with.
- **Implement peer-verified attestation now.** ~~Held in reserve (this ADR's
  design section) rather than rejected; deferred on cost/benefit grounds.~~
  **This is what was chosen and built.** The line above survives from the draft
  written before the maintainer decided; the Status line has said "implement"
  since 2026-06-05 and the five sprints landed. Kept rather than deleted
  because the cost/benefit reasoning is the argument the decision was taken
  *against*, and that is worth being able to read.

## Consequences

- The shipped client-side passkey gate stays; the attestation work makes
  it peer-enforced so on-device malware that bypasses the app UI can no
  longer evict a device without a real passkey gesture.
- The `lakko-mls` apply-path change (sprint 3) is gated by invariant #3:
  this ADR + property tests, and the change is in scope for the Phase 2
  external crypto audit.
- A new signing key + JWKS surface is added to `services/auth` (sprint 2);
  the attestation verifying key is distributed to clients via the same
  pinned/bootstrap channel the rest of the trust anchors use.
- Sprint 4's OpenMLS-0.8 feasibility risk is **retired** — a spike
  confirmed custom GroupContext extensions create + read back + propagate
  to joiners on 0.8.1 (the application-level fallback is unnecessary);
  the only implementation requirement is advertising the extension type in
  every device's leaf capabilities.
- ~~Until sprints 3–5 land, known-gaps G-13 continues to record the
  on-device-malware residual as open (now *in progress* rather than
  accepted).~~ **Sprints 3–5 landed; G-13 is CLOSED** (see the amendment note
  at the head). The one item left open there is optional and is *not* this
  residual: distributing the pinned attestation verifying key through the
  signed bootstrap config, which needs Phase-5 client bootstrap consumption.
  Each client pins the key at build time today, which already defends a
  compromised serving tier — but not a compromised signing key, and that is
  the auth service's own trust tier by construction.
