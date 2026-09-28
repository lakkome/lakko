<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0058: Chat folders, carried between the account's own devices

- **Status:** Accepted (2026-09-28, the maintainer's call); S1 (proto, `lakko-mls` codec, `lakko-sync` fold, `lakko-ui-rules` rules, backup v5, both FFI surfaces) built 2026-09-28; clients not built
- **Date:** 2026-09-28
- **Context:** Phase 3 (product); UX spec [`docs/ux/chat-folders.md`](../ux/chat-folders.md)
- **Extends:** the DeviceGroup payload set of ADR-0007 Phase 2a
  (`schemas/mls-app/devicegroup_v1.proto`), ADR-0050 section 2.9 (what must
  follow a device), `lakko-backup`'s `BackupPayload`, and `lakko-ui-rules`
  (`docs/ux/navigation.md` section 8)

## Context

The UX spec defines a folder as a saved view of the merged conversation list:
a name, three kind toggles (one-to-one, groups, channels), three exclude
toggles (muted, read, archived), conversations added and excluded by hand, and
the folder's own ordered pins. The person's folder set must be the same on
every device of the account and must reach no one else - not the other members
of a conversation, not Lakko's servers. It is a map of how a person thinks
about their contacts, which is social-graph material (invariant #5).

Three facts about the existing machinery decide most of this.

1. **There is already a channel for exactly this kind of state.** The
   DeviceGroup is an MLS group whose leaves are the account's own devices;
   `devicegroup_v1.proto` carries `ReadCursor`, `DraftUpdate`, `ThreadFlags`
   (pin / mute / archive / unread override) and `ConversationOrder` over it,
   each stamped with a caller-supplied HLC and folded last-write-wins on every
   client (`DeviceGroupSyncContext.tsx`, `DeviceGroupSyncRepository.swift` /
   `.kt`). The server sees an opaque MLS application message on the account's
   own journal.
2. **That channel does not bring a new device up to date.** A device that
   joins the DeviceGroup cannot decrypt rows sealed before its join (forward
   secrecy), neither the pairing state bundle (ADR-0050 2.9,
   `devices/state-bundle.ts`) nor `BackupPayload` carries the synced maps, and
   PRODUCT-READINESS row "Drafts, pin, mute, archive" records the persisted
   fold that makes it survive a relaunch. So today a second device never
   learns the first device's pins. Folders built on the same channel inherit
   the gap unless this ADR closes it.
3. **Channels are not addressable by the existing flag key.** `ThreadFlags`
   and `ConversationOrder` name a conversation by MLS `group_id`; a channel
   subscription has a channel id and no group (`navigation.md` 6.1). A folder
   of channels needs a reference that names either.

Field numbers 14-17 of the `DeviceGroupPayload` oneof are claimed by
ADR-0050 2.9 for `ContactUpsert` / `ContactRemove` / `ChannelSubscribed` /
`ChannelUnsubscribed` (unbuilt). This ADR does not take them.

## Decision

The maintainer's choices on 2026-09-28: per-element merge inside a folder (not
last-write-wins per folder), and the catch-up path of section 5 covers the
existing synced maps (pins, mutes, archive, read cursors, drafts, conversation
order) as well as folders.

### 1. The folder state is a set of independently stamped registers

Every piece of folder state is a small register carrying **its own** HLC
stamp, so that two devices changing different parts of the same folder both
keep their change:

| register | key | value |
|---|---|---|
| meta | `folder_id` | name, `include_kinds`, `exclude_flags` |
| member | (`folder_id`, conversation ref) | `NONE` / `INCLUDED` / `EXCLUDED` |
| pins | `folder_id` | the ordered pin list, as one value |
| removed | `folder_id` | a tombstone |
| order | (the account) | the ordered folder ids, as one value |

A conversation's membership is **one** register with three states rather than
two independent sets, so "included" and "excluded by hand" cannot both hold
and the precedence rule of the UX spec never has to arbitrate between two
writes. `NONE` is the element tombstone: removing a chat from a folder writes
`NONE` with a fresh stamp rather than deleting the entry, so a device that has
not yet seen the removal cannot bring the chat back by re-sending its older
`INCLUDED`.

The pin list stays a single value: an ordered list has no cheap per-element
merge, it is at most ten entries, and two devices reordering the same
folder's pins at the same moment is the case where "the later order wins" is
what a person expects.

### 2. Carrier: two new DeviceGroup payload kinds

```proto
// oneof body, in DeviceGroupPayload (14-17 stay with ADR-0050 2.9)
FolderChanges folder_changes = 18;
StateSnapshot state_snapshot = 19;

message ConversationRef {
  enum Kind { KIND_UNSPECIFIED = 0; ONE_TO_ONE = 1; GROUP = 2; CHANNEL = 3; }
  Kind  kind = 1;
  bytes id   = 2;   // MLS group_id for ONE_TO_ONE / GROUP, channel id for CHANNEL
  reserved 3 to 7;
}

message FolderMeta {
  uint64 hlc = 1;
  bytes folder_id = 2;              // 16 random bytes, minted once
  string name = 3;
  uint32 include_kinds = 4;         // bit0 one-to-one, bit1 groups, bit2 channels
  uint32 exclude_flags = 5;         // bit0 muted, bit1 read, bit2 archived
  reserved 6;                       // icon, not in v1
  reserved 7 to 15;
}

message FolderMember {
  enum State { NONE = 0; INCLUDED = 1; EXCLUDED = 2; }
  uint64 hlc = 1;
  bytes folder_id = 2;
  ConversationRef conversation = 3;
  State state = 4;
  reserved 5 to 7;
}

message FolderPins    { uint64 hlc = 1; bytes folder_id = 2; repeated ConversationRef pinned = 3; reserved 4 to 7; }
message FolderRemoved { uint64 hlc = 1; bytes folder_id = 2; reserved 3 to 7; }
message FolderOrder   { uint64 hlc = 1; repeated bytes folder_ids = 2; reserved 3 to 7; }

// A live edit: only the registers that changed, each with its stamp.
message FolderChanges {
  repeated FolderMeta    metas   = 1;
  repeated FolderMember  members = 2;
  repeated FolderPins    pins    = 3;
  repeated FolderRemoved removed = 4;
  FolderOrder            order   = 5;   // absent = unchanged
  reserved 6 to 15;
}

// The catch-up re-send of section 5: the whole folded state of this device,
// every entry with its ORIGINAL stamp.
message StateSnapshot {
  message Stamped { uint64 hlc = 1; oneof entry {
    ThreadFlags flags = 2; ReadCursor cursor = 3; DraftUpdate draft = 4; } }
  repeated Stamped entries = 1;
  message StampedOrder { uint64 hlc = 1; ConversationOrder order = 2; }
  StampedOrder conversation_order = 2;  // absent = none held
  FolderChanges folders = 3;
  uint32 part = 4;                       // 0-based
  uint32 parts = 5;                      // >= 1
  reserved 6 to 15;
}
```

Each register carries its stamp **inside** the body because one message
carries many registers of different ages: a snapshot re-sends state written
over months, and a live edit that adds a chat and renames the folder writes two
registers that another device may later supersede separately. The envelope's
`DeviceGroupPayload.hlc` stays what it is today, the stamp of the message, and
the fold ignores it for these two kinds.

The existing kinds (`ReadCursor`, `DraftUpdate`, `ThreadFlags`,
`ConversationOrder`) are unchanged on the wire and keep taking their stamp
from the envelope; `StateSnapshot` wraps them with an explicit stamp only so a
re-send can carry the original one.

Nothing here reaches a conversation group or a server in the clear: the
payloads are DeviceGroup application messages, the DS stores ciphertext on the
account's own journal (invariant #2), and no conversation member learns that
their conversation is in a folder (invariant #5).

### 3. The fold

One pure function, written once in Rust, consumed by the three clients over
both FFI surfaces (the ADR-0044 argument: three hand-written LWW folds with a
tiebreak are exactly where clients disagree silently). Its home is
`lakko-sync` (HLC + LWW; today wasm-only), which gains a UniFFI exposure.

- **Every register:** an incoming entry replaces the stored one when its stamp
  is strictly greater. On equal stamps the canonical encoded bytes of the
  entry decide, bytewise, larger wins - the only discriminator every receiver
  has and that survives a verbatim re-send (ADR-0044). Byte-identical entries
  are the same write. So the fold is commutative, associative and idempotent:
  any two devices that have seen the same entries hold the same state,
  whatever the order they arrived in. This is the property the fold's tests
  pin.
- **Deletion is final** (the maintainer's call, 2026-09-28): a `FolderRemoved`
  hides every register of that folder whatever its stamp, and a register for
  a deleted folder that arrives later is refused. Deleting is a person's
  deliberate act; an edit that raced it from another device is lost rather
  than bringing back a folder the person deleted. Because folder ids are
  random and minted once, a deleted id is never reused. Tombstones and `NONE`
  members are kept; pruning either lets a device that was offline across the
  change resurrect what was removed.
- **A folder is shown only once its meta register has arrived.** Registers of
  one folder can arrive in different messages (a snapshot is split into
  parts), and a folder with no name is not something to draw.
- **Order:** folder ids in the order that name no live folder are skipped; a
  live folder absent from the order is appended by the stamp of its **current**
  meta register, then by folder id. (The first draft said "first meta
  register"; the first one is not held after a rename, so it cannot converge
  through a snapshot.) A concurrent create and reorder lose neither.
- **Existing maps:** the four synced maps fold exactly as they do today
  (`shouldApply`, strictly newer by stamp), plus the same bytewise tiebreak,
  which today's clients lack (an exact HLC tie is currently decided by arrival
  order). The fold moves into Rust for them too, so the tiebreak exists once.

### 4. Bounds, fail-closed decode

The decoder refuses the whole payload, never truncates it, when: a
`folder_id` is not 16 bytes; a name is empty, longer than 24 scalar values or
96 bytes, or fails the display-name character rule (`lakko_contacts::profile`);
a ref's kind is unspecified or its id is empty or longer than 64 bytes; an
unknown bit is set in either bitfield (a newer client's toggle this build
cannot honour must not be read as "off"); a pin list exceeds 10; an order
exceeds 256 ids; a `FolderChanges` carries more than 1024 registers; a
`StateSnapshot` part has `part >= parts` or `parts` above 64. A refused payload
is skipped; the next write of those registers from any device supersedes it.

The limits the person meets - 20 folders, 200 hand-added and 200 hand-excluded
chats per folder, 10 pins - are **client** limits on creation, not decode
bounds. Two devices can each add the 200th chat, or each create the 20th
folder, concurrently; dropping one would lose data. The fold keeps everything;
a client over a limit shows everything and refuses further additions.

These numbers are chosen, not measured.

### 5. Catch-up for a device that arrives late

Two routes, both required, because either alone leaves a case with no state:

1. **Growth re-send.** When a device observes the DeviceGroup gain a leaf, the
   device with the lowest leaf index among those that were members **before**
   the growth publishes a `StateSnapshot` of its whole folded state - folders
   and the four existing maps - each entry with its **original** stamp. A
   re-stamp would make stale state newer than a live edit (ADR-0044 F-A). It
   waits a random delay of up to 30 seconds first, so the new device's own
   first writes are not raced; a duplicate snapshot from another device is
   harmless because the fold is idempotent. A snapshot larger than 256 KiB
   encoded is split into parts (the DS accepts 1 MiB per publish); parts are
   independent, each folds on its own, and `part` / `parts` exist only so a
   receiver can log a missing part. This covers pairing, the ADR-0049
   confirmation, the ADR-0050 re-admission arm, and a restore while another
   device is alive.
2. **Backup.** `BackupPayload` v5 (v4 was already taken by `known_group_ids`)
   carries the same folded state, as the `lakko-sync` persistence bytes. A restore
   with no other device alive seeds the fold from it; later writes supersede
   by stamp as usual. The backup stays the phrase-sealed opaque blob of
   ADR-0037; storage learns nothing new.

A device never receives its own DeviceGroup messages, so a client folds every
local write into its own state as it publishes it.

The pairing state bundle is **not** extended: the growth re-send reaches the
new device through the DeviceGroup seconds later, and two carriers of the same
state are two places for it to disagree.

Drafts ride the snapshot too. They are messages the person has not sent, and
they already travel between the account's devices on this channel today; a
new device receiving them is the same exposure as any other device.

### 5a. Unresolvable references are carried, never dropped

A device may hold registers that name a conversation it does not have yet (the
Welcome has not arrived; a channel not yet subscribed here). The fold stores
them like any other. Because an edit writes only the registers it changes,
nothing a device does in the normal course re-writes a register it did not
touch - which removes the hazard a whole-folder write carried, of a half-synced
device deleting chats it could not see. A snapshot re-sends every register the
device holds, resolved or not.

### 6. Shared rules in `lakko-ui-rules`

Written once, on both FFI surfaces:

- `folder_matches(folder, row) -> bool` - the rule of UX spec 3.2 (included by
  kind toggle or `INCLUDED`, not `EXCLUDED`, not muted / read / archived under
  the respective toggle).
- `order_conversations_in_folder(rows, folder_pins) -> Vec<u32>` - the
  `order_conversations` rule with the folder's pin list in place of the global
  pin flag, pins in their stored order.
- `ConversationRow` gains `muted` and `has_unread`.

The **selected tab** stays a per-device client preference and is not synced.

## Consequences

- No server, DS, auth or push change. No `lakko-mls` state-machine change:
  the two kinds ride the existing `create_devicegroup_message` /
  `parse_devicegroup_payload`, whose body enum and proto grow two variants (a
  codec change inside `lakko-mls`, with property tests and an extension of the
  existing `devicegroup_payload` fuzz target, which now also drives the new
  bounds).
- `lakko-sync` gains the fold for folders and the four existing maps and a
  UniFFI exposure; the three clients' hand-written folds are deleted in favour
  of it. `lakko-ui-rules` gains two rules and two row fields. `lakko-backup`
  bumps to v5 (no production data: v1-v4 are refused, per the pre-deployment
  rule).
- Pins, mutes, archive, read cursors and drafts reach a newly linked or
  restored device for the first time.
- An older client that meets a new kind returns `Unsupported` from
  `parse_devicegroup_payload` and skips it. It never re-publishes a kind it
  cannot parse, so it cannot delete anyone's folders.
- The fold keeps tombstones forever; on an account that creates and deletes
  folders for years this grows without bound, at 24 bytes per removed folder
  and about 90 bytes per removed membership. Accepted; a compaction would need
  an agreement between all devices on what every device has seen, which does
  not exist.
- The folder set enlarges what a stolen, unlocked device reveals (it already
  holds the conversations themselves); nothing changes for a server-side
  adversary.

## Alternatives considered

- **A later edit brings a deleted folder back** (the second draft). Rejected by
  the maintainer: the deletion is the person's act and should stand.
- **Last-write-wins per folder** (the first draft of this ADR). Simpler and
  smaller, but two devices adding different chats to one folder at nearly the
  same moment lose one of them, silently. The maintainer chose the
  per-element merge.
- **Server-side folder storage** (an encrypted blob per account on
  `services/storage`). A second per-account mutable blob with its own rollback
  question (ADR-0037), and a write pattern that tells the operator when the
  person reorganises their chats. The DeviceGroup already exists for this.
- **Folders inside the pairing state bundle.** Covers linking only, not
  restore and not re-admission; the growth re-send covers all of them.
- **One message per register on re-send, using the envelope stamp.** No wire
  change for the four existing maps, but a snapshot of a few hundred
  conversations becomes a few hundred journal rows.

## Open questions

1. The limits in section 4 and UX spec 3.4.
2. Whether the selected tab should ever follow the person (today: no).
