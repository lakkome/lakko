// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { claimPlayback, releasePlayback } from "./audio-exclusive";
import {
  durationUnit,
  formatClock,
  pollFooterStatus,
  pollKindKey,
  pollPercents,
  pollShowsResults,
} from "./poll-view";
import type { PollView } from "./types";

const base: PollView = {
  question: "q",
  anonymous: false,
  quiz: false,
  multiple: false,
  options: [],
  voters: 0,
  voted: false,
  closed: false,
  sending: false,
  failed: false,
  canVote: true,
  unverifiable: false,
  canViewResults: false,
};

describe("pollPercents", () => {
  it("adds up to 100 for a single answer", () => {
    const p = pollPercents([1, 1, 1], 3, false);
    expect(p.reduce((a, b) => a + b, 0)).toBe(100);
    expect(p).toEqual([34, 33, 33]);
  });
  it("is zero with no votes", () => {
    expect(pollPercents([0, 0], 0, false)).toEqual([0, 0]);
  });
  it("is a share of voters with several answers", () => {
    expect(pollPercents([2, 1], 2, true)).toEqual([100, 50]);
  });
});

describe("footer and kind", () => {
  it("picks the kind line", () => {
    expect(pollKindKey(true, false)).toBe("poll.kind.anonymous");
    expect(pollKindKey(false, false)).toBe("poll.kind.public");
    expect(pollKindKey(false, true)).toBe("poll.kind.quiz");
    expect(pollKindKey(true, true)).toBe("poll.kind.anonymous_quiz");
  });
  it("orders sending, failed, closed, closes-in", () => {
    expect(pollFooterStatus({ ...base, sending: true, closed: true }, 0).kind).toBe("sending");
    expect(pollFooterStatus({ ...base, failed: true }, 0).kind).toBe("failed");
    expect(pollFooterStatus({ ...base, closed: true }, 0).kind).toBe("closed");
    expect(pollFooterStatus({ ...base, closesAtMs: 100 }, 50)).toEqual({ kind: "closes-in", remainingMs: 50 });
    expect(pollFooterStatus({ ...base, closesAtMs: 100 }, 100).kind).toBe("closed");
    expect(pollFooterStatus(base, 0).kind).toBe("none");
  });
  it("shows results after voting or closing, never when unverifiable", () => {
    expect(pollShowsResults({ ...base, voted: true })).toBe(true);
    expect(pollShowsResults({ ...base, closed: true, unverifiable: true })).toBe(false);
    expect(pollShowsResults(base)).toBe(false);
  });
  it("uses the largest whole unit, rounded up", () => {
    expect(durationUnit(90 * 60_000)).toEqual({ value: 2, unit: "hour" });
    expect(durationUnit(25 * 3600_000)).toEqual({ value: 2, unit: "day" });
    expect(durationUnit(10_000)).toEqual({ value: 1, unit: "minute" });
  });
  it("formats an audio clock", () => {
    expect(formatClock(65)).toBe("1:05");
    expect(formatClock(3725)).toBe("1:02:05");
  });
});

describe("claimPlayback", () => {
  it("pauses the previous element only", () => {
    const mk = () => {
      const el = { paused: false, pauses: 0, pause() { this.pauses += 1; this.paused = true; } };
      return el;
    };
    const a = mk();
    const b = mk();
    claimPlayback(a);
    claimPlayback(a);
    expect(a.pauses).toBe(0);
    claimPlayback(b);
    expect(a.pauses).toBe(1);
    releasePlayback(b);
    claimPlayback(a);
    expect(b.pauses).toBe(0);
  });
});
