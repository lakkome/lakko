// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX spec §2, §3 — a rejected play() never shows "Couldn't play this message"
 * unless the media itself is broken.
 */
import { describe, expect, it } from "vitest";

import {
  errorName,
  mediaErrorIsReal,
  playRejectionOutcome,
  waitForMetadata,
  type ReadinessTarget,
} from "./playback-outcome";

const video = { retries: 0, muted: false, canPlayMuted: true };
const voice = { retries: 0, muted: false, canPlayMuted: false };

describe("playRejectionOutcome", () => {
  it("retries an AbortError once, then waits for a tap, never an error", () => {
    expect(playRejectionOutcome("AbortError", video)).toBe("retry");
    expect(playRejectionOutcome("AbortError", { ...video, retries: 1 })).toBe("tap-to-play");
    expect(playRejectionOutcome("AbortError", voice)).toBe("retry");
    expect(playRejectionOutcome("AbortError", { ...voice, retries: 5 })).toBe("tap-to-play");
  });

  it("falls back to muted playback for a video on NotAllowedError", () => {
    expect(playRejectionOutcome("NotAllowedError", video)).toBe("muted-fallback");
  });

  it("does not loop the muted fallback, and a voice message returns to its button", () => {
    expect(playRejectionOutcome("NotAllowedError", { ...video, muted: true })).toBe("tap-to-play");
    expect(playRejectionOutcome("NotAllowedError", voice)).toBe("tap-to-play");
  });

  it("shows the error only for an unsupported source", () => {
    expect(playRejectionOutcome("NotSupportedError", video)).toBe("error");
    expect(playRejectionOutcome("NotSupportedError", voice)).toBe("error");
  });

  it("treats an unnamed or unexpected rejection as not broken", () => {
    expect(playRejectionOutcome(undefined, video)).toBe("tap-to-play");
    expect(playRejectionOutcome("SomethingElse", voice)).toBe("tap-to-play");
  });
});

describe("errorName", () => {
  it("reads a DOMException-shaped name and nothing else", () => {
    expect(errorName({ name: "AbortError" })).toBe("AbortError");
    expect(errorName(new Error("x"))).toBe("Error");
    expect(errorName("NotAllowedError")).toBeUndefined();
    expect(errorName(null)).toBeUndefined();
    expect(errorName({ name: 3 })).toBeUndefined();
  });
});

describe("mediaErrorIsReal", () => {
  it("ignores an aborted load and missing codes", () => {
    expect(mediaErrorIsReal(1)).toBe(false);
    expect(mediaErrorIsReal(undefined)).toBe(false);
    expect(mediaErrorIsReal(2)).toBe(true);
    expect(mediaErrorIsReal(3)).toBe(true);
    expect(mediaErrorIsReal(4)).toBe(true);
  });
});

class FakeElement implements ReadinessTarget {
  readyState = 0;
  error: { code: number } | null = null;
  private listeners = new Map<string, Set<() => void>>();
  addEventListener(type: string, l: () => void): void {
    const set = this.listeners.get(type) ?? new Set();
    set.add(l);
    this.listeners.set(type, set);
  }
  removeEventListener(type: string, l: () => void): void {
    this.listeners.get(type)?.delete(l);
  }
  fire(type: string): void {
    for (const l of [...(this.listeners.get(type) ?? [])]) l();
  }
  listenerCount(): number {
    let n = 0;
    for (const s of this.listeners.values()) n += s.size;
    return n;
  }
}

function manualTimers(): {
  timers: { setTimeout: (fn: () => void, ms: number) => unknown; clearTimeout: (h: unknown) => void };
  fire: () => void;
  pending: () => number;
} {
  const pending = new Map<number, () => void>();
  let next = 1;
  return {
    timers: {
      setTimeout: (fn) => {
        const id = next++;
        pending.set(id, fn);
        return id;
      },
      clearTimeout: (h) => {
        pending.delete(h as number);
      },
    },
    fire: () => {
      for (const [id, fn] of [...pending]) {
        pending.delete(id);
        fn();
      }
    },
    pending: () => pending.size,
  };
}

describe("waitForMetadata", () => {
  it("resolves immediately when metadata is already there", async () => {
    const el = new FakeElement();
    el.readyState = 1;
    await expect(waitForMetadata(el, 1000)).resolves.toBe("ready");
  });

  it("resolves on loadedmetadata and removes every listener and the timer", async () => {
    const el = new FakeElement();
    const t = manualTimers();
    const p = waitForMetadata(el, 1000, t.timers);
    el.fire("loadedmetadata");
    await expect(p).resolves.toBe("ready");
    expect(el.listenerCount()).toBe(0);
    expect(t.pending()).toBe(0);
  });

  it("resolves on canplay too", async () => {
    const el = new FakeElement();
    const p = waitForMetadata(el, 1000, manualTimers().timers);
    el.fire("canplay");
    await expect(p).resolves.toBe("ready");
  });

  it("times out without an error", async () => {
    const el = new FakeElement();
    const t = manualTimers();
    const p = waitForMetadata(el, 1000, t.timers);
    t.fire();
    await expect(p).resolves.toBe("timeout");
    expect(el.listenerCount()).toBe(0);
  });

  it("reports a real media error but ignores an aborted load", async () => {
    const el = new FakeElement();
    const t = manualTimers();
    const p = waitForMetadata(el, 1000, t.timers);
    el.error = { code: 1 };
    el.fire("error");
    el.error = { code: 3 };
    el.fire("error");
    await expect(p).resolves.toBe("error");
  });

  it("returns error straight away for an element already in a real error", async () => {
    const el = new FakeElement();
    el.error = { code: 4 };
    await expect(waitForMetadata(el, 1000, manualTimers().timers)).resolves.toBe("error");
  });
});
