<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0015: Device-removal authorization — threat analysis and (deferred) peer-verified passkey attestation

- **Status:** Proposed
- **Date:** 2026-06-05
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

**Defer the peer-verified attestation protocol; document the residual.**
The current posture for device-removal is:

- remote token-only attacker — **blocked** by the hardware-protected
  per-device MLS signing key (not by anything client- or auth-side);
- the client-side passkey gate (already shipped) raises the bar for the
  normal app flow on a non-compromised device;
- the remaining residual — **on-device malware bypassing the UI** — is
  documented in known-gaps G-13 as an accepted limitation, to be revisited
  if the threat model elevates (e.g. a high-risk / enterprise profile),
  at which point this ADR's peer-attestation design is the path.

This keeps the audited `lakko-mls` core untouched for a narrow,
partially-mitigated residual, rather than taking on a multi-sprint
protocol change with external-audit implications. The decision is
explicitly reversible: if device-removal becomes a higher-value target,
re-open this ADR and implement the design above.

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
- **Implement peer-verified attestation now.** Held in reserve (this ADR's
  design section) rather than rejected; deferred on cost/benefit grounds.

## Consequences

- Device-removal keeps its shipped client-side passkey gate; no change to
  the audited `lakko-mls` core.
- known-gaps G-13 records the residual (on-device-malware UI bypass) as an
  accepted, documented limitation with this ADR as the implementation path
  if it is ever elevated.
- If re-opened, the work is gated by invariant #3 (ADR + property tests +
  external crypto audit for the `lakko-mls` apply-path change).
