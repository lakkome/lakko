// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  type LinkFounded,
  MAX_CLAIMS_PER_REQUEST,
  MAX_JOINED_WELCOMES,
  holdsOneToOneWith,
  linkBuildStep,
  welcomeOrder,
  MAX_SEEN_LINK_CLAIMS,
  claimsToDecide,
  clearLinkClaims,
  linkClaimRecord,
  linkRequestKeyHex,
  linkRequestStep,
  shownCardAnswered,
} from "./link-claims";

function memoryStorage(): Pick<Storage, "getItem" | "setItem" | "removeItem"> {
  const m = new Map<string, string>();
  return {
    getItem: (k) => m.get(k) ?? null,
    setItem: (k, v) => {
      m.set(k, v);
    },
    removeItem: (k) => {
      m.delete(k);
    },
  };
}

const B = "bb".repeat(32);
const posted = { wires: [], ids: [9], won: null, settled: false, founded: null };
const posting = { wires: ["aa"], ids: [], won: null, settled: false, founded: null };
const lost = { wires: [], ids: [9], won: null, settled: true, founded: null };

describe("linkRequestStep (ADR-0079, reworked 2026-10-17)", () => {
  it("waits, with no timer, while the synced state is not loaded", () => {
    expect(linkRequestStep({ own: null, answeredElsewhere: false, verdict: null })).toBe("wait");
  });

  it("follows the register once it is loaded", () => {
    expect(linkRequestStep({ own: null, answeredElsewhere: false, verdict: "accept" })).toBe("post-claim");
    expect(linkRequestStep({ own: null, answeredElsewhere: false, verdict: "answered" })).toBe("drop");
    expect(linkRequestStep({ own: null, answeredElsewhere: false, verdict: "dead" })).toBe("card-dead");
  });

  it("waits, unshown, on a roster device's claim until the register says answered", () => {
    expect(linkRequestStep({ own: null, answeredElsewhere: true, verdict: "accept" })).toBe("wait");
    expect(linkRequestStep({ own: null, answeredElsewhere: true, verdict: null })).toBe("wait");
    expect(linkRequestStep({ own: null, answeredElsewhere: true, verdict: "answered" })).toBe("drop");
    expect(linkRequestStep({ own: null, answeredElsewhere: true, verdict: "dead" })).toBe("drop");
    const none = { wires: [], ids: [], won: null, settled: false, founded: null };
    expect(linkRequestStep({ own: none, answeredElsewhere: true, verdict: "accept" })).toBe("wait");
  });

  it("decides by the journal once this device has posted or is posting", () => {
    expect(linkRequestStep({ own: posted, answeredElsewhere: true, verdict: "accept" })).toBe("decide");
    expect(linkRequestStep({ own: posting, answeredElsewhere: true, verdict: null })).toBe("decide");
  });

  it("re-checks a lost claim against the roster until the request is answered", () => {
    expect(linkRequestStep({ own: lost, answeredElsewhere: true, verdict: "accept" })).toBe("recheck");
    expect(linkRequestStep({ own: lost, answeredElsewhere: false, verdict: null })).toBe("recheck");
    expect(linkRequestStep({ own: lost, answeredElsewhere: true, verdict: "answered" })).toBe("drop");
    expect(linkRequestStep({ own: lost, answeredElsewhere: true, verdict: "dead" })).toBe("drop");
  });

  it("builds a won claim even after the link was used up or answered meanwhile", () => {
    const won = { wires: [], ids: [9], won: 9, settled: false, founded: null };
    expect(linkRequestStep({ own: won, answeredElsewhere: true, verdict: "dead" })).toBe("build");
    expect(linkRequestStep({ own: won, answeredElsewhere: true, verdict: "answered" })).toBe("build");
  });

  it("takes down a dead-link card a roster device claimed, never one this device claimed", () => {
    expect(shownCardAnswered(null, true)).toBe(true);
    expect(shownCardAnswered(null, false)).toBe(false);
    expect(shownCardAnswered(posted, true)).toBe(false);
  });

  it("hands the core this device's claims under its own key, then the others", () => {
    expect(claimsToDecide(posted, [{ id: 4, author: B }], "aa".repeat(32))).toEqual([
      { id: 9, author: "aa".repeat(32) },
      { id: 4, author: B },
    ]);
    expect(claimsToDecide(null, [], "aa")).toEqual([]);
  });
});

describe("the build of a won request (ADR-0079 ordering, 2026-10-17)", () => {
  it("writes this account's own mailbox first and the requester's after", () => {
    const targets = [
      { accountKeyB64: "requester", isRecipient: true },
      { accountKeyB64: "own", isRecipient: false },
    ];
    expect(welcomeOrder(targets).map((t) => t.accountKeyB64)).toEqual(["own", "requester"]);
    expect(welcomeOrder([{ accountKeyB64: "requester", isRecipient: true }])).toHaveLength(1);
  });

  it("a takeover device that already holds a one-to-one with the requester does not build", () => {
    const requester = "ab".repeat(32);
    const held = [
      { oneToOne: false, memberAccountKeysHex: [requester, "cd".repeat(32)] },
      { oneToOne: true, memberAccountKeysHex: ["ef".repeat(32), requester.toUpperCase()] },
    ];
    expect(holdsOneToOneWith(held, requester)).toBe(true);
    expect(linkBuildStep({ hasFounded: false, mailboxCaughtUp: true, holdsOneToOne: true })).toBe("answered");
    // A group (not a one-to-one) with the requester does not answer it.
    expect(holdsOneToOneWith([held[0]!], requester)).toBe(false);
    expect(linkBuildStep({ hasFounded: false, mailboxCaughtUp: true, holdsOneToOne: false })).toBe("found");
  });

  it("a takeover forwards the Welcome it joined from, kept per one-to-one and bounded", () => {
    const r = linkClaimRecord("acct", memoryStorage());
    expect(r.joinedWelcome("g")).toBeNull();
    r.recordJoinedWelcome("g", "w", 1);
    expect(r.joinedWelcome("g")).toBe("w");
    for (let i = 0; i <= MAX_JOINED_WELCOMES; i += 1) r.recordJoinedWelcome(`g${i}`, `w${i}`, i + 2);
    expect(r.joinedWelcome("g")).toBeNull();
    expect(r.joinedWelcome(`g${MAX_JOINED_WELCOMES}`)).toBe(`w${MAX_JOINED_WELCOMES}`);
  });

  it("never builds blind: an unread mailbox keeps the request for the next pass", () => {
    expect(linkBuildStep({ hasFounded: false, mailboxCaughtUp: false, holdsOneToOne: false })).toBe("wait");
    expect(linkBuildStep({ hasFounded: false, mailboxCaughtUp: false, holdsOneToOne: true })).toBe("wait");
  });

  it("a founding already recorded is resumed, never founded again", () => {
    expect(linkBuildStep({ hasFounded: true, mailboxCaughtUp: false, holdsOneToOne: true })).toBe("resume");
    const storage = memoryStorage();
    const key = "k";
    const founded: LinkFounded = {
      groupIdB64: "g",
      welcomeB64: "w",
      targets: [
        { accountKeyB64: "own", isRecipient: false },
        { accountKeyB64: "requester", isRecipient: true },
      ],
      delivered: false,
    };
    linkClaimRecord("acct", storage).setWon(key, 3);
    linkClaimRecord("acct", storage).setFounded(key, founded);
    expect(linkClaimRecord("acct", storage).own(key)?.founded).toEqual(founded);
    linkClaimRecord("acct", storage).setFounded(key, { ...founded, welcomeB64: "", delivered: true });
    expect(linkClaimRecord("acct", storage).own(key)?.founded?.delivered).toBe(true);
  });
});

describe("linkClaimRecord", () => {
  const key = linkRequestKeyHex(new Uint8Array(16).fill(1), new Uint8Array(32).fill(2));

  it("names a request by link id then requester key, in hex", () => {
    expect(key).toBe("01".repeat(16) + "02".repeat(32));
  });

  it("keeps every claim another device posted, with its author, lowest first", () => {
    const r = linkClaimRecord("acct", memoryStorage());
    expect(r.seen(key)).toEqual([]);
    expect(r.recordSeen(key, 12, B, 1)).toBe(true);
    expect(r.recordSeen(key, 12, B, 2)).toBe(false);
    expect(r.recordSeen(key, 9, B, 3)).toBe(true);
    expect(r.seen(key)).toEqual([
      { id: 9, author: B },
      { id: 12, author: B },
    ]);
  });

  it("bounds the claims per request to the lowest ids", () => {
    const r = linkClaimRecord("acct", memoryStorage());
    for (let i = MAX_CLAIMS_PER_REQUEST + 5; i >= 1; i -= 1) r.recordSeen(key, i, B, 1);
    expect(r.seen(key).length).toBe(MAX_CLAIMS_PER_REQUEST);
    expect(r.seen(key)[0]?.id).toBe(1);
    expect(r.recordSeen(key, 999, B, 2)).toBe(false);
  });

  it("recognises this device's own claim row by its bytes, recorded before the publish", () => {
    const storage = memoryStorage();
    const r = linkClaimRecord("acct", storage);
    r.addOwnWire(key, "c0ffee");
    expect(r.hasUnmatchedWires()).toBe(true);
    // A crash lost the publish answer; the receive loop meets the row later.
    expect(linkClaimRecord("acct", storage).recordOwnClaimRow("beef", 10)).toBe(false);
    expect(linkClaimRecord("acct", storage).recordOwnClaimRow("c0ffee", 10)).toBe(true);
    expect(r.own(key)).toEqual({ wires: [], ids: [10], won: null, settled: false, founded: null });
    expect(r.hasUnmatchedWires()).toBe(false);
    // A later duplicate claim lands as a second own id; the earlier stays.
    r.addOwnWire(key, "d00d");
    r.recordOwnClaimRow("d00d", 14);
    expect(r.own(key)?.ids).toEqual([10, 14]);
  });

  it("drops sealed claims that never landed, and records a win across a restart", () => {
    const storage = memoryStorage();
    linkClaimRecord("acct", storage).addOwnWire(key, "aa");
    linkClaimRecord("acct", storage).dropUnlanded(key);
    expect(linkClaimRecord("acct", storage).own(key)).toEqual({ wires: [], ids: [], won: null, settled: false, founded: null });
    linkClaimRecord("acct", storage).setSettled(key);
    expect(linkClaimRecord("acct", storage).own(key)?.settled).toBe(true);
    linkClaimRecord("acct", storage).setWon(key, 5);
    expect(linkClaimRecord("acct", storage).own(key)?.won).toBe(5);
    linkClaimRecord("acct", storage).forgetOwn(key);
    expect(linkClaimRecord("acct", storage).own(key)).toBeNull();
  });

  it("keeps one account's claims apart from another's and clears them on Forget device", () => {
    const storage = memoryStorage();
    linkClaimRecord("a", storage).recordSeen(key, 3, B, 1);
    expect(linkClaimRecord("b", storage).seen(key)).toEqual([]);
    clearLinkClaims("a", storage);
    expect(linkClaimRecord("a", storage).seen(key)).toEqual([]);
  });

  it("bounds the requests with seen claims, dropping the oldest", () => {
    const storage = memoryStorage();
    const r = linkClaimRecord("acct", storage);
    for (let i = 0; i <= MAX_SEEN_LINK_CLAIMS; i += 1) r.recordSeen(`k${i}`, i + 1, B, i);
    expect(r.seen("k0")).toEqual([]);
    expect(r.seen(`k${MAX_SEEN_LINK_CLAIMS}`)).toEqual([{ id: MAX_SEEN_LINK_CLAIMS + 1, author: B }]);
  });

  it("reads a damaged or older row as nothing held", () => {
    const storage = memoryStorage();
    storage.setItem("lakko.link-claims.v1.acct", "{not json");
    expect(linkClaimRecord("acct", storage).own(key)).toBeNull();
    storage.setItem("lakko.link-claims.v1.acct", JSON.stringify({ v: 1, seen: {}, own: { [key]: { state: "won" } } }));
    expect(linkClaimRecord("acct", storage).own(key)).toBeNull();
    storage.setItem(
      "lakko.link-claims.v1.acct",
      JSON.stringify({ v: 2, seen: {}, own: { [key]: { wires: [], ids: [0], won: null, settled: false, founded: null } } }),
    );
    expect(linkClaimRecord("acct", storage).own(key)).toBeNull();
  });
});
