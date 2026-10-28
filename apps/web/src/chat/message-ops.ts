// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0023 — pure, DOM-free message-op overlay logic for the
 * chat timeline (edit / delete / reactions).
 *
 * The `Chat.tsx` render path keeps a session-scoped overlay keyed
 * on each target message's DS `messageId`; this module owns the
 * two security-relevant pieces of that machinery so they can be
 * unit-tested without the SolidJS / wasm surface:
 *
 *   1. [`applyOp`] — fold an (already-authorized) op into a
 *      mutation-overlay map, returning a NEW map (replace-to-
 *      rerender for SolidJS). Edit sets the text, delete sets the
 *      tombstone, reaction toggles `(reactor, emoji)`.
 *
 *   2. [`editDeleteAuthorized`] — the author-only integrity gate
 *      (ADR-0023, invariant #4): an edit / delete is applied ONLY
 *      if the op's sender is the target message's original author.
 *      Reactions are accepted from any member (and are not routed
 *      through this gate).
 *
 * Both are total + pure — no throw, no I/O — so a malformed op the
 * receive loop already structurally validated can never wedge the
 * timeline.
 *
 * ## Identity is a signature key, never a leaf index
 *
 * Both rules compare a [`MemberIdentity`] — the member's Ed25519
 * signature public key, base64 — resolved from the MLS sender leaf
 * at receipt, in the epoch the message arrived. They used to
 * compare the raw leaf, and that was the bug: OpenMLS hands a
 * vacated leaf to the next member added, so a `remove Bob, add
 * Dave` commit (ADR-0045 composes both into ONE commit) leaves
 * Dave holding Bob's index, and a leaf comparison then reads Dave
 * as Bob — free rein to edit and delete Bob's history and to
 * toggle off Bob's reactions. See `author-identity.ts` for the
 * resolution rule and the reason it must happen at receipt.
 */

import {
  OWN_REACTION_SENTINEL as OWN_SENTINEL,
  type MemberIdentity,
} from "./author-identity";

/** The session-scoped mutation overlay for one message, keyed in
 *  the timeline's `msgOps` map on the target's DS `messageId`.
 *  A message with no entry renders verbatim. */
export interface MessageOpState {
  /** Latest edited text. `undefined` ⇒ never edited (render the
   *  original). The author-only gate is enforced before apply. */
  editedText?: string;
  /** Set once a delete op has been applied — renders the
   *  "message deleted" tombstone. */
  deleted?: boolean;
  /** The DS `messageId` of the latest edit or delete op applied, when
   *  known: the version a cached translation or transcript of this
   *  message is keyed on (ADR-0082 section 5). `undefined` = no such op
   *  seen, or one applied without an id (a local-only op). */
  lastOpId?: number;
  /** emoji → set of reactor IDENTITIES (base64 signature keys),
   *  never leaf indices: a departing member's leaf is handed to
   *  the next member added, so a leaf-keyed set lets a newcomer
   *  toggle the departed member's reaction off and merge their own
   *  into the departed member's count. A sentinel
   *  ([`OWN_REACTION_SENTINEL`]) records an own reaction whose key
   *  the client couldn't resolve; it still toggles + counts. Empty
   *  sets are pruned so a chip vanishes when its last reactor
   *  toggles off. */
  reactions: Map<string, Set<MemberIdentity>>;
}

/** A normalized op ready to fold into the overlay. The caller has
 *  already run the structural decode + (for edit/delete) the
 *  author gate. */
export type OverlayOp =
  | {
      op: "edit";
      target: number;
      text: string;
      /** The edit op's own DS `messageId`, when known. */
      opId?: number | undefined;
    }
  | { op: "delete"; target: number; opId?: number | undefined }
  | {
      op: "reaction";
      target: number;
      emoji: string;
      /** The reactor's signature key, resolved from its MLS leaf at
       *  receipt — see the module doc on why not the leaf. */
      reactor: MemberIdentity;
    };

/** Re-exported so the reaction call sites take the sentinel from
 *  the module that owns the op shape. Defined in
 *  `author-identity.ts` alongside the identity type it inhabits. */
export const OWN_REACTION_SENTINEL = OWN_SENTINEL;

/** The fixed reaction palette offered by the picker (kept here so
 *  the UI + any future test share one definition). */
export const REACTION_EMOJI = ["👍", "❤️", "😂", "😮", "😢", "🙏"] as const;

/**
 * Fold one (already-authorized) op into the overlay map, returning
 * a NEW map (the previous map is never mutated, so a stale signal
 * value stays valid). Pure + total.
 *
 *   - `edit`     → set `editedText`.
 *   - `delete`   → set `deleted`.
 *   - `reaction` → toggle `(reactor, emoji)`; an empty emoji set
 *     is pruned.
 */
export function applyOp(
  prev: Map<number, MessageOpState>,
  op: OverlayOp,
): Map<number, MessageOpState> {
  const next = new Map(prev);
  const cur = next.get(op.target);
  // Clone the per-message state (incl. the reactions map) so we
  // never mutate the object the previous map points at. Optional
  // fields are spread conditionally for `exactOptionalPropertyTypes`.
  const state: MessageOpState = {
    ...(cur?.editedText !== undefined ? { editedText: cur.editedText } : {}),
    ...(cur?.deleted !== undefined ? { deleted: cur.deleted } : {}),
    ...(cur?.lastOpId !== undefined ? { lastOpId: cur.lastOpId } : {}),
    reactions: cur
      ? new Map([...cur.reactions].map(([k, v]) => [k, new Set(v)]))
      : new Map(),
  };
  if (op.op === "edit") {
    state.editedText = op.text;
    if (op.opId !== undefined) state.lastOpId = op.opId;
  } else if (op.op === "delete") {
    state.deleted = true;
    if (op.opId !== undefined) state.lastOpId = op.opId;
  } else {
    const reactors =
      state.reactions.get(op.emoji) ?? new Set<MemberIdentity>();
    if (reactors.has(op.reactor)) {
      reactors.delete(op.reactor);
    } else {
      reactors.add(op.reactor);
    }
    if (reactors.size === 0) {
      state.reactions.delete(op.emoji);
    } else {
      state.reactions.set(op.emoji, reactors);
    }
  }
  next.set(op.target, state);
  return next;
}

/**
 * The author-only integrity gate for edit / delete (ADR-0023,
 * invariant #4). Returns `true` ONLY when both identities are
 * known and equal. Fail-closed: an `undefined` author (a message
 * stored before the author identity was persisted, a control
 * frame) or an `undefined` op sender is never authorized.
 *
 * The comparison is on the **signature key**, and the reason is
 * load-bearing rather than stylistic: comparing MLS leaf indices
 * here is what let a member added into a departed member's vacated
 * leaf edit and delete that member's history. See the module doc.
 *
 * Reactions DO NOT go through this gate — any member may react.
 */
export function editDeleteAuthorized(
  author: MemberIdentity | undefined,
  opSender: MemberIdentity | undefined,
): boolean {
  if (author === undefined || opSender === undefined) return false;
  return author === opSender;
}
