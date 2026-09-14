// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  decideLinkCallStart,
  linkCallNotice,
  linkCallOccupiesSlot,
  reduceLinkCallPhase,
  type LinkCallEvent,
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
    expect(group.kind === "busy" && group.message).toContain("group call");
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

describe("linkCallNotice", () => {
  it("says nothing while the /c/ route shows the call, or while the overlay does", () => {
    expect(linkCallNotice("waiting", true, null)).toBeNull();
    expect(linkCallNotice("live", false, null)).toBeNull();
    expect(linkCallNotice(null, false, null)).toBeNull();
  });

  it("before the call is live: still going, with a way back and a way out", () => {
    expect(linkCallNotice("starting", false, null)?.actions).toEqual(["open", "cancel"]);
    const waiting = linkCallNotice("waiting", false, null);
    expect(waiting?.text).toBe("Waiting for the owner to admit you…");
    expect(waiting?.actions).toEqual(["open", "cancel"]);
  });

  it("after it ends: why, once, dismissible", () => {
    expect(linkCallNotice("ended", false, null)).toEqual({
      text: "The call has ended.",
      actions: ["dismiss"],
    });
    expect(linkCallNotice("dead", false, null)?.text).toBe("This call has ended or isn't active.");
    expect(linkCallNotice("error", false, "Joined the wrong call")?.text).toBe(
      "The call failed: Joined the wrong call",
    );
    expect(linkCallNotice("error", false, null)?.text).toBe("The call failed.");
  });
});
