// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  type HeldLinkNote,
  type LinkBuildDeps,
  type LinkFounded,
  MAX_CLAIMS_PER_REQUEST,
  MAX_LINK_NOTES,
  linkBuildStep,
  runLinkBuild,
  welcomeOrder,
  MAX_SEEN_LINK_CLAIMS,
  claimsToDecide,
  clearLinkClaims,
  linkClaimRecord,
  linkRequestKeyHex,
  linkRequestStep,
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
const posted = { wires: [], ids: [9], won: null, settled: false, founded: null, noteWire: null, noteConfirmed: false };
const posting = { wires: ["aa"], ids: [], won: null, settled: false, founded: null, noteWire: null, noteConfirmed: false };
const lost = { wires: [], ids: [9], won: null, settled: true, founded: null, noteWire: null, noteConfirmed: false };

describe("linkRequestStep (ADR-0079, reworked 2026-10-17)", () => {
  it("waits, with no timer, while the synced state is not loaded", () => {
    expect(linkRequestStep({ own: null, answeredElsewhere: false, verdict: null })).toBe("wait");
  });

  it("follows the register once it is loaded", () => {
    expect(linkRequestStep({ own: null, answeredElsewhere: false, verdict: "accept" })).toBe("post-claim");
    expect(linkRequestStep({ own: null, answeredElsewhere: false, verdict: "answered" })).toBe("drop");
    expect(linkRequestStep({ own: null, answeredElsewhere: false, verdict: "dead" })).toBe("refuse");
  });

  it("waits on a link this device holds no record of, and never refuses it (2026-10-19)", () => {
    expect(linkRequestStep({ own: null, answeredElsewhere: false, verdict: "unknown" })).toBe("wait");
  });

  it("waits, unshown, on a roster device's claim until the register says answered", () => {
    expect(linkRequestStep({ own: null, answeredElsewhere: true, verdict: "accept" })).toBe("wait");
    expect(linkRequestStep({ own: null, answeredElsewhere: true, verdict: null })).toBe("wait");
    expect(linkRequestStep({ own: null, answeredElsewhere: true, verdict: "answered" })).toBe("drop");
    expect(linkRequestStep({ own: null, answeredElsewhere: true, verdict: "dead" })).toBe("drop");
    const none = { wires: [], ids: [], won: null, settled: false, founded: null, noteWire: null, noteConfirmed: false };
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

  it("builds a won claim once answered, but a revocation before the answer refuses (2026-10-19)", () => {
    const won = { wires: [], ids: [9], won: 9, settled: false, founded: null, noteWire: null, noteConfirmed: false };
    // Revoked (or otherwise dead) before this device founded: refused.
    expect(linkRequestStep({ own: won, answeredElsewhere: true, verdict: "dead" })).toBe("refuse");
    // Another device already founded (its note is held): the answer stands.
    expect(linkRequestStep({ own: won, answeredElsewhere: true, verdict: "dead", noteHeld: true })).toBe("build");
    // This device founded already: the answer was given, the build carries on.
    const founded = { ...won, founded: { groupIdB64: "g", welcomeB64: "w", targets: [], notePosted: true, delivered: false } };
    expect(linkRequestStep({ own: founded, answeredElsewhere: true, verdict: "dead" })).toBe("build");
    expect(linkRequestStep({ own: won, answeredElsewhere: true, verdict: "answered" })).toBe("build");
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
  const founding: LinkFounded = {
    groupIdB64: "g",
    welcomeB64: "w",
    targets: [
      { accountKeyB64: "own", isRecipient: false },
      { accountKeyB64: "requester", isRecipient: true },
    ],
    notePosted: false,
    delivered: false,
  };

  /** A device with recorded state; every step logs its name in order. */
  function device(
    initial: { founded?: LinkFounded | null; note?: HeldLinkNote | null; heldNote?: boolean; noteCounts?: boolean } = {},
  ) {
    const log: string[] = [];
    let founded = initial.founded ?? null;
    const deps: LinkBuildDeps = {
      founded: () => founded,
      note: () => initial.note ?? null,
      prepare: async () => {
        log.push("prepare");
        return founding;
      },
      record: (f) => {
        founded = f;
        log.push(`record:${f.notePosted ? "note" : "-"}:${f.delivered ? "delivered" : "-"}`);
      },
      postNote: async () => {
        log.push("postNote");
        return initial.noteCounts ?? true;
      },
      hold: async (f) => {
        log.push("hold");
        return f.groupIdB64;
      },
      deliver: async () => {
        log.push("deliver");
      },
      forward: async (n) => {
        log.push(`forward:${n.groupIdB64}`);
        return initial.heldNote === true ? n.groupIdB64 : null;
      },
      finish: async (g) => {
        log.push(`finish:${g ?? "none"}`);
      },
    };
    return { deps, log, founded: () => founded };
  }

  it("writes this account's own mailbox first and the requester's after", () => {
    const targets = [
      { accountKeyB64: "requester", isRecipient: true },
      { accountKeyB64: "own", isRecipient: false },
    ];
    expect(welcomeOrder(targets).map((t) => t.accountKeyB64)).toEqual(["own", "requester"]);
  });

  it("posts the note before any Welcome, and records each step before the next", async () => {
    const d = device();
    expect(await runLinkBuild(d.deps)).toBe(true);
    expect(d.log).toEqual([
      "prepare",
      "record:-:-",
      "postNote",
      "record:note:-",
      "hold",
      "deliver",
      "record:note:delivered",
      "finish:g",
    ]);
    expect(d.log.indexOf("postNote")).toBeLessThan(d.log.indexOf("deliver"));
  });

  it("delivers no Welcome when the note's publish fails", async () => {
    const d = device();
    d.deps.postNote = async () => {
      throw new Error("offline");
    };
    await expect(runLinkBuild(d.deps)).rejects.toThrow();
    expect(d.log).not.toContain("deliver");
    // The founding is recorded; the next pass resumes it, never founds again.
    expect(d.founded()?.notePosted).toBe(false);
  });

  it("delivers no Welcome on a note that landed straddled (it did not count)", async () => {
    const d = device({ noteCounts: false });
    expect(await runLinkBuild(d.deps)).toBe(false);
    expect(d.log).toEqual(["prepare", "record:-:-", "postNote"]);
    expect(d.founded()?.notePosted).toBe(false);
  });

  it("a takeover device that is not a member of the founded group and sees a note does not found and forwards", async () => {
    const d = device({ note: { id: 7, groupIdB64: "theirs", welcomeB64: "w" }, heldNote: false });
    expect(await runLinkBuild(d.deps)).toBe(true);
    expect(d.log).toEqual(["forward:theirs", "finish:none"]);
    expect(linkBuildStep({ hasFounded: false, hasNote: true })).toBe("forward");
  });

  it("a founding already recorded is resumed, never founded again, even with a note held", async () => {
    const d = device({ founded: { ...founding, notePosted: true }, note: { id: 7, groupIdB64: "theirs", welcomeB64: "w" } });
    expect(await runLinkBuild(d.deps)).toBe(true);
    expect(d.log).not.toContain("prepare");
    expect(d.log).not.toContain("postNote");
    expect(d.log).not.toContain("forward:theirs");
    expect(linkBuildStep({ hasFounded: true, hasNote: true })).toBe("resume");
    expect(linkBuildStep({ hasFounded: false, hasNote: false })).toBe("found");
  });

  it("keeps the earliest note per request, bounded, across a restart", () => {
    const storage = memoryStorage();
    const r = linkClaimRecord("acct", storage);
    expect(r.note("k")).toBeNull();
    expect(r.recordNote("k", { id: 9, groupIdB64: "a", welcomeB64: "w" }, 1)).toBe(true);
    expect(r.recordNote("k", { id: 12, groupIdB64: "b", welcomeB64: "w" }, 2)).toBe(false);
    expect(r.recordNote("k", { id: 4, groupIdB64: "c", welcomeB64: "w" }, 3)).toBe(true);
    expect(linkClaimRecord("acct", storage).note("k")?.groupIdB64).toBe("c");
    for (let i = 0; i <= MAX_LINK_NOTES; i += 1) r.recordNote(`k${i}`, { id: 1, groupIdB64: "g", welcomeB64: "w" }, i + 10);
    expect(r.note("k")).toBeNull();
    clearLinkClaims("acct", storage);
    expect(linkClaimRecord("acct", storage).note(`k${MAX_LINK_NOTES}`)).toBeNull();
  });

  it("records the founding with its note flag", () => {
    const storage = memoryStorage();
    linkClaimRecord("acct", storage).setFounded("k", founding);
    expect(linkClaimRecord("acct", storage).own("k")?.founded).toEqual(founding);
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
    // A crash lost the publish answer; the receive loop meets the row later
    // and judges it at its position.
    expect(linkClaimRecord("acct", storage).judgeOwnRow("beef", 10, true)).toBeNull();
    expect(linkClaimRecord("acct", storage).judgeOwnRow("c0ffee", 10, true)).toBe("claim");
    expect(r.own(key)).toEqual({
      wires: [],
      ids: [10],
      won: null,
      settled: false,
      founded: null,
      noteWire: null,
      noteConfirmed: false,
    });
    expect(r.hasUnmatchedWires()).toBe(false);
    // A later duplicate claim lands as a second own id; the earlier stays.
    r.addOwnWire(key, "d00d");
    r.judgeOwnRow("d00d", 14, true);
    expect(r.own(key)?.ids).toEqual([10, 14]);
  });

  it("a straddled own claim does not count for its author either: dropped, claimed again", () => {
    const r = linkClaimRecord("acct", memoryStorage());
    r.addOwnWire(key, "c0ffee");
    expect(r.judgeOwnRow("c0ffee", 10, false)).toBe("claim");
    expect(r.own(key)?.ids).toEqual([]);
    expect(r.own(key)?.wires).toEqual([]);
    expect(linkRequestStep({ own: r.own(key), answeredElsewhere: false, verdict: "accept" })).toBe("post-claim");
  });

  it("confirms this device's note only when its row counts", () => {
    const r = linkClaimRecord("acct", memoryStorage());
    r.setNoteWire(key, "abcd");
    expect(r.hasUnmatchedWires()).toBe(true);
    expect(r.judgeOwnRow("abcd", 20, false)).toBe("note");
    expect(r.own(key)?.noteConfirmed).toBe(false);
    expect(r.own(key)?.noteWire).toBeNull();
    r.setNoteWire(key, "ef01");
    expect(r.judgeOwnRow("ef01", 22, true)).toBe("note");
    expect(r.own(key)?.noteConfirmed).toBe(true);
    r.setNoteWire(key, "aa");
    r.dropUnlandedNote(key);
    expect(r.own(key)?.noteWire).toBeNull();
  });

  it("keeps the journal's epochs for one DeviceGroup only", () => {
    const r = linkClaimRecord("acct", memoryStorage());
    expect(r.journal("aa")).toBeNull();
    r.setJournal({ groupIdHex: "aa", base: "5", commitRowIds: ["11"], commitEpochsAfter: ["6"] });
    expect(r.journal("aa")?.base).toBe("5");
    expect(r.journal("bb")).toBeNull();
  });

  it("drops sealed claims that never landed, and records a win across a restart", () => {
    const storage = memoryStorage();
    linkClaimRecord("acct", storage).addOwnWire(key, "aa");
    linkClaimRecord("acct", storage).dropUnlanded(key);
    expect(linkClaimRecord("acct", storage).own(key)).toEqual({ wires: [], ids: [], won: null, settled: false, founded: null, noteWire: null, noteConfirmed: false });
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
      JSON.stringify({ v: 2, seen: {}, own: { [key]: { wires: [], ids: [0], won: null, settled: false, founded: null, noteWire: null, noteConfirmed: false } } }),
    );
    expect(linkClaimRecord("acct", storage).own(key)).toBeNull();
  });
});
