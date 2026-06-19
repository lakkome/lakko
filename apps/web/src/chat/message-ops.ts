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
 *      tombstone, reaction toggles `(reactorLeaf, emoji)`.
 *
 *   2. [`editDeleteAuthorized`] — the author-only integrity gate
 *      (ADR-0023, invariant #4): an edit / delete is applied ONLY
 *      if the op's sender leaf equals the target message's
 *      original author leaf. Reactions are accepted from any
 *      member (and are not routed through this gate).
 *
 * Both are total + pure — no throw, no I/O — so a malformed op the
 * receive loop already structurally validated can never wedge the
 * timeline.
 */

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
  /** emoji → set of reactor leaf indices. A sentinel
   *  ([`OWN_REACTION_SENTINEL`]) records an own reaction whose
   *  leaf the FFI couldn't surface; it still toggles + counts.
   *  Empty sets are pruned so a chip vanishes when its last
   *  reactor toggles off. */
  reactions: Map<string, Set<number>>;
}

/** A normalized op ready to fold into the overlay. The caller has
 *  already run the structural decode + (for edit/delete) the
 *  author gate. */
export type OverlayOp =
  | { op: "edit"; target: number; text: string }
  | { op: "delete"; target: number }
  | { op: "reaction"; target: number; emoji: string; reactorLeaf: number };

/** Sentinel reactor-leaf for an own reaction whose MLS leaf the
 *  FFI couldn't surface (mirrors the `senderLeaf === undefined`
 *  case). Never collides with a real leaf (which is `>= 0`). */
export const OWN_REACTION_SENTINEL = -1;

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
 *   - `reaction` → toggle `(reactorLeaf, emoji)`; an empty emoji
 *     set is pruned.
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
    reactions: cur
      ? new Map([...cur.reactions].map(([k, v]) => [k, new Set(v)]))
      : new Map(),
  };
  if (op.op === "edit") {
    state.editedText = op.text;
  } else if (op.op === "delete") {
    state.deleted = true;
  } else {
    const reactors = state.reactions.get(op.emoji) ?? new Set<number>();
    if (reactors.has(op.reactorLeaf)) {
      reactors.delete(op.reactorLeaf);
    } else {
      reactors.add(op.reactorLeaf);
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
 * invariant #4). Returns `true` ONLY when both leaves are known
 * and equal. Fail-closed: an `undefined` author (a reloaded
 * message, a control frame) or an `undefined` op sender leaf is
 * never authorized.
 *
 * Reactions DO NOT go through this gate — any member may react.
 */
export function editDeleteAuthorized(
  authorLeaf: number | undefined,
  opSenderLeaf: number | undefined,
): boolean {
  if (authorLeaf === undefined || opSenderLeaf === undefined) return false;
  return authorLeaf === opSenderLeaf;
}
