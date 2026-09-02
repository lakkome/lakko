<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — MLS state machine

Lakko wraps [OpenMLS](https://github.com/openmls/openmls) ≥
0.8 with a thin Rust facade. The facade fixes the
ciphersuite, isolates upstream API churn, and exposes a
Lakko-specific operation surface. **The facade — not OpenMLS
itself — is what this audit pass covers.** Any
state-confusion / re-key bypass / downgrade bug in the
wrapper is in scope; equivalent bugs inside OpenMLS are
out of scope (OpenMLS gets its own external audit upstream).

## Crate layout

| Module | Responsibility |
|---|---|
| [`crates/lakko-mls/src/identity.rs`](../../crates/lakko-mls/src/identity.rs) | `Identity` — signing keypair + credential + per-device storage provider. `from_seed` builds a deterministic Identity from a 32-byte seed (HKDF → Ed25519). |
| [`crates/lakko-mls/src/group.rs`](../../crates/lakko-mls/src/group.rs) | `Group` — single MLS conversation group. `create` / `join_from_welcome` / `add_member` / `remove_member` / `create_application_message` / `process_incoming` / `export_secret`, **plus the ADR-0045 proposal surface** — `propose_add_member` / `propose_remove_member` / `propose_group_roles` / `propose_ownership_transfer`, `pending_proposals`, `own_committer_rank`, the staged sweep (`stage_pending_proposals` / `has_staged_sweep` / `merge_staged_sweep` / `abandon_staged_sweep`), the role-verifying receive path `process_incoming_verifying_roles`, the external-join quarantine (`pending_external_joins` / `approve_external_join` / `reject_external_join`), and `is_active`. |
| [`crates/lakko-mls/src/group_proposals.rs`](../../crates/lakko-mls/src/group_proposals.rs) | ADR-0045 — the **one** per-proposal authorisation rule (`ProposalKind` / `classify` / `authorise`), the commit-level `owner_survives` (**C-63**), `commit_effects`, and the sweep filter's exclusion pass. |
| [`crates/lakko-mls/src/group_roles.rs`](../../crates/lakko-mls/src/group_roles.rs) | ADR-0022 — `RoleTable` / `Role` and the `0xFF02` GroupContext codec, plus ADR-0045's `committer_rank`. |
| [`crates/lakko-mls/src/refusal.rs`](../../crates/lakko-mls/src/refusal.rs) | `RefusalReason` / `RefusalDisposition` — the machine-readable reason a membership operation was refused, so a client selects copy from a type rather than substring-matching core prose. |
| [`crates/lakko-mls/src/publisher_group.rs`](../../crates/lakko-mls/src/publisher_group.rs) | ADR-0010 — `PublisherGroup`, a `Group` with a deterministic id and the `lakko/channel/v1` exporter. Carries **no** role table, which is why it keeps the direct external-join approve. |
| [`crates/lakko-mls/src/welcome_rendezvous.rs`](../../crates/lakko-mls/src/welcome_rendezvous.rs) | ADR-0027 — the bounded `WelcomeRendezvous` envelope a not-yet-member matches by its own signature key. |
| [`crates/lakko-mls/src/device_group.rs`](../../crates/lakko-mls/src/device_group.rs) | `DeviceGroup` — per-user multi-device MLS group (ADR-0007). Same operations as `Group` plus a deterministic `group_id_for(account_id)`. |
| [`crates/lakko-mls/src/key_package.rs`](../../crates/lakko-mls/src/key_package.rs) | `KeyPackage` creation + wire decoding with validation, plus `create_external_join_proposal` (ADR-0027). |
| [`crates/lakko-mls/src/pairing.rs`](../../crates/lakko-mls/src/pairing.rs) | Pairing-QR codec (ADR-0007 §"Add-device"). |
| [`crates/lakko-mls/src/history_bundle.rs`](../../crates/lakko-mls/src/history_bundle.rs) | History catch-up bundle exporter + AEAD seal / open (Phase 2a3d8). |
| [`crates/lakko-mls/src/history_placeholder.rs`](../../crates/lakko-mls/src/history_placeholder.rs) | Bounded codec for the cross-platform history-bundle payload placeholder (G-6). |
| [`crates/lakko-mls/fuzz/`](../../crates/lakko-mls/fuzz/) | `cargo fuzz` harnesses — **eight**: `mls_message`, `key_package`, `history_placeholder`, `devicegroup_payload`, `pairing_qr`, `history_bundle_open`, `role_table_decode`, `welcome_rendezvous_decode`. (This row listed two for a long time; every entry point is in `fuzz_api` in [`lib.rs`](../../crates/lakko-mls/src/lib.rs).) |

## Invariants the wrapper MUST preserve

These are the load-bearing properties downstream code
relies on. Each cites the test(s) that validate it.

### I-1: A Welcome targeting a different `account_id` is rejected before any state advance

DeviceGroup uses `group_id = BLAKE3("lakko-device-group/v1"
\|\| account_id)`. The receiver re-derives the expected
`group_id` from its own `account_id` and rejects any inbound
Welcome whose embedded group_id doesn't match — BEFORE
calling `MlsGroup::process_message`.

Test: `device_group::tests::join_rejects_welcome_for_a_different_account`
in [`device_group.rs`](../../crates/lakko-mls/src/device_group.rs).

### I-2: `remove_device_at(own_leaf)` is rejected

DeviceGroup remove path pre-checks the caller's
`own_leaf_index()` and refuses to commit a self-remove —
that would render the local view inconsistent on
`merge_pending_commit`. The user removes their own device
via the `forgetDevice` path, not via Remove.

Test: `device_group::tests::remove_device_at_rejects_own_leaf`.

### I-3: `remove_device_at(unknown_leaf)` is rejected before OpenMLS sees it

Pre-check against the current member list. Bails with a
clean "no such device leaf N" error instead of letting
OpenMLS surface a less specific internal failure.

Test: `device_group::tests::remove_device_at_rejects_unknown_leaf`.

### I-4: After a successful `add_device_from_wire` / `join_from_welcome` pair, both devices' exporter secrets agree

The post-pairing exporter secret (label
`lakko/history-bundle/v1`) is the symmetric AEAD key the
history catch-up bundle (Phase 2a3d8) round-trips on. If
the two views were to derive different secrets, the
joiner couldn't decrypt the scanner's bundle.

Test: `history_bundle::tests::exporter_agrees_across_paired_devices`.

### I-5: Sealing a `HistoryBundle` larger than `MAX_BUNDLE_MESSAGES = 1000` is rejected

Defensive cap on the producer side — prevents an out-of-
budget bundle from saturating the rendezvous mailbox.

Test: `history_bundle::tests::seal_refuses_oversized_bundle`.

### I-6: Opening a `SealedBundle` with a wrong key surfaces as an authentication failure, not a parse failure

The AEAD layer (`lakko-crypto::aead::open`) returns
`Error::Aead` on tag mismatch. The history-bundle open
maps that to `Error::Message`, which the caller surfaces as
"history bundle apply failed" — distinct from
"unsupported algorithm".

Test: `history_bundle::tests::seal_open_rejects_wrong_key`.

### I-7: Opening a `SealedBundle` with an unknown `algorithm` discriminator is rejected before AEAD-open is attempted

Forward-compatibility hook. Older clients reading a
future-version bundle bail with `Error::Unsupported`
instead of attempting an open that would fail with a less
informative error.

Test: `history_bundle::tests::open_rejects_unknown_algorithm`.

### I-8: `create_devicegroup_message` round-trips through `parse_devicegroup_payload` losslessly for every typed body variant (`ReadCursor` / `Draft` / `Flags` / `Order`)

Phase 2a3e wire format invariant — receivers must
reconstruct the same Rust enum the sender encoded.

Test: `device_group::tests::devicegroup_payload_each_variant_roundtrips`.

## Exporter labels in use

Every exporter label Lakko coins is namespaced under
`lakko/...` so the global RFC-9420 label registry stays
free of collisions. Current set:

| Label | Length (bytes) | Source | Used for |
|---|---|---|---|
| `lakko/history-bundle/v1` | 32 | `lakko_mls::history_bundle::exporter_secret` (`HISTORY_EXPORTER_LABEL`) | Per-pairing AEAD key for the history catch-up bundle (Phase 2a3d8) |
| `lakko/call/v1` | 32 | `lakko_calls::sframe::CallKey::derive` (Phase 3a4) | Per-call SFrame seed; per-participant keys derived via HKDF over `(seed, kid_be)` with info `lakko/sframe/v1/key` and nonce salt under `lakko/sframe/v1/nonce`. **The HKDF input is the full RFC 9605 header KID, not the sender leaf** — [ADR-0035](../adr/0035-sframe-per-stream-kid-key.md) made `KID = (sender_leaf << 8) \| stream_index` so a leaf's concurrent streams derive disjoint key + nonce spaces; this row said `sender_leaf_be`. See [`calls.md`](./calls.md). |
| `lakko/channel/v1` | 32 | `lakko_mls::publisher_group::CHANNEL_EXPORTER_LABEL`, consumed by `PublisherGroup::channel_seed` | Per-epoch channel content seed ([ADR-0010](../adr/0010-channels-two-tier-broadcast.md)). **Was missing from this table** — it is a production label in the core, not a test constant. |
| `lakko/device-recovery/v1` | 32 | Client-side constant (`RECOVERY_EXPORTER_LABEL` on web, `recoveryExporterLabel` on iOS, `RECOVERY_EXPORTER_LABEL` on Android), passed to `DeviceGroup.exportSecret` | Per-pairing key sealing the ADR-0018 Path-B recovery payload `(grant ‖ phrase seed)`. **Also missing.** It has no Rust-side production constant — the label is supplied by the caller, and the only in-crate occurrence is a `device_group.rs` test — which is exactly how it escaped this table, and is the case the rule below is weakest against. |

A new label requires an ADR amendment + a test +
re-running this audit pack. **That rule was not followed for the last two rows**,
and the reason is worth recording rather than just fixing: `export_secret` takes the
label as a **parameter**, so a new label can be coined entirely outside
`crates/lakko-mls` — by a client, as `lakko/device-recovery/v1` was — and nothing in
the crate can notice. There is no gate here, only this list.

## MLS-state-machine rules that live in the `C-` class

Several rules about this state machine are numbered `C-`, not `I-`, because that is
where the numbering happened to be when they were minted: **C-63** (a commit must never
leave the group with an owner who is not in it), **C-64** (the committer election and the
staged-sweep lifecycle), **C-65** (one role-table change in flight), **C-66** (a group
must never lose the ability to change its own membership), and the receiver half of
**C-67** (the cursor may lag the durable MLS state, never lead it). All five are
[ADR-0045](../adr/0045-remote-commit-epoch-fork.md) rules about proposal handling, commit
composition and the receive path — this document's subject, not the call layer's.

**They are deliberately not renumbered.** Those numbers are already cited in commit
messages, code comments and each other's cross-references, and git history is immutable —
a rename would leave dangling references in the log for as long as the repository exists,
which is a worse defect than a filing error.

**What the misfiling actually cost, and what fixes it.** This section carries an
obligation the `C-` section does not: a change requires an ADR amendment, a test, and
**re-running this audit pack**. A rule filed under `C-` silently escapes that — which is
exactly how the two exporter labels above went missing. So the obligation is extended
here rather than the rules moved: **the five entries named above are governed by this
document's re-check rule, wherever they are numbered.** A change to any of them requires
the same ADR amendment, test and re-read as a change to `I-1`..`I-8`.

The rule of thumb for the next one, since the numbering will drift again: **file by what
the rule constrains, not by which section was last edited.** If breaking it would produce
a wrong group state rather than a wrong call, it belongs to this document's obligation
even if its number says otherwise.

## State-flow diagrams

### Conversation group lifecycle

```text
   ┌─ Identity::generate / Identity::from_seed
   │                              │
   │                              ▼
   │           Identity::create_key_package ─────► published to discovery
   │                              │
   │                              ▼
   │       Group::create  ──or──  Group::join_from_welcome
   │              │                      │
   │              ▼                      ▼
   │      ──────  Group  ──────────────────────────────────┐
   │         │   │                                          │
   │         │   ├─ add_member(KeyPackage)                  │
   │         │   │     └─► (commit, welcome)                │
   │         │   │                                          │
   │         │   ├─ remove_member(LeafNodeIndex)            │
   │         │   │     └─► commit                           │
   │         │   │                                          │
   │         │   ├─ create_application_message              │
   │         │   │     └─► encrypted bytes                  │
   │         │   │                                          │
   │         │   └─ process_incoming(bytes) ────► Incoming  │
   │         │                                              │
   │         ▼                                              │
   │      export_secret(label, ctx, n) ◄────────────────────┘
   │
   └─ snapshot_storage() / restore_storage(blob)
```

**This diagram is the pre-ADR-0045 shape and is kept because it is still exactly
what a group with NO role table does.** A group that carries one — which is every
group `Group::create` makes, since ADR-0022 — takes the path below instead, and the
difference is the whole point of [ADR-0045](../adr/0045-remote-commit-epoch-fork.md):
a membership change stops being one atomic call.

```text
   member                          elected committer (committer_rank)
     │                                        │
     ├─ propose_add_member / _remove_ /       │
     │  _group_roles / _ownership_transfer    │
     │        └─► proposal bytes ─── DS ─────►│
     │            (advances NO epoch)         │
     │                                        ├─ stand down if any journal row
     │                                        │  is unapplied
     ▼                                        │
   process_incoming_verifying_roles ──────────┤
     │   receipt gate: authorise(author)      │
     │     ├─ stored   ─► pending set         ├─ stage_pending_proposals
     │     └─ dropped  ─► Incoming::Proposal  │     └─► commit staged, still at N
     │                    { stored: false }   │          publish ──► DS
     │                                        │
     │   commit gate: authorise(EVERY         ├─ read the verdict: apply the rows
     │   proposal against ITS OWN author)     │  below our own commit, ask whether
     │     + owner_survives (C-63)            │  our epoch moved
     │     └─ refuse-to-merge, never          │     ├─ moved   ─► merge_staged_sweep
     │        apply-then-warn                 │     └─ did not ─► abandon_staged_sweep
     │                                        │
     └─ is_active() ─► false once a commit removes our own leaf
```

Two properties of this shape a reader must not infer wrongly. A staged sweep
**freezes that group's own membership operations** and is **durable**, so a client
that stages and never adjudicates wedges membership across restarts — reaching a
verdict is a client obligation, not an optimisation. And a staged sweep does **not**
block a chat send (`create_message` gates on `is_active`, not `is_operational`),
which is why all three clients hold the per-group lock across seal-and-publish.

### DeviceGroup lifecycle (ADR-0007)

```text
   first device ─► create_founder(account_id)
                              │
                              ▼
                     ┌────────────────┐
                     │   DeviceGroup  │ ◄── join_from_welcome (other device)
                     └────────────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
    add_device_from_wire  remove_device_at  create_devicegroup_message
        │                      │                     │
        └─► (commit,welcome)   └─► commit            └─► encrypted bytes
              │                      │                     │
              ▼                      ▼                     ▼
      published on DS         broadcast on DS        DS group journal
       mailbox + journal       group journal               │
                                                           ▼
                                                process_incoming →
                                                parse_devicegroup_payload →
                                                  (ReadCursor / Draft / Flags / Order)
```

## Known soft spots — explicitly out of scope, tracked for follow-up

See [`known-gaps.md`](./known-gaps.md) for the canonical list.
The MLS-side items currently parked there:

- **Channel two-tier flow** (>50k subscribers) — ~~sender-key
  bootstrap + content-key rotation. The wire shape lands in
  Phase 4; not in this audit pass.~~ **BUILT** (crypto,
  service, UI, rotation cron, batched fanout and multi-epoch
  retention all landed in 2026-07; `PublisherGroup` and
  `crates/lakko-channels`). What G-1 still tracks is the
  deploy-gated >50k **scale proof**, not the wire shape.
- **MLS-2 / quantum hybrid suites** — pinned for later
  upgrade once the IETF lands the hybrid cipher suite
  identifiers. **Still open** (G-2).
- **External-join proposals** — ~~currently returned as a
  defensive `Incoming::ExternalJoinProposal` variant but
  no UI surface drives them.~~ **Driven end to end on all
  three clients** (G-3 closed): the variant carries the
  joiner's signature key and a `retained` flag, a role-gated
  `approve_external_join` / `reject_external_join` pair acts
  on it, and since [ADR-0045](../adr/0045-remote-commit-epoch-fork.md)
  S7 an approval is an `Add` **proposal the approving admin
  authors itself** rather than a direct commit. The retained
  set is a **quarantine**, deliberately kept separate from the
  member proposal store so a `/g/` link-holder cannot fill the
  store the sweep consumes.

**Three properties in this document have no numbered invariant and arguably want
one; this is a pointer, not a mint.** The ADR-0045 receipt gate (an unauthorised
proposal is dropped, and the drop is reported as data), the staged-sweep lifecycle
(nothing merges until the journal has ordered it), and `is_active` (a group the
device has been evicted from can never decrypt again, at any epoch) are all
load-bearing and are currently pinned only as **C-63 / C-64 / C-66 / C-67 / C-68**
in [`calls.md`](./calls.md) — which is where the ADR-0045 arc put them, because the
`C-` class had already absorbed ADR-0022's role table. Whether any of them belongs
in the `I-` class instead is a maintainer call, recorded here so the omission is
deliberate rather than accidental.
