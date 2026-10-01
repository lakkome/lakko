<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC (codespeak) for crates/lakko-calls/src/link_invite.rs — behavioral contract, reviewer-facing. Keep in lock-step with the module; the spec-drift gate fails CI if link_invite.rs changes and this file does not. -->

# `link_invite` — joiner/owner orchestration for link-based group calls (spec)

**Module:** `crates/lakko-calls/src/link_invite.rs`
**One line:** Turn a verified `CallInviteCapability` into a live MLS group + a `GroupCallSession` — the founding, admitting, and joining steps of a link-based call, plus the deterministic committer election that keeps a multi-joiner call single-group and failover-safe.

## Why this module exists

The wire codec for call-invite links lives in `lakko_contacts::call_invite` (decode + verify a signed `CallInviteCapability`). This module is the layer ABOVE that: it consumes an already-verified capability and drives the MLS side effects — create/join a group, add members, and derive the per-call `CallKey` seed via the MLS exporter so every participant agrees byte-for-byte. It carries no crypto of its own; it orchestrates the audited `lakko_mls` and `GroupCallSession` primitives into the two supported call topologies.

## Two topologies

**1×1 fast-path (ADR-0009), `accept_call_invite`.** The joiner founds a fresh MLS group (joiner at leaf 0), adds the owner via the capability's embedded one-time `KeyPackage`, and returns the Welcome bytes for the owner to consume. Single-use by construction: a second joiner reusing the same capability would found its own separate group at a different epoch, so the call would not connect. Retained for the genuine 1×1 case.

**Multi-joiner shareable links (ADR-0032), owner-founds.** For a link many strangers join, the **owner is the sole founder** (`found_call`, owner at leaf 0). Joiners never call `Group::create`, so there is exactly one group and no founding race — the G-21 two-groups bug is closed by design, and the embedded one-time owner-KP goes unused (the one-time-KP-in-a-shared-link problem disappears). Each joiner mints its own fresh `KeyPackage` (`request_call_join`), the acting committer adds it (`admit_joiner`), and the joiner enters via the returned Welcome (`join_call_from_welcome`).

## Public contract

- `accept_call_invite(capability, joiner_identity) -> AcceptedCallInvite` — the ADR-0009 path. Founds the group, adds the owner from the embedded KP, builds a `LinkBased`-scoped session keyed by `capability.call_id`, and hands back `{ group, session, welcome_for_owner }`. `set_joined` is NOT called — the platform decides when the joiner is "on the call".
- `found_call(owner_identity, call_id) -> FoundedCall` — the ADR-0032 owner side. Creates the sole-founder group + the session. Joiners are added later; no joiner ever founds.
- `request_call_join(joiner_identity) -> Vec<u8>` — a joiner mints a fresh serialized `KeyPackage`. Its private half persists in the identity's own provider, so the SAME identity can later open the Welcome. Never founds a group.
- `admit_joiner(group, committer_identity, joiner_key_package) -> AdmittedJoiner` — the acting committer adds a joiner's KP, advancing `group` to the new epoch, and returns `{ commit_for_members, welcome_for_joiner }`. The caller MUST already be the elected committer (see below) — a commit from a non-committer would fork the epoch.
- `join_call_from_welcome(welcome_bytes, joiner_identity, call_id) -> JoinedCall` — the joiner runs `Group::join_from_welcome` (with the private half from its earlier `request_call_join`) and builds the session, landing at the same `CallKey` seed as every other member at that epoch.
- `elect_committer(candidates) -> Option<u32>` — the deterministic committer election (below).

## State machine — committer election (`elect_committer`)

`elect_committer` is a **pure, total function** of a `[CommitterCandidate]` snapshot, where each candidate carries `{ leaf_index, is_owner, is_present }` — app-resolved facts (MLS membership × SFU presence × the capability's `owner_account_id`), never MLS internals.

Rule, in order:
1. If any present owner exists → the **lowest-leaf present owner** wins.
2. Else → the **lowest-leaf present member** wins.
3. Else (no member present) → `None` (a transient no-committer state).

Because every member computes the same answer from the same snapshot, **failover** (owner drops → the lowest-leaf present successor takes over) and **reclaim** (owner returns → the rule reverts to the owner) need no election protocol and cannot split-brain. A caller that mis-flags two owners still gets a deterministic answer (lowest leaf), so the result is independent of candidate ordering. The function reads only public state — no key material, no MLS mutation.

## Invariants upheld

- **Cross-party `CallKey` seed agreement (C-27).** Every path builds the session against the same `call_id` and the same MLS epoch, so the exporter yields the identical SFrame seed for all participants — asserted end-to-end across the 1×1 accept boundary and the multi-party / failover / handover flows (tests seal-and-open real frames both directions, deriving per-`(leaf, stream)` keys via `own_participant_key(stream_index)` / `remote_participant_key(kid)` on stream 0 per ADR-0035).
- **Single group, no founding race (ADR-0032, G-21).** On the owner-founds path only the owner ever calls `Group::create`; joiners only mint KPs and join via Welcome, so exactly one MLS group exists.
- **Every call-link group is a `GroupKind::Group` (ADR-0060).** Both founding paths (`accept_call_invite`, `found_call`) create the group with kind `Group`, because ADR-0032 admits joiners into it and a one-to-one would refuse every third person at every member's gate. Hiding call groups from the chat lists stays the clients' local bookkeeping, not a kind.
- **Deterministic, protocol-free committer transition (ADR-0032).** `elect_committer` is total and order-independent; failover and reclaim fall out of re-evaluating the snapshot.
- **Forward secrecy across committer changes.** Each Add / Remove commit advances the epoch, so a member's pre-leave seed never matches the post-handover seed (pinned in the owner-handover test).

## Failure modes

- `Error::Crypto(String)` — the single typed shape for any MLS-layer rejection: `Group::create`, a malformed/incompatible embedded or joiner `KeyPackage` (`add_member_from_wire`), Welcome encode/decode, KP mint/serialize, or `join_from_welcome` referencing a KP the provider does not hold. The message is prefixed with the failing entry point for triage.
- `Error::Exporter` — the MLS exporter rejects the call-key derivation (practically unreachable with valid inputs).
- `elect_committer` never errors; `None` is a valid answer (no present member).

## What this module does NOT do

- **Does not decode or verify the capability.** The caller runs `lakko_contacts::call_invite::decode_and_verify` first and passes the validated struct. This module trusts the `CallInviteCapability`.
- **Does not check expiry.** `is_valid_now(now_ms)` is the caller's job; any capability is accepted here — expiry is treated as a UX concern.
- **Does not deliver the Welcome / ferry any bytes.** Transport (SFU `signal-relay` for a live call, or the stranger-inbox `wrap_call_kp_forward` / `wrap_call_welcome` envelopes for cold start, routed by `owner_account_id`) is the platform's layer.
- **Does not enforce `max_uses` or per-joiner dedup.** The discovery `/c/<blob_id>` blob enforces `max_uses`; the acting committer dedups by `(call_id, joiner leaf key)` (ADR-0032, G-22). This module just founds / admits / joins.
- **Does not verify that the caller is the elected committer.** `admit_joiner` will commit for anyone; the deterministic election is what keeps exactly one committer active — enforcing it is the orchestration layer's responsibility.

## Attacker exposure

The bytes crossing this module are the embedded owner `KeyPackage` (from a signed, already-verified capability) and a joiner's minted `KeyPackage` (ferried from a stranger). Both reach `Group::add_member_from_wire`, whose MLS-layer validation rejects a malformed / wrong-ciphersuite / bad-signature KP as `Error::Crypto` before any group state is mutated (the corrupted-KP test flips a byte deep in the KP and asserts the reject). This module has **no wire parser of its own** — it does not decode attacker bytes directly, so it has no dedicated fuzz target; the attacker-facing codecs it depends on are fuzzed in `lakko-contacts` (`call_invite_capability_decode_and_verify`, `call_welcome_unwrap`, `call_kp_forward_unwrap`, `incoming_envelope_dispatch`) and the KP parser in `lakko-mls` (`key_package`).

The module's E2E negative test drives `lakko_contacts::incoming_envelope::dispatch_incoming_envelope` directly to pin that a truncated welcome payload surfaces as `Err`, never as a silent intro-message fallthrough. The dispatcher now serves several envelope kinds, so its error is the wrapping `lakko_contacts::DispatchError` — a truncated welcome arrives as `DispatchError::Call(CallWelcomeError::TruncatedPayload { .. })`, not as a bare `CallWelcomeError`.

## References

- ADR-0009 (link-based call invitations — the 1×1 fast-path), ADR-0060 (the conversation kind a call-link group is founded with), ADR-0032 (multi-joiner shareable call links — owner-founds + committer election), ADR-0035 (per-`(leaf, stream)` KID SFrame key separation — the `stream_index` / `kid` on the session's participant-key derivation; supersedes the ADR-0034 per-media-TYPE param)
- Invariants C-27 (cross-party CallKey seed agreement), C-32 / C-33 (Welcome + KP-forward envelopes), C-34 (owner-side dispatch order), C-35 (full link-call wire chain, 1×1 + multi-party), C-36 (owner-handover preserves the call-key chain across a mid-call committer leave); known gaps G-21 (multi-joiner, closed) and G-22 (per-joiner dedup, closed)
- Key tests (in-module `#[cfg(test)]`): `elect_owner_when_owner_present`, `elect_lowest_leaf_when_owner_absent`, `elect_reclaim_reverts_to_owner_on_return`, `owner_founds_multi_joiner_with_failover_and_reclaim`, `accept_invite_round_trips_via_welcome`, `accept_invite_with_corrupted_kp_rejects`, `link_call_full_e2e_via_welcome_envelope`, `link_call_multi_party_e2e_via_kp_forward_envelope`, `link_call_owner_handover_after_committer_leaves`, `dispatcher_rejects_truncated_welcome_in_full_chain`
