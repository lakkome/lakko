// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The call that outlived its peer by 45 minutes.
 *
 * On 2026-09-21 a 1x1 call ran in the browser for three quarters of an hour
 * after the other side had left it, counting time the whole way and writing
 * `Incoming call, 45:12` into the conversation when it was ended by hand. The
 * ICE handling could not catch it: `failed` ends a call at once and
 * `disconnected` ends it after the grace, but the ICE state never left
 * `connected` — both ends were on one machine, so the candidate pair stayed
 * reachable after the peer stopped taking part. These tests pin the question
 * nothing was asking: is anything still arriving.
 */
import { describe, expect, it } from "vitest";

import { inboundHasGoneSilent, noteInboundMedia } from "./media-watchdog";

describe("noteInboundMedia", () => {
  it("starts the clock at the first sample", () => {
    expect(noteInboundMedia(null, 1000, 5_000)).toEqual({
      packets: 1000,
      lastChangeAtMs: 5_000,
    });
  });

  it("restarts the clock whenever the counter moves", () => {
    const first = noteInboundMedia(null, 1000, 5_000);
    const second = noteInboundMedia(first, 1400, 7_000);
    expect(second).toEqual({ packets: 1400, lastChangeAtMs: 7_000 });
  });

  it("leaves the clock alone while the counter stands still", () => {
    const first = noteInboundMedia(null, 1000, 5_000);
    const same = noteInboundMedia(first, 1000, 9_000);
    expect(same.lastChangeAtMs).toBe(5_000);
  });

  it("counts a counter that went BACKWARDS as activity, not silence", () => {
    // A reset means the pair was replaced (renegotiation, ICE restart).
    // Reading it as silence would end a call that is working.
    const first = noteInboundMedia(null, 90_000, 5_000);
    const reset = noteInboundMedia(first, 12, 6_000);
    expect(reset).toEqual({ packets: 12, lastChangeAtMs: 6_000 });
  });
});

describe("inboundHasGoneSilent", () => {
  it("ends a call that has received nothing for the whole window", () => {
    const activity = noteInboundMedia(null, 1000, 0);
    expect(inboundHasGoneSilent(activity, 30_000, 30_000)).toBe(true);
  });

  it("does not end one a moment short of it", () => {
    const activity = noteInboundMedia(null, 1000, 0);
    expect(inboundHasGoneSilent(activity, 29_999, 30_000)).toBe(false);
  });

  it("never ends a call before the first sample", () => {
    expect(inboundHasGoneSilent(null, 10_000_000, 30_000)).toBe(false);
  });

  it("is disabled, not instant, at a timeout of zero or less", () => {
    const activity = noteInboundMedia(null, 1000, 0);
    expect(inboundHasGoneSilent(activity, 10_000_000, 0)).toBe(false);
    expect(inboundHasGoneSilent(activity, 10_000_000, -1)).toBe(false);
  });

  it("a trickle of bytes keeps a call alive indefinitely", () => {
    // A muted microphone still sends RTP and a 1x1 call always carries an
    // audio section both ways, so any live call keeps this counter moving.
    let activity = noteInboundMedia(null, 0, 0);
    for (let t = 2_000; t <= 120_000; t += 2_000) {
      activity = noteInboundMedia(activity, t, t);
      expect(inboundHasGoneSilent(activity, t, 30_000)).toBe(false);
    }
  });

  it("the 45-minute call would have ended in the first minute", () => {
    // 2 s samples, the counter frozen from the moment the peer left.
    let activity = noteInboundMedia(null, 500_000, 0);
    let endedAtMs: number | null = null;
    for (let t = 2_000; t <= 45 * 60_000; t += 2_000) {
      activity = noteInboundMedia(activity, 500_000, t);
      if (endedAtMs === null && inboundHasGoneSilent(activity, t, 30_000)) {
        endedAtMs = t;
      }
    }
    expect(endedAtMs).toBe(30_000);
  });
});
