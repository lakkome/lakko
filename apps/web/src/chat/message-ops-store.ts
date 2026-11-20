// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0023 — carrying the edit / delete / reaction overlay to disk.
 *
 * `message-ops.ts` owns the overlay: a session-scoped map from a DS
 * `messageId` to the mutation applied to that message. Owning it was
 * the whole of it — the overlay lived in a `Chat.tsx` signal and
 * reached no store, so every op was undone by a reload. Not delayed:
 * **undone**. The journal cursor has long since passed the op rows,
 * and MLS forward secrecy means a consumed row can never be
 * decrypted again, so a deleted message came back and stayed back.
 *
 * On a messenger whose premise is that a person controls their own
 * messages, a delete that silently reverts is the worst shape the
 * defect could take, so this module is small on purpose: three total,
 * pure functions and no I/O, testable without the SolidJS or wasm
 * surface (nothing in `routes/Chat.tsx` is reachable from vitest).
 *
 * ## Three doors, one rule
 *
 * An op reaches the log three ways, and each used to lose it:
 *
 *   1. the open chat persists its rows on a debounce
 *      ([`persistedWithOps`]);
 *   2. a reload reads them back and must rebuild the overlay, or the
 *      next persist writes the un-edited row straight back over the
 *      good one ([`opsFromPersisted`]);
 *   3. the background receive files rows for a chat that is not open.
 *      Its ops are buffered in memory for a view that may never
 *      attach, so a reload loses them outright ([`foldOpsIntoStored`]).
 *
 * All three go through `applyOp` and `editDeleteAuthorized` from
 * `message-ops.ts` rather than restating either. The author-only gate
 * in particular must not be reimplemented here: it is what stops one
 * member rewriting another's history, and a second copy of a security
 * rule is a second thing to get wrong.
 */

import {
  applyOp,
  editDeleteAuthorized,
  reactionsFromHeld,
  type HeldReactionRecord,
  type MessageOpState,
  type OverlayOp,
} from "./message-ops";
import { persistedAuthorIdentity, type MemberIdentity } from "./author-identity";
import type { PersistedMessage, PersistedReaction } from "./message-log-store";

/**
 * Fold one message's overlay state into the row about to be written.
 *
 * Returns the input unchanged (by identity) when there is nothing to
 * fold, so an untouched transcript re-persists as the same objects.
 *
 * A deleted row is written EMPTY — no text, no attachments. See
 * [`PersistedMessage.deleted`] for why, and why it differs from the
 * natives. Delete wins over edit: a message edited and then deleted
 * keeps neither the original nor the edit.
 */
export function persistedWithOps(
  m: PersistedMessage,
  state: MessageOpState | undefined,
): PersistedMessage {
  if (state === undefined) return m;
  const reactions = reactionsToPersisted(state.held);
  const opId = state.lastOpId === undefined ? {} : { lastOpId: String(state.lastOpId) };
  if (state.deleted === true) {
    // ADR-0098 section 1 - the evidence holds the message's plaintext, so a
    // deleted row keeps none of it (the reason the text is emptied). The
    // invitation card goes with the text too: it holds the link's secret.
    const { evidence: _evidence, channelInviteB64: _card, ...rest } = m;
    return {
      ...rest,
      text: "",
      attachments: [],
      deleted: true,
      ...opId,
      ...(reactions.length === 0 ? {} : { reactions }),
    };
  }
  const edited = state.editedText !== undefined;
  if (!edited && reactions.length === 0) return m;
  return {
    ...m,
    ...(edited ? { text: state.editedText as string, edited: true } : {}),
    ...opId,
    ...(reactions.length === 0 ? {} : { reactions }),
  };
}

/**
 * Rebuild the overlay from rows read back off disk.
 *
 * `editedText` is set to the row's stored text, which is already the
 * edited text. That looks redundant and is not: the bubble renders
 * `editedText ?? text` — the same words either way — but the
 * "(edited)" marker is driven by the overlay, and a later persist
 * reads the overlay rather than the row, so an overlay that forgot
 * the edit would write the flag straight back off.
 *
 * A row with none of the three fields contributes no entry, so a
 * transcript that was never touched rebuilds an empty map.
 */
export function opsFromPersisted(
  messages: readonly PersistedMessage[],
): Map<number, MessageOpState> {
  const out = new Map<number, MessageOpState>();
  for (const m of messages) {
    const id = Number(m.messageId);
    if (!Number.isFinite(id)) continue;
    const held = heldFromPersisted(m.reactions);
    const deleted = m.deleted === true;
    const edited = m.edited === true;
    if (!deleted && !edited && held.length === 0) continue;
    const opId = m.lastOpId === undefined ? Number.NaN : Number(m.lastOpId);
    out.set(id, {
      ...(edited ? { editedText: m.text } : {}),
      ...(deleted ? { deleted: true } : {}),
      ...(Number.isFinite(opId) ? { lastOpId: opId } : {}),
      held,
      reactions: reactionsFromHeld(held),
    });
  }
  return out;
}

/** One op as the receive path hands it over, before any gate. */
export interface IncomingOp {
  readonly op: "edit" | "delete" | "reaction";
  /** The DS `messageId` this op acts on. */
  readonly target: number;
  /** The new text, for `edit`. */
  readonly text: string;
  /** The emoji, for `reaction`. */
  readonly emoji: string | undefined;
  /** Add or take off, for `reaction` (absent reads as add). */
  readonly reactionAction?: "add" | "remove";
  /** This op's own DS message id: the fold's journal position. */
  readonly messageId?: number;
  /**
   * The sender's signature key, resolved from its MLS leaf at
   * receipt. `undefined` — unattributable — is dropped.
   */
  readonly senderKey: MemberIdentity | undefined;
}

/**
 * Apply ops to stored rows: the background receive's door.
 *
 * The open chat folds an op into its live overlay and persists that;
 * a chat that is not open has no overlay to fold into, so the ops
 * are applied against the stored rows directly. Same rules, because
 * it is the same two functions underneath — including the author-only
 * gate, which here reads the target's persisted author identity. A
 * row stored before that identity was persisted reports `undefined`
 * and fails the gate closed, which is the behaviour every such row
 * already had.
 *
 * Ops must arrive in journal order; a reaction toggle is order-
 * dependent by nature, and a delete after an edit must stay deleted.
 *
 * An op whose target is not among `messages` is dropped, matching the
 * open chat: nothing is materialised for a message never displayed.
 * Returns the input array unchanged (by identity) when no op applied.
 */
export function foldOpsIntoStored(
  messages: readonly PersistedMessage[],
  ops: readonly IncomingOp[],
): readonly PersistedMessage[] {
  if (ops.length === 0) return messages;
  const byId = new Map<number, PersistedMessage>();
  for (const m of messages) {
    const id = Number(m.messageId);
    if (Number.isFinite(id)) byId.set(id, m);
  }
  let overlay = opsFromPersisted(messages);
  let changed = false;
  for (const op of ops) {
    const target = byId.get(op.target);
    if (target === undefined) continue;
    if (op.senderKey === undefined) continue;
    const folded = foldOne(overlay, op, target);
    if (folded !== overlay) {
      overlay = folded;
      changed = true;
    }
  }
  if (!changed) return messages;
  return messages.map((m) => persistedWithOps(m, overlay.get(Number(m.messageId))));
}

/** One gated op against one known target. `===` the input when dropped. */
function foldOne(
  overlay: Map<number, MessageOpState>,
  op: IncomingOp,
  target: PersistedMessage,
): Map<number, MessageOpState> {
  const sender = op.senderKey;
  if (sender === undefined) return overlay;
  if (op.op === "reaction") {
    const emoji = op.emoji;
    // An empty emoji is what the decoder produces for a reaction it
    // could not read; reacting with nothing is not a reaction.
    if (emoji === undefined || emoji === "") return overlay;
    return applyOp(overlay, {
      op: "reaction",
      target: op.target,
      emoji,
      reactor: sender,
      action: op.reactionAction ?? "add",
      at: op.messageId ?? 0,
    });
  }
  // edit / delete are the author's alone (invariant #4, fail-closed).
  if (!editDeleteAuthorized(persistedAuthorIdentity(target.authorKeyB64), sender)) {
    return overlay;
  }
  const next: OverlayOp =
    op.op === "edit"
      ? { op: "edit", target: op.target, text: op.text, opId: op.messageId }
      : { op: "delete", target: op.target, opId: op.messageId };
  return applyOp(overlay, next);
}

/**
 * What is held as the stored flat list, with each add's journal position.
 *
 * Sorted by `(emoji, reactor)` so re-persisting unchanged state
 * produces identical bytes: the blob is re-encrypted on every write.
 */
function reactionsToPersisted(held: readonly HeldReactionRecord[]): readonly PersistedReaction[] {
  const out: PersistedReaction[] = held.map((h) => ({
    reactorKeyB64: h.person,
    emoji: h.emoji,
    at: String(h.at),
  }));
  out.sort((a, b) =>
    a.emoji === b.emoji
      ? compareStrings(a.reactorKeyB64, b.reactorKeyB64)
      : compareStrings(a.emoji, b.emoji),
  );
  return out;
}

/** Inverse of [`reactionsToPersisted`], ordered by position. An empty emoji
 *  is dropped: a chip that renders no glyph would be one nothing removes. */
function heldFromPersisted(
  stored: readonly PersistedReaction[] | undefined,
): HeldReactionRecord[] {
  const out: HeldReactionRecord[] = [];
  for (const r of stored ?? []) {
    if (r.emoji === "") continue;
    const at = Number(r.at ?? "0");
    out.push({ person: r.reactorKeyB64, emoji: r.emoji, at: Number.isFinite(at) ? at : 0 });
  }
  out.sort((a, b) => a.at - b.at);
  return out;
}

/** Ordering that does not depend on the runtime's locale. */
function compareStrings(a: string, b: string): number {
  if (a === b) return 0;
  return a < b ? -1 : 1;
}
