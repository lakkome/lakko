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
 *      tombstone, a reaction adds or takes off `(reactor, emoji)` through
 *      the core's fold (`foldReactions`).
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
 * take off Bob's reactions. A reactor is the ACCOUNT key the leaf resolves
 * to (ADR-0050), so one person on two devices is one reactor. See `author-identity.ts` for the
 * resolution rule and the reason it must happen at receipt.
 */

import {
  foldReactions as wasmFoldReactions,
  ReactionOp as WasmReactionOp,
} from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import {
  OWN_REACTION_SENTINEL as OWN_SENTINEL,
  type MemberIdentity,
} from "./author-identity";

/** One reaction a person holds on a message after the fold: who (the
 *  account key, base64), which emoji, and the journal position (DS message
 *  id) of the add that gave it. */
export interface HeldReactionRecord {
  readonly person: MemberIdentity;
  readonly emoji: string;
  readonly at: number;
}

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
  /** What each person holds, as the Rust fold left it (UX
   *  reactions-and-emoji sections 10 and 17): at most three per person, an
   *  ignored fourth stays ignored. Ordered by `at`. The source of truth;
   *  {@link reactions} is derived from it. */
  held: readonly HeldReactionRecord[];
  /** emoji → set of reactor IDENTITIES (base64 ACCOUNT keys, ADR-0050),
   *  derived from {@link held}. Never leaf indices: a departing member's
   *  leaf is handed to the next member added. Empty sets do not occur. */
  reactions: Map<string, Set<MemberIdentity>>;
}

/** A normalized op ready to fold into the overlay. The caller has
 *  already run the structural decode + (for edit/delete) the
 *  author gate. */
export type OverlayOp =
  | { op: "edit"; target: number; text: string }
  | { op: "delete"; target: number }
  | {
      op: "reaction";
      target: number;
      emoji: string;
      /** The reactor's account key, resolved from its MLS leaf at
       *  receipt — see the module doc on why not the leaf. */
      reactor: MemberIdentity;
      /** Explicit add or take off. */
      action: "add" | "remove";
      /** The op's own journal position (its DS message id): ops are folded
       *  in this order on every member. */
      at: number;
    };

/** Re-exported so the reaction call sites take the sentinel from
 *  the module that owns the op shape. Defined in
 *  `author-identity.ts` alongside the identity type it inhabits. */
export const OWN_REACTION_SENTINEL = OWN_SENTINEL;

/** Derive the `emoji -> people` view of what is held, in `at` order. */
export function reactionsFromHeld(
  held: readonly HeldReactionRecord[],
): Map<string, Set<MemberIdentity>> {
  const out = new Map<string, Set<MemberIdentity>>();
  for (const h of held) {
    const set = out.get(h.emoji) ?? new Set<MemberIdentity>();
    set.add(h.person);
    out.set(h.emoji, set);
  }
  return out;
}

/** The emoji `person` holds on a message. */
export function heldBy(
  state: MessageOpState | undefined,
  person: MemberIdentity | null | undefined,
): string[] {
  if (state === undefined || person === null || person === undefined) return [];
  return state.held.filter((h) => h.person === person).map((h) => h.emoji);
}

/** Decode a reactor identity to the 32-byte key the fold keys on, or
 *  `null` (an undecodable identity takes no part in the fold). */
function personBytes(person: MemberIdentity): Uint8Array | null {
  try {
    const b = base64Decode(person);
    return b.length === 32 ? b : null;
  } catch {
    return null;
  }
}

/**
 * Apply one reaction op on top of what is held, through the core's fold
 * (`foldReactions`, the one rule every member computes alike). What is held
 * re-enters the fold as the adds that gave it, at their own positions, then
 * the new op: the same result as folding the whole history, since an ignored
 * add never re-enters.
 */
function foldHeld(
  held: readonly HeldReactionRecord[],
  op: Extract<OverlayOp, { op: "reaction" }>,
): HeldReactionRecord[] {
  // The inputs cross by value (the core takes them), so they are not freed
  // here; the glue's finalizers release whatever is left.
  const inputs: WasmReactionOp[] = [];
  for (const h of held) {
    const p = personBytes(h.person);
    if (p === null) continue;
    inputs.push(new WasmReactionOp(p, h.emoji, "add", BigInt(Math.max(0, h.at))));
  }
  const p = personBytes(op.reactor);
  if (p === null) return [...held];
  inputs.push(new WasmReactionOp(p, op.emoji, op.action, BigInt(Math.max(0, op.at))));
  return wasmFoldReactions(inputs).map((r) => {
    const rec: HeldReactionRecord = {
      person: base64Encode(r.person),
      emoji: r.emoji,
      at: Number(r.at),
    };
    r.free();
    return rec;
  });
}

/**
 * Fold one (already-authorized) op into the overlay map, returning
 * a NEW map (the previous map is never mutated, so a stale signal
 * value stays valid). Pure + total apart from the core fold.
 *
 *   - `edit`     → set `editedText`.
 *   - `delete`   → set `deleted`.
 *   - `reaction` → add or take off `(reactor, emoji)` through the core's
 *     fold: at most three per person, an add of a held emoji and a remove of
 *     one not held are no-ops.
 */
export function applyOp(
  prev: Map<number, MessageOpState>,
  op: OverlayOp,
): Map<number, MessageOpState> {
  const next = new Map(prev);
  const cur = next.get(op.target);
  const state: MessageOpState = {
    ...(cur?.editedText !== undefined ? { editedText: cur.editedText } : {}),
    ...(cur?.deleted !== undefined ? { deleted: cur.deleted } : {}),
    held: cur?.held ?? [],
    reactions: cur ? new Map([...cur.reactions].map(([k, v]) => [k, new Set(v)])) : new Map(),
  };
  if (op.op === "edit") {
    state.editedText = op.text;
  } else if (op.op === "delete") {
    state.deleted = true;
  } else {
    const held = foldHeld(state.held, op);
    state.held = held;
    state.reactions = reactionsFromHeld(held);
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
