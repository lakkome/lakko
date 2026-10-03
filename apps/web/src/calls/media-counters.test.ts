// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { deltaOf, describeDeltas, describeOutbound, readMedia } from "./media-counters";

const video = (over: Record<string, unknown> = {}): Record<string, unknown> => ({
  id: "IT01V",
  type: "inbound-rtp",
  kind: "video",
  packetsReceived: 1000,
  packetsLost: 10,
  nackCount: 4,
  pliCount: 1,
  freezeCount: 2,
  totalFreezesDuration: 1.5,
  framesDecoded: 300,
  framesDropped: 3,
  jitterBufferDelay: 15,
  jitterBufferEmittedCount: 300,
  ...over,
});

const audio = (over: Record<string, unknown> = {}): Record<string, unknown> => ({
  id: "IT01A",
  type: "inbound-rtp",
  kind: "audio",
  packetsReceived: 500,
  packetsLost: 5,
  jitterBufferDelay: 24,
  jitterBufferEmittedCount: 480_000,
  concealedSamples: 4_800,
  totalSamplesReceived: 480_000,
  ...over,
});

describe("readMedia", () => {
  it("keeps the inbound streams and the camera sender, nothing else", () => {
    const r = readMedia([
      video(),
      audio(),
      { id: "CP", type: "candidate-pair", currentRoundTripTime: 0.04 },
      { id: "OT", type: "outbound-rtp", kind: "video", scalabilityMode: "L1T3", frameWidth: 640, frameHeight: 480 },
    ]);
    expect([...r.inbound.keys()]).toEqual(["IT01V", "IT01A"]);
    expect(r.outboundVideo?.scalabilityMode).toBe("L1T3");
  });

  it("reads a missing or non-numeric counter as zero", () => {
    const r = readMedia([video({ freezeCount: undefined, framesDropped: "3" })]);
    expect(r.inbound.get("IT01V")?.freezeCount).toBe(0);
    expect(r.inbound.get("IT01V")?.framesDropped).toBe(0);
  });
});

describe("deltaOf", () => {
  it("measures the interval between two readings", () => {
    const before = readMedia([video()]);
    const now = readMedia([
      video({
        packetsReceived: 1500,
        freezeCount: 3,
        totalFreezesDuration: 2.25,
        jitterBufferDelay: 21,
        jitterBufferEmittedCount: 400,
      }),
    ]);
    const d = deltaOf(before, now).get("IT01V");
    expect(d?.packetsReceived).toBe(500);
    expect(d?.freezes).toBe(1);
    expect(d?.freezeSeconds).toBeCloseTo(0.75);
    // 6 s of buffer time over 100 frames = 60 ms each.
    expect(d?.jitterBufferMs).toBeCloseTo(60);
  });

  it("measures a new stream from zero", () => {
    const d = deltaOf(null, readMedia([audio()])).get("IT01A");
    expect(d?.packetsReceived).toBe(500);
    expect(d?.concealedShare).toBeCloseTo(0.01);
    expect(d?.jitterBufferMs).toBeCloseTo(0.05);
  });

  it("starts over when a stream's counters went backwards", () => {
    const before = readMedia([video({ packetsReceived: 9000 })]);
    const d = deltaOf(before, readMedia([video({ packetsReceived: 20 })])).get("IT01V");
    expect(d?.packetsReceived).toBe(20);
  });

  it("says nothing about the buffer when nothing left it", () => {
    const before = readMedia([video()]);
    expect(deltaOf(before, readMedia([video()])).get("IT01V")?.jitterBufferMs).toBeNull();
  });
});

describe("the log lines", () => {
  it("are plain text with no ids", () => {
    const lines = describeDeltas("call media", deltaOf(null, readMedia([video(), audio()])));
    expect(lines).toHaveLength(2);
    for (const line of lines) {
      expect(line).toMatch(/^[\x20-\x7e]+$/);
      expect(line).not.toContain("IT01");
    }
    expect(lines[0]).toContain("freezes=2");
    expect(lines[1]).toContain("concealed=1.0%");
  });

  it("describe the camera sender, or nothing without one", () => {
    expect(describeOutbound("call media", null)).toBeNull();
    const line = describeOutbound(
      "call media",
      readMedia([{ id: "OT", type: "outbound-rtp", kind: "video", scalabilityMode: "L1T3" }]).outboundVideo,
    );
    expect(line).toContain("mode=L1T3");
  });
});
