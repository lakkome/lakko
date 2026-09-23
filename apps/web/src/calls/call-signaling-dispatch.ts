// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Inbound call-signaling delivery for a chat — the seam between the receive
 * path (which decrypts journal rows) and the call state machines (which must
 * see each row once, in order, one at a time).
 *
 * ## Why this is a module and not an effect in `Chat.tsx`
 *
 * Signaling used to be dispatched from a `createEffect` over the chat's
 * timeline rows. That effect re-ran on every change to the rows — every poll
 * batch, every send — and walked ALL of them again, firing an unawaited async
 * handler per row. Three defects followed, all observed:
 *
 *   - a ringing callee was re-fed its own offer, read it as a renegotiation
 *     and published an Answer the user never gave; Accept then ran
 *     `createAnswer` in `stable`, threw, and ended the call as `error`;
 *   - after a call ended the offer was still in the rows, so the next poll
 *     rang again for a call that was over;
 *   - handlers for one call overlapped, so an ICE candidate could be applied
 *     against a peer connection another handler was still building.
 *
 * The fix is structural: rows are handed over by the one place that consumes
 * a poll batch exactly once, deduplicated on the DS `messageId`, and run
 * through a serial queue. Living in a `.ts` module is what makes the rules
 * testable — vitest here runs in plain node with no JSX transform.
 */

/** One decrypted signaling row, as the receive path hands it over. */
export interface InboundSignalingRow {
  /** DS journal id — per-group monotonic, the deduplication key. */
  readonly messageId: number;
  /** DS server timestamp of the row (RFC 3339). */
  readonly createdAt: string;
  /** Plaintext `CallSignalingPayload` bytes. */
  readonly payloadBytes: Uint8Array;
  /** MLS-authenticated sender leaf, when the frame named one. */
  readonly senderLeaf?: number;
  /** The sender leaf's own signature key (base64), resolved at receipt. A
   *  group Join / Leave without it is dropped. */
  readonly senderMemberKey?: string;
  /** The sender's person key (base64), resolved at receipt. */
  readonly senderPersonKey?: string;
}

/**
 * A serial queue that delivers each item at most once.
 *
 * - **At most once:** an item whose key was ever enqueued is refused, whether
 *   it is still queued, running, or long finished. A batch the poll re-feeds (a
 *   held persist re-fetches its rows) therefore changes nothing.
 * - **In order, one at a time:** the handler for an item starts only after
 *   the previous item's handler has settled.
 * - **Isolated failures:** a handler that throws is reported and the queue
 *   moves on; one malformed row must not wedge every call after it.
 *
 * ## `keyOf` is REQUIRED, and it is the second argument
 *
 * It used to be the third, and to DEFAULT to a bare `item.messageId` — which is
 * only unique within ONE group's journal. The app-level call host receives
 * signaling from every joined group, so under that default row 42 of one chat
 * would be refused as a repeat of row 42 of another and a real call frame
 * would be silently dropped. The one production caller already passed the
 * qualified pair, so the default was never load-bearing — only a trap set for
 * the next one. There is no safe default a generic queue can pick either, since
 * only the caller knows what makes its item unique, so the parameter is
 * mandatory and sits ahead of the optional `onError`, where it cannot be left
 * off by accident.
 */
export class SerialOnceQueue<T> {
  private readonly seen = new Set<string | number>();
  private tail: Promise<void> = Promise.resolve();
  private closed = false;

  constructor(
    private readonly handler: (item: T) => Promise<void>,
    /** What makes an item unique. For anything carrying a DS `messageId`, that
     *  is the `(group, messageId)` PAIR — never the id alone. */
    private readonly keyOf: (item: T) => string | number,
    private readonly onError: (err: unknown, item: T) => void = (err) => {
      // eslint-disable-next-line no-console
      console.warn("call signaling dispatch failed", err);
    },
  ) {}

  /** Queue an item. Returns `false` when it was refused (a repeat, or the
   *  queue is closed). */
  enqueue(item: T): boolean {
    const key = this.keyOf(item);
    if (this.closed || this.seen.has(key)) return false;
    this.seen.add(key);
    this.tail = this.tail.then(async () => {
      if (this.closed) return;
      try {
        await this.handler(item);
      } catch (err) {
        this.onError(err, item);
      }
    });
    return true;
  }

  /** Resolves once everything queued so far has been handled. */
  whenIdle(): Promise<void> {
    return this.tail;
  }

  /** Stop delivering. Items still queued are dropped, new ones refused. */
  close(): void {
    this.closed = true;
  }
}

/** Where a parsed signaling body goes. */
export type SignalingRoute =
  /** `groupStart` / `groupJoin` / `groupLeave` — the group-call record only. */
  | "group"
  /** `end` — terminates whichever call carries that `call_id`: a 1×1 session
   *  or (reused as the group terminal event) a group-call record. */
  | "end"
  /** `offer` / `answer` / `iceCandidate` — the 1×1 session only. */
  | "oneToOne"
  /** Unknown kind (a newer schema) — dropped. */
  | "drop";

/**
 * Route a `ParsedCallSignaling.kind`.
 *
 * C-11: a group-control body never routes to the 1×1 session. Written as an
 * allow-list of the three kinds that do, so a kind added to the wire later
 * lands in `drop` rather than in a state machine that does not know it.
 */
export function routeSignalingKind(kind: string): SignalingRoute {
  switch (kind) {
    case "groupStart":
    case "groupJoin":
    case "groupLeave":
      return "group";
    case "end":
      return "end";
    case "offer":
    case "answer":
    case "iceCandidate":
      return "oneToOne";
    default:
      return "drop";
  }
}

/*
 * The staleness rule used to live here, as `STALE_OFFER_MS = 60_000` and an
 * `isStaleOffer` helper. It is gone: "how old is too old" is one half of the
 * shared ring rule (`crates/lakko-ui-rules/src/ring.rs`, reached through
 * `calls/ring-rule.ts`), whose window is 45 s — the same number
 * `call-session.ts` arms its ring timer from. The two constants disagreed, so
 * an offer between 45 s and 60 s old rang here for a call the caller had
 * already given up on and recorded as missed.
 */

/** Lowercase hex of a byte string — the record / dedup key for a call id. */
export function callIdHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}
