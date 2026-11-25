// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  decideLinkCallStart,
  linkCallHistoryEntry,
  linkCallNotice,
  linkCallOccupiesSlot,
  linkCallOverlayShown,
  linkCallPagePhase,
  linkCallPageReturnsToApp,
  reduceLinkCallPhase,
  type LinkCallEvent,
  type LinkCallJournal,
  type LinkCallPhase,
} from "./link-call-host";

function fold(start: LinkCallPhase, events: readonly LinkCallEvent[]): LinkCallPhase {
  return events.reduce(reduceLinkCallPhase, start);
}

describe("reduceLinkCallPhase", () => {
  it("owner: live from founding, before signaling reports open", () => {
    // `MultiJoinerCall.host` fires onJoined inside host(), then connects.
    expect(
      fold("starting", [{ kind: "joined" }, { kind: "signaling-open", role: "owner" }]),
    ).toBe("live");
  });

  it("joiner: waits once signaling is open, goes live on its Welcome", () => {
    expect(fold("starting", [{ kind: "signaling-open", role: "joiner" }])).toBe("waiting");
    expect(
      fold("starting", [{ kind: "signaling-open", role: "joiner" }, { kind: "joined" }]),
    ).toBe("live");
  });

  it("joiner: a Welcome that beats the open report is not demoted back to waiting", () => {
    expect(
      fold("starting", [{ kind: "joined" }, { kind: "signaling-open", role: "joiner" }]),
    ).toBe("live");
  });

  it("the admission timeout: the session close is reported first, then no-committer wins", () => {
    // `onAdmissionTimeout` hangs the session up (onEnded) before onNoCommitter.
    expect(fold("waiting", [{ kind: "ended" }, { kind: "no-committer" }])).toBe("dead");
  });

  it("a refused redirect: the session close is reported first, then the error wins", () => {
    expect(fold("waiting", [{ kind: "ended" }, { kind: "error" }])).toBe("error");
  });

  it("a live call that closes has ended; a late no-committer cannot rewrite it as dead", () => {
    expect(fold("live", [{ kind: "ended" }])).toBe("ended");
    expect(fold("live", [{ kind: "no-committer" }])).toBe("live");
  });

  it("dead and error are final", () => {
    for (const final of ["dead", "error"] as const) {
      expect(
        fold(final, [
          { kind: "joined" },
          { kind: "ended" },
          { kind: "error" },
          { kind: "no-committer" },
          { kind: "signaling-open", role: "joiner" },
        ]),
      ).toBe(final);
    }
  });

  it("an ended call does not come back to life", () => {
    expect(fold("ended", [{ kind: "joined" }, { kind: "signaling-open", role: "joiner" }])).toBe(
      "ended",
    );
  });

  it("only starting, waiting and live hold the call slot", () => {
    expect(linkCallOccupiesSlot("starting")).toBe(true);
    expect(linkCallOccupiesSlot("waiting")).toBe(true);
    expect(linkCallOccupiesSlot("live")).toBe(true);
    expect(linkCallOccupiesSlot("ended")).toBe(false);
    expect(linkCallOccupiesSlot("dead")).toBe(false);
    expect(linkCallOccupiesSlot("error")).toBe(false);
  });
});

describe("decideLinkCallStart", () => {
  it("starts when the slot is free", () => {
    expect(
      decideLinkCallStart({
        occupant: null,
        activeLinkCallIdHex: null,
        requestedCallIdHex: "aa",
        verb: "join",
      }),
    ).toEqual({ kind: "start" });
  });

  it("refuses with the reason while a group or 1x1 call is live — never a shared UI", () => {
    const group = decideLinkCallStart({
      occupant: "group",
      activeLinkCallIdHex: null,
      requestedCallIdHex: "aa",
      verb: "join",
    });
    expect(group.kind).toBe("busy");
    expect(group.kind === "busy" && group.message()).toContain("group call");
    expect(
      decideLinkCallStart({
        occupant: "one-to-one",
        activeLinkCallIdHex: null,
        requestedCallIdHex: "aa",
        verb: "start",
      }).kind,
    ).toBe("busy");
  });

  it("refuses a different link while a link call is live", () => {
    expect(
      decideLinkCallStart({
        occupant: "link",
        activeLinkCallIdHex: "bb",
        requestedCallIdHex: "aa",
        verb: "join",
      }).kind,
    ).toBe("busy");
  });

  it("returning to the same link finds the call already running, not a second one", () => {
    expect(
      decideLinkCallStart({
        occupant: "link",
        activeLinkCallIdHex: "aa",
        requestedCallIdHex: "aa",
        verb: "join",
      }),
    ).toEqual({ kind: "already-in-this-call" });
  });
});

describe("linkCallOverlayShown", () => {
  it("is up for a live call whoever this device is", () => {
    expect(linkCallOverlayShown("live", "owner")).toBe(true);
    expect(linkCallOverlayShown("live", "joiner")).toBe(true);
  });

  it("is up from the first second for a founder, who pressed New call", () => {
    // docs/ux/call-links.md section 5: "The call screen, connecting. Never a
    // form" — and never a strip at the bottom of the screen they were on.
    expect(linkCallOverlayShown("starting", "owner")).toBe(true);
  });

  it("is not up for a joiner that has not been admitted yet", () => {
    expect(linkCallOverlayShown("starting", "joiner")).toBe(false);
    expect(linkCallOverlayShown("waiting", "joiner")).toBe(false);
  });

  it("is down once the call is over, and with no call at all", () => {
    for (const phase of ["ended", "dead", "error"] as LinkCallPhase[]) {
      expect(linkCallOverlayShown(phase, "owner")).toBe(false);
    }
    expect(linkCallOverlayShown(null, null)).toBe(false);
  });
});

describe("linkCallNotice", () => {
  it("says nothing while the /c/ route shows the call, or while the overlay does", () => {
    expect(linkCallNotice("waiting", "joiner", true, null)).toBeNull();
    expect(linkCallNotice("live", "owner", false, null)).toBeNull();
    expect(linkCallNotice(null, null, false, null)).toBeNull();
  });

  it("never doubles the founder's own call screen with a banner under it", () => {
    expect(linkCallNotice("starting", "owner", false, null)).toBeNull();
  });

  it("before the call is live: still going, with a way back and a way out", () => {
    expect(linkCallNotice("starting", "joiner", false, null)?.actions).toEqual(["open", "cancel"]);
    const waiting = linkCallNotice("waiting", "joiner", false, null);
    expect(waiting?.text).toBe("Waiting for the owner to admit you...");
    expect(waiting?.actions).toEqual(["open", "cancel"]);
  });

  it("after it ends: why, once, dismissible", () => {
    expect(linkCallNotice("ended", "owner", false, null)).toEqual({
      text: "The call has ended.",
      actions: ["dismiss"],
    });
    expect(linkCallNotice("dead", "joiner", false, null)?.text).toBe(
      "This call has ended or is not active.",
    );
    expect(linkCallNotice("error", "joiner", false, () => "Joined the wrong call.")?.text).toBe(
      "The call failed. Joined the wrong call.",
    );
    expect(linkCallNotice("error", "owner", false, null)?.text).toBe("The call failed.");
  });
});

describe("linkCallHistoryEntry", () => {
  function journal(over: Partial<LinkCallJournal> = {}): LinkCallJournal {
    return {
      callIdHex: "00112233445566778899aabbccddeeff",
      role: "owner",
      video: true,
      liveAtMs: 3_000,
      recorded: false,
      ...over,
    };
  }
  const facts = { endedAtMs: 63_000, isGuest: false };

  it("writes an outgoing row for a call this device started", () => {
    expect(linkCallHistoryEntry(journal(), facts)).toEqual({
      id: "00112233445566778899aabbccddeeff",
      outcome: "outgoing",
      video: true,
      // Filed at the moment this device got past the join step, and as long
      // as it was in the call from then.
      startedAtMs: 3_000,
      durationMs: 60_000,
    });
  });

  it("writes an incoming row for a call joined from a link", () => {
    expect(linkCallHistoryEntry(journal({ role: "joiner", video: false }), facts)?.outcome).toBe(
      "incoming",
    );
  });

  it("writes nothing for a device that never got past the join step", () => {
    expect(linkCallHistoryEntry(journal({ role: "joiner", liveAtMs: null }), facts)).toBeNull();
  });

  it("writes nothing for a guest account", () => {
    expect(linkCallHistoryEntry(journal(), { ...facts, isGuest: true })).toBeNull();
  });

  it("writes nothing a second time for the same attempt", () => {
    expect(linkCallHistoryEntry(journal({ recorded: true }), facts)).toBeNull();
  });

  it("never reports a negative duration when the clock moved backwards", () => {
    expect(linkCallHistoryEntry(journal(), { ...facts, endedAtMs: 0 })?.durationMs).toBe(0);
  });
});

describe("a link to a call that had already ended (seen live 2026-11-24)", () => {
  it("the SFU's call-ended refusal folds to spent, which is final", () => {
    expect(fold("waiting", [{ kind: "call-already-ended" }])).toBe("spent");
    expect(fold("starting", [{ kind: "call-already-ended" }])).toBe("spent");
    // An `ended` folded a moment earlier for the same teardown is replaced.
    expect(fold("waiting", [{ kind: "ended" }, { kind: "call-already-ended" }])).toBe("spent");
    // The admission timer that fires afterwards changes nothing.
    expect(fold("spent", [{ kind: "no-committer" }, { kind: "ended" }, { kind: "error" }])).toBe("spent");
    expect(linkCallOccupiesSlot("spent")).toBe(false);
    expect(linkCallNotice("spent", "joiner", false, null)?.text).toBe(
      linkCallNotice("ended", "joiner", false, null)?.text,
    );
  });

  it("a call-ended answer on join leaves the page showing the ended state, not the app", () => {
    const phase = linkCallPagePhase({
      slotPhase: fold("waiting", [{ kind: "call-already-ended" }]),
      localPhase: "idle",
      roomEnded: false,
    });
    expect(phase).toBe("spent");
    expect(linkCallPageReturnsToApp({ phase, signedIn: true, isGuest: false })).toBe(false);
  });

  it("control: a call this device was in that ends still returns a signed-in person to the app", () => {
    const phase = fold("live", [{ kind: "ended" }]);
    expect(linkCallPageReturnsToApp({ phase, signedIn: true, isGuest: false })).toBe(true);
    expect(linkCallPageReturnsToApp({ phase, signedIn: true, isGuest: true })).toBe(false);
    expect(linkCallPageReturnsToApp({ phase, signedIn: false, isGuest: false })).toBe(false);
  });

  it("the room probe answering ended shows the ended state up front, only while idle", () => {
    expect(linkCallPagePhase({ slotPhase: null, localPhase: "idle", roomEnded: true })).toBe("spent");
    expect(linkCallPagePhase({ slotPhase: null, localPhase: "idle", roomEnded: false })).toBe("idle");
    expect(linkCallPagePhase({ slotPhase: null, localPhase: "starting", roomEnded: true })).toBe("starting");
    // The host's own phase for the call wins.
    expect(linkCallPagePhase({ slotPhase: "live", localPhase: "idle", roomEnded: true })).toBe("live");
  });
});
