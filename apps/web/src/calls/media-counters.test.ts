// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  deltaOf,
  describeDeltas,
  describeOutbound,
  describeOutboundLayers,
  describeSender,
  describeShown,
  describeShownPeers,
  readMedia,
  readSender,
} from "./media-counters";

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

describe("a simulcast camera sender", () => {
  const layers: Record<string, unknown>[] = [
    {
      id: "OQ", type: "outbound-rtp", kind: "video", rid: "q", codecId: "CV", scalabilityMode: "L1T3",
      frameWidth: 320, frameHeight: 180, framesPerSecond: 30, qualityLimitationReason: "none",
      framesEncoded: 300, packetsSent: 400, retransmittedPacketsSent: 3, nackCount: 2, pliCount: 1,
      targetBitrate: 150_000,
    },
    {
      id: "OF", type: "outbound-rtp", kind: "video", rid: "f", codecId: "CV", scalabilityMode: "L1T3",
      frameWidth: 1280, frameHeight: 720, framesPerSecond: 24, qualityLimitationReason: "bandwidth",
      framesEncoded: 250, packetsSent: 2600, retransmittedPacketsSent: 40, nackCount: 9, pliCount: 2,
      targetBitrate: 1_100_000,
    },
    { id: "CV", type: "codec", mimeType: "video/VP8" },
    { id: "RQ", type: "remote-inbound-rtp", kind: "video", fractionLost: 0.02, roundTripTime: 0.03 },
    { id: "RF", type: "remote-inbound-rtp", kind: "video", fractionLost: 0.4, roundTripTime: 0.05 },
  ];

  it("reports each layer: rid, size, frame rate and what limits it", () => {
    const reading = readMedia(layers);
    expect(reading.outboundVideoLayers.map((l) => l.rid)).toEqual(["q", "f"]);
    expect(reading.outboundVideo?.rid).toBe("q");
    expect(describeOutboundLayers("call media interval", reading.outboundVideoLayers)).toEqual([
      "call media interval send video mode=L1T3 size=320x180 fps=30 limited_by=none rid=q",
      "call media interval send video mode=L1T3 size=1280x720 fps=24 limited_by=bandwidth rid=f",
    ]);
  });

  it("shows a layer the browser has stopped sending as having no size", () => {
    const reading = readMedia([{ id: "OF", type: "outbound-rtp", kind: "video", rid: "f" }]);
    expect(describeOutboundLayers("call media", reading.outboundVideoLayers)).toEqual([
      "call media send video mode=- size=- fps=- limited_by=- rid=f",
    ]);
  });

  it("keeps the line of a single encoding as it was, and says nothing without a camera", () => {
    const single = readMedia([
      { id: "OT", type: "outbound-rtp", kind: "video", scalabilityMode: "L1T3", frameWidth: 640, frameHeight: 480, framesPerSecond: 30, qualityLimitationReason: "none" },
    ]);
    expect(describeOutboundLayers("call media", single.outboundVideoLayers)).toEqual([
      "call media send video mode=L1T3 size=640x480 fps=30 limited_by=none",
    ]);
    expect(describeOutboundLayers("call media", readMedia([audio()]).outboundVideoLayers)).toEqual([]);
  });

  it("sums the sender line across the layers", () => {
    const now = readSender(layers);
    expect(now).not.toBeNull();
    if (now === null) return;
    expect(now.framesEncoded).toBe(550);
    expect(now.packetsSent).toBe(3000);
    expect(now.targetBitrate).toBe(1_250_000);
    expect(now.remoteFractionLost).toBeCloseTo(0.4);
    expect(now.remoteRttMs).toBeCloseTo(50);
    const line = describeSender("call media interval", null, now);
    expect(line).toContain("sender encoded=550 pkts=3000 rtx=43 nack_in=11 pli_in=3 fir_in=0 huge=0");
    expect(line).toContain("target_kbps=1250");
    expect(line).toContain("remote_lost=40%");
    expect(line).toContain("remote_rtt_ms=50");
    expect(line).toContain("video=video/VP8");
  });
});

describe("the sender line", () => {
  const entries = (over: Record<string, unknown> = {}): Record<string, unknown>[] => [
    { id: "O", type: "outbound-rtp", kind: "video", codecId: "CV", framesEncoded: 500, packetsSent: 2000, targetBitrate: 900_000, ...over },
    { id: "OA", type: "outbound-rtp", kind: "audio", codecId: "CA" },
    { id: "CV", type: "codec", mimeType: "video/VP8" },
    { id: "CA", type: "codec", mimeType: "audio/red" },
    { id: "R", type: "remote-inbound-rtp", kind: "video", fractionLost: 0.25, roundTripTime: 0.04 },
    { id: "P", type: "candidate-pair", nominated: true, availableOutgoingBitrate: 1_200_000 },
  ];

  it("is absent without a camera sender", () => {
    expect(readSender([{ id: "A", type: "outbound-rtp", kind: "audio" }])).toBeNull();
  });

  it("reports what the far end says and what the browser estimates", () => {
    const before = readSender(entries());
    const now = readSender(entries({ framesEncoded: 515, packetsSent: 2100 }));
    expect(before).not.toBeNull();
    expect(now).not.toBeNull();
    if (before === null || now === null) return;
    const line = describeSender("call media interval", before, now);
    expect(line).toContain("encoded=15");
    expect(line).toContain("pkts=100");
    expect(line).toContain("target_kbps=900");
    expect(line).toContain("estimate_kbps=1200");
    expect(line).toContain("remote_lost=25%");
    expect(line).toContain("remote_rtt_ms=40");
    expect(line).toContain("audio=audio/red video=video/VP8");
    expect(line).toMatch(/^[\x20-\x7e]+$/);
  });
});

describe("which layer a video receiver carries", () => {
  it("is light for a companion's track, main for any other, unknown without a track id", () => {
    const r = readMedia(
      [
        video({ id: "V1", trackIdentifier: "track-main" }),
        video({ id: "V2", trackIdentifier: "track-light" }),
        video({ id: "V3" }),
        audio({ trackIdentifier: "track-audio" }),
      ],
      new Set(["track-light"]),
    );
    expect(r.inbound.get("V1")?.layer).toBe("main");
    expect(r.inbound.get("V2")?.layer).toBe("light");
    expect(r.inbound.get("V3")?.layer).toBe("-");
    expect(r.inbound.get("IT01A")?.layer).toBeNull();
    // With no companion known every identified video receiver is main.
    expect(readMedia([video({ trackIdentifier: "track-light" })]).inbound.get("IT01V")?.layer).toBe("main");
  });

  it("is the last field of a video line, and the other fields are unchanged", () => {
    const r = readMedia(
      [
        video({ id: "V1", trackIdentifier: "track-main" }),
        video({ id: "V2", trackIdentifier: "track-light" }),
        video({ id: "V3" }),
        audio(),
      ],
      new Set(["track-light"]),
    );
    expect(describeDeltas("call media interval", deltaOf(null, r))).toEqual([
      "call media interval video#1 pkts=1000 lost=10 nack=4 jb_ms=50 pli=1 freezes=2 freeze_s=1.50 decoded=300 dropped=3 layer=main",
      "call media interval video#2 pkts=1000 lost=10 nack=4 jb_ms=50 pli=1 freezes=2 freeze_s=1.50 decoded=300 dropped=3 layer=light",
      "call media interval video#3 pkts=1000 lost=10 nack=4 jb_ms=50 pli=1 freezes=2 freeze_s=1.50 decoded=300 dropped=3 layer=-",
      "call media interval audio#4 pkts=500 lost=5 nack=0 jb_ms=0 concealed=1.0%",
    ]);
  });
});

describe("what a remote tile showed", () => {
  it("is one exact line per peer", () => {
    expect(
      describeShown("call media interval", "peerX", {
        lightSwitches: 2,
        lightMs: 2314.6,
        stallMaxMs: 187.4,
        stallTotalMs: 361.5,
      }),
    ).toBe(
      "call media interval shown peer=peerX light_switches=2 light_ms=2315 stall_max_ms=187 stall_total_ms=362",
    );
  });

  it("lists every peer, for the interval and for the total", () => {
    const byPeer = new Map([
      ["a1", { lightSwitches: 0, lightMs: 0, stallMaxMs: 0, stallTotalMs: 0 }],
      ["b2", { lightSwitches: 1, lightMs: 1200, stallMaxMs: 950, stallTotalMs: 1100 }],
    ]);
    expect(describeShownPeers("call media total", byPeer)).toEqual([
      "call media total shown peer=a1 light_switches=0 light_ms=0 stall_max_ms=0 stall_total_ms=0",
      "call media total shown peer=b2 light_switches=1 light_ms=1200 stall_max_ms=950 stall_total_ms=1100",
    ]);
    expect(describeShownPeers("call media total", new Map())).toEqual([]);
  });
});
