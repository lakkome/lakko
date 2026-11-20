// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { joinPageView, waitForGroup, type WaitTimers } from "./join-group-wait";

const NOW = 1_800_000_000_000;
const DAY = 24 * 60 * 60 * 1000;

describe("joinPageView (group-invite-page.md, After Request to join)", () => {
  it("offers Request to join when this device has not asked this group", () => {
    expect(joinPageView({ phase: "idle", sentAtMs: null, nowMs: NOW })).toBe("request");
  });

  it("right after the request: the waiting line and no control", () => {
    expect(joinPageView({ phase: "waiting", sentAtMs: NOW, nowMs: NOW })).toBe("waiting");
    // Sent but not recorded (no account bound): still the waiting line.
    expect(joinPageView({ phase: "waiting", sentAtMs: null, nowMs: NOW })).toBe("waiting");
  });

  it("opened again within the day for a group already asked: the waiting line, no button", () => {
    expect(joinPageView({ phase: "idle", sentAtMs: NOW - DAY + 60_000, nowMs: NOW })).toBe("waiting");
  });

  it("a day or more: No answer yet and Ask again, also on a reopened page", () => {
    expect(joinPageView({ phase: "idle", sentAtMs: NOW - DAY, nowMs: NOW })).toBe("no-answer");
    expect(joinPageView({ phase: "waiting", sentAtMs: NOW - 3 * DAY, nowMs: NOW })).toBe("no-answer");
  });

  it("an Ask again that went out restarts the day", () => {
    // The record is replaced with the new time.
    expect(joinPageView({ phase: "idle", sentAtMs: NOW, nowMs: NOW })).toBe("waiting");
  });

  it("sending, joined and a failure are what this visit did, whatever the record says", () => {
    expect(joinPageView({ phase: "requesting", sentAtMs: NOW - 2 * DAY, nowMs: NOW })).toBe("sending");
    expect(joinPageView({ phase: "joined", sentAtMs: NOW, nowMs: NOW })).toBe("joined");
    expect(joinPageView({ phase: "error", sentAtMs: null, nowMs: NOW })).toBe("error");
  });
});

function fakeTimers(): WaitTimers & { tick: () => void; live: () => number } {
  const fns = new Map<number, () => void>();
  let next = 1;
  return {
    setInterval: (fn) => {
      const id = next++;
      fns.set(id, fn);
      return id;
    },
    clearInterval: (id) => {
      fns.delete(id);
    },
    tick: () => {
      for (const fn of [...fns.values()]) fn();
    },
    live: () => fns.size,
  };
}

describe("waitForGroup", () => {
  it("opens the chat once, the moment the group lands, with no step in between", () => {
    const timers = fakeTimers();
    let landed = false;
    const opened: string[] = [];
    waitForGroup({ landed: () => landed, onLanded: () => opened.push("chat"), timers });
    timers.tick();
    timers.tick();
    expect(opened).toEqual([]);
    landed = true;
    timers.tick();
    expect(opened).toEqual(["chat"]);
    expect(timers.live()).toBe(0);
    timers.tick();
    expect(opened).toEqual(["chat"]);
  });

  it("stops when the page goes", () => {
    const timers = fakeTimers();
    let landed = false;
    const opened: string[] = [];
    const stop = waitForGroup({ landed: () => landed, onLanded: () => opened.push("chat"), timers });
    stop();
    landed = true;
    timers.tick();
    expect(opened).toEqual([]);
    expect(timers.live()).toBe(0);
  });
});
