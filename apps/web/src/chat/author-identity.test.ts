// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The leaf-reuse impersonation, and the two sibling misattributions
 * of the same shape.
 *
 * Every test here drives the client's real receive pipeline in
 * miniature: resolve the sender's leaf to a signature key AT RECEIPT
 * (`resolveMemberIdentity` against the roster of the epoch the
 * message decrypted at), carry the key forward, and let the rule
 * decide on the key. What makes the pipeline the thing under test
 * rather than the individual rules is the membership change in the
 * middle: a `remove Bob, add Dave` commit — which ADR-0045 composes
 * into ONE commit, so the whole sequence can land in a single poll
 * batch — hands Dave the leaf index Bob vacated.
 *
 * Before this, `senderLeaf` was carried and compared raw, so every
 * one of these read Dave as Bob.
 */

import { describe, it, expect } from "vitest";
import {
  authorIdentityToPersisted,
  OWN_REACTION_SENTINEL,
  persistedAuthorIdentity,
  resolveMemberIdentity,
  type MemberIdentity,
  type RosterMember,
} from "./author-identity";
import { applyOp, editDeleteAuthorized, type MessageOpState } from "./message-ops";
import {
  foldReadWatermark,
  foldTyping,
  activeTypers,
  isReadByEveryOther,
} from "./presence-ops";

// ---------------------------------------------------------------
// The roster, before and after the remove+add commit
// ---------------------------------------------------------------

/** Distinct 32-byte signature keys, byte-filled so each is obvious
 *  in a failure message. */
const key = (fill: number): Uint8Array => new Uint8Array(32).fill(fill);

const ALICE_KEY = key(0xa1);
const BOB_KEY = key(0xb0);
const DAVE_KEY = key(0xda);

/** Epoch N: Alice at leaf 0, Bob at leaf 2. */
const ROSTER_BEFORE: RosterMember[] = [
  { leafIndex: 0, signaturePublicKey: ALICE_KEY },
  { leafIndex: 2, signaturePublicKey: BOB_KEY },
];

/** Epoch N+1, after one commit removing Bob and adding Dave.
 *  OpenMLS hands the vacated leaf to the next Add, so Dave is at
 *  leaf 2 — the index Bob held. This is the whole hazard. */
const ROSTER_AFTER: RosterMember[] = [
  { leafIndex: 0, signaturePublicKey: ALICE_KEY },
  { leafIndex: 2, signaturePublicKey: DAVE_KEY },
];

/** The leaf Bob held and Dave inherited. */
const REUSED_LEAF = 2;

const ALICE = resolveMemberIdentity(ROSTER_BEFORE, 0) as MemberIdentity;
const BOB = resolveMemberIdentity(ROSTER_BEFORE, REUSED_LEAF) as MemberIdentity;
const DAVE = resolveMemberIdentity(ROSTER_AFTER, REUSED_LEAF) as MemberIdentity;

// ---------------------------------------------------------------
// resolveMemberIdentity — the resolution rule itself
// ---------------------------------------------------------------

describe("resolveMemberIdentity", () => {
  it("resolves a leaf to the member's signature key", () => {
    expect(resolveMemberIdentity(ROSTER_BEFORE, 0)).toBe(ALICE);
  });

  it("resolves ONE leaf to DIFFERENT members across the commit", () => {
    // The premise of every test below, stated once: the same leaf
    // index names two different people either side of the commit.
    expect(BOB).not.toBe(DAVE);
    expect(resolveMemberIdentity(ROSTER_BEFORE, REUSED_LEAF)).toBe(BOB);
    expect(resolveMemberIdentity(ROSTER_AFTER, REUSED_LEAF)).toBe(DAVE);
  });

  it("is undefined for a leaf no member holds (fail closed)", () => {
    expect(resolveMemberIdentity(ROSTER_BEFORE, 7)).toBeUndefined();
  });

  it("is undefined against an unreadable roster (fail closed)", () => {
    // What `rosterOf` yields when `group.members()` throws.
    expect(resolveMemberIdentity([], REUSED_LEAF)).toBeUndefined();
  });

  it("treats leaf 0 as a real leaf, not a falsy miss", () => {
    expect(resolveMemberIdentity(ROSTER_BEFORE, 0)).toBeDefined();
  });
});

// ---------------------------------------------------------------
// The headline: edit / delete impersonation
// ---------------------------------------------------------------

describe("ADR-0023 author gate — leaf reuse impersonation", () => {
  /** Bob authors a message at epoch N. The receive loop resolves his
   *  leaf THERE, against the roster that names him. */
  const bobsMessageAuthor = resolveMemberIdentity(ROSTER_BEFORE, REUSED_LEAF);

  it("REFUSES a newcomer at a vacated leaf EDITING the departed member's message", () => {
    // One commit later Dave holds leaf 2 and sends an edit op. His
    // op's senderLeaf is 2 — indistinguishable from Bob's, which is
    // precisely why the leaf cannot be the thing compared.
    const opSender = resolveMemberIdentity(ROSTER_AFTER, REUSED_LEAF);
    expect(opSender).toBe(DAVE);
    expect(editDeleteAuthorized(bobsMessageAuthor, opSender)).toBe(false);
  });

  it("REFUSES a newcomer at a vacated leaf DELETING the departed member's message", () => {
    // Delete runs the identical gate — pinned separately so a change
    // that only re-secures edit cannot pass.
    const opSender = resolveMemberIdentity(ROSTER_AFTER, REUSED_LEAF);
    expect(editDeleteAuthorized(bobsMessageAuthor, opSender)).toBe(false);
  });

  it("still lets the real author edit their own message", () => {
    // The refusal must not be "refuse everything": Bob editing at
    // epoch N resolves to the same identity he authored under.
    const opSender = resolveMemberIdentity(ROSTER_BEFORE, REUSED_LEAF);
    expect(editDeleteAuthorized(bobsMessageAuthor, opSender)).toBe(true);
  });

  it("still refuses a plain non-author who never held the leaf", () => {
    const opSender = resolveMemberIdentity(ROSTER_BEFORE, 0);
    expect(editDeleteAuthorized(bobsMessageAuthor, opSender)).toBe(false);
  });

  it("fails closed on an unresolvable author (a pre-v2 stored row)", () => {
    expect(editDeleteAuthorized(undefined, DAVE)).toBe(false);
  });

  it("would be AUTHORIZED if the author were resolved late — which is why it is not", () => {
    // The wrong shape, kept as a permanent contrast: store the
    // author's LEAF and resolve it when the op arrives, against
    // whatever roster is current then. Both sides then resolve to
    // Dave and the gate says yes. The rule that stops it is not the
    // key comparison on its own — it is resolving the author's leaf
    // at RECEIPT and carrying the key. Delete either half and the
    // impersonation is back.
    const resolvedLate = resolveMemberIdentity(ROSTER_AFTER, REUSED_LEAF);
    const opSender = resolveMemberIdentity(ROSTER_AFTER, REUSED_LEAF);
    expect(editDeleteAuthorized(resolvedLate, opSender)).toBe(true);
    // ...whereas the identity resolved at receipt refuses the same op.
    expect(editDeleteAuthorized(bobsMessageAuthor, opSender)).toBe(false);
  });

  it("fails closed on an unresolvable op sender (torn roster)", () => {
    expect(editDeleteAuthorized(BOB, resolveMemberIdentity([], REUSED_LEAF))).toBe(
      false,
    );
  });
});

// ---------------------------------------------------------------
// Sibling 1 — reactions
// ---------------------------------------------------------------

describe("ADR-0023 reactions — leaf reuse", () => {
  const react = (
    prev: Map<number, MessageOpState>,
    reactor: MemberIdentity,
  ): Map<number, MessageOpState> =>
    applyOp(prev, { op: "reaction", target: 7, emoji: "👍", reactor });

  it("a newcomer at a vacated leaf cannot toggle OFF the departed member's reaction", () => {
    const bob = resolveMemberIdentity(ROSTER_BEFORE, REUSED_LEAF) as MemberIdentity;
    const dave = resolveMemberIdentity(ROSTER_AFTER, REUSED_LEAF) as MemberIdentity;
    let m = react(new Map(), bob);
    m = react(m, dave);
    // Two distinct people reacted: Bob's survives AND Dave's adds.
    expect(m.get(7)?.reactions.get("👍")).toEqual(new Set([bob, dave]));
  });

  it("a newcomer's reaction does not MERGE into the departed member's count", () => {
    let m = react(new Map(), BOB);
    m = react(m, DAVE);
    expect(m.get(7)?.reactions.get("👍")?.size).toBe(2);
  });

  it("the real reactor can still toggle their OWN reaction off", () => {
    let m = react(new Map(), BOB);
    m = react(m, BOB);
    expect(m.get(7)?.reactions.has("👍")).toBe(false);
  });

  it("does not render a stranger's reaction as the local user's", () => {
    // The chip's "mine" test compares our identity against the
    // stored reactors. Under a leaf comparison, a newcomer taking
    // OUR vacated leaf would light the highlight on their reaction.
    const m = react(new Map(), DAVE);
    const reactors = m.get(7)?.reactions.get("👍");
    expect(reactors?.has(BOB)).toBe(false);
    expect(reactors?.has(ALICE)).toBe(false);
    expect(reactors?.has(DAVE)).toBe(true);
  });

  it("the own-reaction sentinel can never collide with a real identity", () => {
    // A real identity is base64; `?` is outside that alphabet. This
    // is the property the old numeric `-1` had against a real leaf.
    for (const id of [ALICE, BOB, DAVE]) {
      expect(id).not.toBe(OWN_REACTION_SENTINEL);
    }
    expect(OWN_REACTION_SENTINEL).toContain("?");
  });
});

// ---------------------------------------------------------------
// Sibling 2 — read-receipt watermarks
// ---------------------------------------------------------------

describe("ADR-0024 read watermarks — leaf reuse", () => {
  it("a newcomer at a vacated leaf does NOT inherit the departed member's watermark", () => {
    // Bob read up to message 50, then was removed.
    const bob = resolveMemberIdentity(ROSTER_BEFORE, REUSED_LEAF) as MemberIdentity;
    const m = foldReadWatermark(new Map(), bob, 50);
    // Dave arrives at Bob's leaf having read nothing.
    const dave = resolveMemberIdentity(ROSTER_AFTER, REUSED_LEAF) as MemberIdentity;
    expect(m.get(dave)).toBeUndefined();
  });

  it("does not report a message as read by someone who never saw it", () => {
    // Alice's own view: Bob (departed) read to 50; Dave (newcomer)
    // has reported nothing. Message 50 must NOT show as Read.
    let m = foldReadWatermark(new Map(), BOB, 50);
    // Dave joins and reports having read only up to 10.
    m = foldReadWatermark(m, DAVE, 10);
    expect(isReadByEveryOther(m, 50, ALICE)).toBe(false);
    // It flips only once Dave genuinely catches up.
    m = foldReadWatermark(m, DAVE, 50);
    expect(isReadByEveryOther(m, 50, ALICE)).toBe(true);
  });

  it("stays monotone per member across the membership change", () => {
    let m = foldReadWatermark(new Map(), BOB, 50);
    // Dave's first receipt is lower than Bob's — under a leaf key it
    // would be swallowed as a non-advance and Dave would keep Bob's
    // 50. Under an identity key it is Dave's own first watermark.
    m = foldReadWatermark(m, DAVE, 10);
    expect(m.get(BOB)).toBe(50);
    expect(m.get(DAVE)).toBe(10);
  });
});

// ---------------------------------------------------------------
// Sibling 3 — typing
// ---------------------------------------------------------------

describe("ADR-0024 typing — leaf reuse", () => {
  it("a newcomer at a vacated leaf does not inherit the departed member's typing state", () => {
    // Same shape as the watermark, but self-healing in ~6 s, so the
    // window is narrow rather than permanent. Keyed on the identity
    // anyway: the misattribution is real while it lasts, and one
    // rule is easier to keep right than two.
    const t0 = 1_000_000;
    const m = foldTyping(new Map(), BOB, true, t0, t0);
    expect(activeTypers(m, t0 + 1)).toEqual([BOB]);
    expect(m.has(DAVE)).toBe(false);
  });

  it("a newcomer's stop does not clear the departed member's entry", () => {
    const t0 = 1_000_000;
    let m = foldTyping(new Map(), BOB, true, t0, t0);
    m = foldTyping(m, DAVE, false, t0, t0);
    expect(m.has(BOB)).toBe(true);
  });
});

// ---------------------------------------------------------------
// Persistence — the author identity round-trips; a pre-v2 row does not
// ---------------------------------------------------------------

describe("persisted author identity", () => {
  it("round-trips a resolved identity, so edit/delete survives a reload", () => {
    const stored = authorIdentityToPersisted(BOB);
    expect(persistedAuthorIdentity(stored)).toBe(BOB);
    // Which is the point of persisting it at all: before this, every
    // reloaded row had an unknown author and the gate refused Bob's
    // own edit of his own message.
    expect(editDeleteAuthorized(persistedAuthorIdentity(stored), BOB)).toBe(true);
  });

  it("a PRE-EXISTING row (field absent) fails the gate CLOSED", () => {
    // What `JSON.parse` yields for a v1 row: the property is simply
    // not there. Not a migration, not a wipe — it reads as an
    // unknown author, exactly as every reloaded row did before.
    const v1Row: { authorKeyB64?: string | null } = {};
    expect(persistedAuthorIdentity(v1Row.authorKeyB64)).toBeUndefined();
    expect(editDeleteAuthorized(persistedAuthorIdentity(v1Row.authorKeyB64), BOB)).toBe(
      false,
    );
  });

  it("a pre-existing row is never MISATTRIBUTED to the reader", () => {
    // The tempting shortcut for an outgoing row — "it is ours, so
    // assume our current key" — would invent an author the store
    // never recorded. Unknown must stay unknown.
    expect(persistedAuthorIdentity(undefined)).not.toBe(ALICE);
    expect(persistedAuthorIdentity(undefined)).toBeUndefined();
  });

  it("an explicitly unresolved identity stores as null, not as a key", () => {
    expect(authorIdentityToPersisted(undefined)).toBe(null);
    expect(persistedAuthorIdentity(null)).toBeUndefined();
  });

  it("never persists a leaf index", () => {
    // A leaf would round-trip cleanly and then name whoever inherited
    // it. The stored value is always a key or null.
    const stored = authorIdentityToPersisted(BOB);
    expect(stored).toBe(BOB);
    expect(typeof stored).toBe("string");
  });
});
