<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-RC-10 — The threat model and invariant #5 cite "blinded per-conversation presence tokens" as an active social-graph protection; no such mechanism is wired on any platform

- **Severity:** Low (finder's proposal; `audit-verify` calibrates)
- **Confidence:** CONFIRMED
- **Workstream:** recon
- **Tracking id:** LAK-RC-10
- **Status:** Open
- **Component:** `docs/security/threat-model.md` (asset table row "Social graph",
  invariant #5, mass-correlation adversary row); `CLAUDE.md` invariant #5;
  `crates/lakko-presence/src/lib.rs:24-55`
- **Invariants touched:** #5 (the social graph is not materialised on the server)
- **Vendor claim contradicted:** `docs/security/threat-model.md`, three places:
  *"Social graph | Client-side only | Add-contact = encrypted envelope to a
  per-recipient pseudonym; presence = blinded per-conversation tokens"*;
  invariant #5, *"presence = blinded per-conversation tokens"*;
  mass-correlation row, *"presence blinded per-conversation"*

## Preconditions

None. The claim is made in the present tense in the project's top-level security
document and in the invariant list reproduced in `CLAUDE.md`.

## Mechanism

**Step 1 — the mechanism has no caller on any platform.**
`cargo tree -i lakko-presence --workspace` yields exactly one reverse dependency,
`lakko-ffi-wasm`, i.e. the web binding layer — it is not a dependency of
`lakko-ffi-uniffi`, so it does not exist on iOS or Android at all.

Grepping each client tree for the crate's exported surface —
`PresenceToken`, `deriveToken`, `PresenceTracker`, `presence_seed` — returns
**zero** matches in each of `apps/web/src`, `apps/ios/Lakko`,
`apps/android/app/src/main`.

**Step 2 — the key it needs does not exist.** The crate specifies its own input at
`crates/lakko-presence/src/lib.rs:29-30`:

```
//! - `presence_seed` is a 32-byte secret derived from the MLS group
//!   exporter (label `"lakko/presence/v1"`, scoped per group).
```

and lists at `:55` — in its own "what is not here" section — *"Sourcing
`presence_seed` from the MLS exporter"* as outstanding work. Grepping
`crates/lakko-mls/src` for `presence_seed` or `lakko/presence` returns nothing.
So the mechanism has neither a caller nor a key source.

**Step 3 — the project's own module map already says this**, in a place the
threat model does not reach. `CLAUDE.md`'s crate table for `lakko-presence` reads:
*"**Unwired**: exposed on wasm only, no UniFFI, no client caller, and the
`presence_seed` MLS exporter it is specified against does not exist."* That is
accurate. The threat model, written for external readers and for exactly this kind
of review, says the opposite three times.

## Reproduction

```
cargo tree -i lakko-presence --workspace
for d in apps/web/src apps/ios/Lakko apps/android/app/src/main; do
  grep -rc 'PresenceToken\|deriveToken\|PresenceTracker\|presence_seed' "$d"; done   # 0 0 0
grep -rn 'presence_seed\|lakko/presence' crates/lakko-mls/src                        # no output
grep -n 'presence' docs/security/threat-model.md                                     # 3 present-tense claims
```

## Impact

Invariant #5 names three defences for the social graph: pseudonymous contact
envelopes, OPRF-blinded discovery, and blinded per-conversation presence tokens.
Two exist. The third does not, and its absence is stated as its presence in the
one document an external reviewer is pointed at.

The practical position is better than the claim in one respect and worse in
another, and both should be said:

- **Better:** Lakko emits no presence signal at all, which is a stronger privacy
  posture than emitting a blinded one. Nothing is leaking *because of this*.
- **Worse:** the mass-correlation adversary row lists this as one of four
  mitigations against pattern-matching across server logs, so the row overstates
  the defence-in-depth actually present. And the activity signals Lakko *does*
  emit — DS journal writes (readable without authentication, **LAK-RC-01**),
  ADR-0045 committer beacons every 60 s per admin, ADR-0024 typing indicators
  persisted to the 30-day journal prune, and push wakes — are exactly the traffic a
  presence design would have had to reason about, and no document reasons about
  them under invariant #5.

So the harm is not a leak; it is that a reader (including the next auditor, and
including the maintainer prioritising work) believes a control is in force, does
not look for its absence, and does not notice that the signals it would have
covered are being emitted by other subsystems instead.

## Recommendation

1. **Delete the three present-tense references** in `docs/security/threat-model.md`
   and the copy in `CLAUDE.md`'s invariant #5, or mark them explicitly as planned.
   The replacement claim is stronger and simpler: *no presence signal is emitted.*
2. **State what invariant #5 is actually up against.** If presence is dropped as a
   defence, the row should name the activity signals that do exist — journal
   writes, committer beacons, typing indicators, push wakes — and say which are
   accepted residuals. ADR-0042/0043 already do this work for push; the top-level
   document is where it should be visible.
3. If a presence feature is still intended, `crates/lakko-presence`'s own
   prerequisite (an MLS exporter at `"lakko/presence/v1"`) is the first missing
   piece and the ADR should say so.

## Notes

- Closely related to **LAK-RC-05**, which covers the same crate as *dead code*.
  They are filed separately because the root causes and the fixes differ: LAK-RC-05
  is "unused code ships to browsers, remove it", and this one is "a security
  document asserts a control that does not exist, correct it". `audit-verify` may
  merge them; if it does, the documentation half should survive, because it is the
  half that misleads.
- `crates/lakko-contacts/src/committer_beacon.rs:61` contains a careful,
  well-argued section titled *"Why not `lakko-presence`"* explaining why the beacon
  did not reuse it. That reasoning is sound and shows the team knows the crate is
  unwired — which makes this squarely a documentation-synchronisation gap rather
  than a misunderstanding.
- I did not assess whether the other two invariant-#5 defences (pseudonym
  envelopes, OPRF discovery) are correctly implemented. That is crypto/backend
  workstream territory and no claim is made here.
