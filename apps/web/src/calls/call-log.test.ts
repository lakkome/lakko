// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  callBackKind,
  callRowAccessibleLabel,
  callRowIsUnanswered,
  callRowLabel,
  classifyCallOutcome,
  formatCallDuration,
  spokenCallDuration,
  type CallLogEntry,
} from "./call-log";

function entry(over: Partial<CallLogEntry> = {}): CallLogEntry {
  return {
    id: "a1b2",
    outcome: "outgoing",
    video: false,
    startedAtMs: 1_700_000_000_000,
    durationMs: 724_000,
    ...over,
  };
}

describe("classifyCallOutcome", () => {
  it("files a deliberate refusal as Declined from either seat", () => {
    // The user chose this, and it is the one outcome that is not an accident.
    expect(
      classifyCallOutcome({ incoming: true, connected: false, reason: "declined" }),
    ).toBe("declined");
    expect(
      classifyCallOutcome({ incoming: false, connected: false, reason: "declined" }),
    ).toBe("declined");
  });

  it("files every unanswered incoming call as missed", () => {
    // docs/ux/calls.md section 2.1: a call nobody answered records a missed
    // call for both sides. The caller giving up, the ring running out, and
    // this device being busy are the same fact from the callee's seat.
    for (const reason of ["hungUp", "timedOut", "busy", "error", undefined] as const) {
      expect(classifyCallOutcome({ incoming: true, connected: false, reason })).toBe("missed");
    }
  });

  it("keeps an answered call as its own direction, however it ended", () => {
    // Hanging up is how calls normally finish; the row is not the place to
    // relitigate it.
    for (const reason of ["hungUp", "timedOut", "error", undefined] as const) {
      expect(classifyCallOutcome({ incoming: true, connected: true, reason })).toBe("incoming");
      expect(classifyCallOutcome({ incoming: false, connected: true, reason })).toBe("outgoing");
    }
  });

  it("still says Outgoing call for a call that never got through", () => {
    // True and terse: a call was placed. The absent duration is what says it
    // did not connect — never `Outgoing call, 0:00`.
    for (const reason of ["hungUp", "timedOut", "busy", "error", undefined] as const) {
      expect(classifyCallOutcome({ incoming: false, connected: false, reason })).toBe("outgoing");
    }
  });

  it("matches the four outcomes the natives ship and adds none", () => {
    const seen = new Set<string>();
    for (const incoming of [true, false]) {
      for (const connected of [true, false]) {
        for (const reason of ["hungUp", "busy", "declined", "timedOut", "error", undefined] as const) {
          seen.add(classifyCallOutcome({ incoming, connected, reason }));
        }
      }
    }
    expect([...seen].sort()).toEqual(["declined", "incoming", "missed", "outgoing"]);
  });
});

describe("formatCallDuration", () => {
  it("writes the spec's own example", () => {
    // docs/ux/calls.md section 4 / section 6: `12:04`.
    expect(formatCallDuration(724_000)).toBe("12:04");
  });

  it("zero-pads seconds and not minutes", () => {
    expect(formatCallDuration(5_000)).toBe("0:05");
    expect(formatCallDuration(65_000)).toBe("1:05");
  });

  it("keeps a short call's seconds rather than rounding them away", () => {
    // `0:11` says a call lasted eleven seconds; "less than a minute" does not.
    expect(formatCallDuration(11_000)).toBe("0:11");
  });

  it("grows an hours field rather than counting past 59 minutes", () => {
    expect(formatCallDuration(3_600_000)).toBe("1:00:00");
    expect(formatCallDuration(3_753_000)).toBe("1:02:33");
  });

  it("reads a clock that moved backwards as no duration, not a negative one", () => {
    expect(formatCallDuration(-5)).toBeNull();
    expect(formatCallDuration(null)).toBeNull();
  });
});

describe("callRowLabel", () => {
  it("writes exactly the four wordings the spec and the natives share", () => {
    expect(callRowLabel(entry({ outcome: "outgoing" }))).toBe("Outgoing call, 12:04");
    expect(callRowLabel(entry({ outcome: "incoming" }))).toBe("Incoming call, 12:04");
    expect(callRowLabel(entry({ outcome: "missed", durationMs: null }))).toBe("Missed call");
    expect(callRowLabel(entry({ outcome: "declined", durationMs: null }))).toBe("Declined");
  });

  it("says only which way a call went when it never connected", () => {
    // Never `Outgoing call, 0:00` — that would report a conversation that did
    // not happen.
    expect(callRowLabel(entry({ outcome: "outgoing", durationMs: null }))).toBe("Outgoing call");
    expect(callRowLabel(entry({ outcome: "incoming", durationMs: null }))).toBe("Incoming call");
  });

  it("never prints a duration on an outcome that cannot have one", () => {
    // Even if a caller managed to hand one in.
    expect(callRowLabel(entry({ outcome: "missed", durationMs: 724_000 }))).toBe("Missed call");
    expect(callRowLabel(entry({ outcome: "declined", durationMs: 724_000 }))).toBe("Declined");
  });

  it("has words for every outcome", () => {
    // A missing arm would be a row rendering as `undefined`.
    for (const outcome of ["outgoing", "incoming", "missed", "declined"] as const) {
      expect(callRowLabel(entry({ outcome })).length).toBeGreaterThan(0);
    }
  });
});

describe("callRowIsUnanswered", () => {
  it("tints the outcomes where nobody spoke", () => {
    expect(callRowIsUnanswered(entry({ outcome: "missed" }))).toBe(true);
    expect(callRowIsUnanswered(entry({ outcome: "declined" }))).toBe(true);
    expect(callRowIsUnanswered(entry({ outcome: "outgoing" }))).toBe(false);
    expect(callRowIsUnanswered(entry({ outcome: "incoming" }))).toBe(false);
  });
});

describe("callBackKind", () => {
  it("calls back in the kind the call was, so voice stays voice", () => {
    expect(callBackKind(entry({ video: false }))).toBe("audio");
    expect(callBackKind(entry({ video: true }))).toBe("video");
  });
});

describe("spokenCallDuration", () => {
  it("reads a duration as a duration", () => {
    expect(spokenCallDuration(724_000)).toBe("12 minutes 4 seconds");
  });

  it("uses the singular where it belongs", () => {
    expect(spokenCallDuration(61_000)).toBe("1 minute 1 second");
  });

  it("drops the units that are zero, but never says nothing", () => {
    expect(spokenCallDuration(120_000)).toBe("2 minutes");
    expect(spokenCallDuration(0)).toBe("0 seconds");
  });

  it("reads hours", () => {
    expect(spokenCallDuration(3_753_000)).toBe("1 hour 2 minutes 33 seconds");
  });

  it("is absent when there was no duration", () => {
    expect(spokenCallDuration(null)).toBeNull();
  });
});

describe("callRowAccessibleLabel", () => {
  it("says the kind, which way it went, how long, and what pressing does", () => {
    expect(callRowAccessibleLabel(entry({ outcome: "outgoing", video: false }))).toBe(
      "Call, outgoing, lasted 12 minutes 4 seconds. Call back",
    );
    expect(callRowAccessibleLabel(entry({ outcome: "incoming", video: true }))).toBe(
      "Video call, incoming, lasted 12 minutes 4 seconds. Call back",
    );
  });

  it("names the unanswered outcomes without a duration", () => {
    expect(callRowAccessibleLabel(entry({ outcome: "missed", durationMs: null }))).toBe(
      "Missed call. Call back",
    );
    expect(
      callRowAccessibleLabel(entry({ outcome: "declined", video: true, durationMs: null })),
    ).toBe("Video call declined. Call back");
  });

  it("does not carry a touch gesture the web has no equivalent for", () => {
    // The natives end with "Double tap to call back."; on the web the row is a
    // button whose role already tells the reader how to press it.
    for (const outcome of ["outgoing", "incoming", "missed", "declined"] as const) {
      expect(callRowAccessibleLabel(entry({ outcome }))).not.toContain("Double tap");
    }
  });
});

describe("a call answered here that never connected (D24)", () => {
  it("reads failed, not missed", () => {
    expect(classifyCallOutcome({ incoming: true, connected: false, accepted: true, reason: "error" })).toBe("failed");
  });

  it("is missed when nobody pressed Accept", () => {
    expect(classifyCallOutcome({ incoming: true, connected: false, accepted: false, reason: "timedOut" })).toBe("missed");
    expect(classifyCallOutcome({ incoming: true, connected: false, reason: "error" })).toBe("missed");
  });

  it("a decline stays a decline", () => {
    expect(classifyCallOutcome({ incoming: true, connected: false, accepted: true, reason: "declined" })).toBe("declined");
  });

  it("is drawn as unanswered", () => {
    expect(
      callRowIsUnanswered({ id: "a", outcome: "failed", video: false, startedAtMs: 1, durationMs: null }),
    ).toBe(true);
  });
});
