// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  deltaOf,
  describeDeltas,
  describeOutbound,
  describeOutboundSenders,
  describeSender,
  describeGivenChanges,
  describeShown,
  describeShownPeers,
  InboundCounterLog,
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
  jitterBufferTargetDelay: 30,
  jitterBufferMinimumDelay: 6,
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

  it("end the camera sender's line with the ceiling its encoding holds, when given one", () => {
    const out = readMedia([{ id: "OT", type: "outbound-rtp", kind: "video" }]).outboundVideo;
    // Not given: the line is as it was.
    expect(describeOutbound("call media", out)).not.toContain("ceiling=");
    expect(describeOutbound("call media", out, undefined, 1_200_000)).toMatch(/ sender=main ceiling=1200$/);
    expect(describeOutbound("call media", out, undefined, null)).toMatch(/ sender=main ceiling=-$/);
    expect(describeOutbound("call media", out, { needed: true, active: true }, 483_000)).toMatch(
      / sender=main needed=yes active=yes ceiling=483$/,
    );
    const lines = describeOutboundSenders(
      "call media",
      [],
      () => ({ needed: null, active: true }),
      ["main", "light"],
      (kind) => (kind === "light" ? 150_000 : null),
    );
    expect(lines).toEqual([
      "call media send video mode=- size=- fps=- limited_by=- sender=main needed=- active=yes ceiling=-",
      "call media send video mode=- size=- fps=- limited_by=- sender=light needed=- active=yes ceiling=150",
    ]);
  });
});

describe("the camera's two senders", () => {
  // The report's order is the browser's: here the light sender comes first.
  const senders: Record<string, unknown>[] = [
    {
      id: "OL", type: "outbound-rtp", kind: "video", codecId: "CL",
      frameWidth: 320, frameHeight: 180, framesPerSecond: 30, qualityLimitationReason: "none",
      framesEncoded: 300, packetsSent: 400, retransmittedPacketsSent: 3, nackCount: 2, pliCount: 1,
      targetBitrate: 150_000,
    },
    {
      id: "OM", type: "outbound-rtp", kind: "video", codecId: "CV", scalabilityMode: "L1T3",
      frameWidth: 1280, frameHeight: 720, framesPerSecond: 24, qualityLimitationReason: "bandwidth",
      framesEncoded: 250, packetsSent: 2600, retransmittedPacketsSent: 40, nackCount: 9, pliCount: 2,
      targetBitrate: 1_100_000,
    },
    { id: "CV", type: "codec", mimeType: "video/VP8" },
    { id: "CL", type: "codec", mimeType: "video/H264" },
    { id: "RL", type: "remote-inbound-rtp", kind: "video", fractionLost: 0.02, roundTripTime: 0.03 },
    { id: "RM", type: "remote-inbound-rtp", kind: "video", fractionLost: 0.4, roundTripTime: 0.05 },
  ];
  const lightIds = new Set(["OL"]);

  it("get one line each, the main one first, named sender=main and sender=light", () => {
    const reading = readMedia(senders, new Set(), lightIds);
    expect(reading.outboundVideoSenders.map((s) => s.sender)).toEqual(["main", "light"]);
    expect(reading.outboundVideo?.sender).toBe("main");
    expect(reading.outboundVideo?.frameWidth).toBe(1280);
    const lines = describeOutboundSenders("call media interval", reading.outboundVideoSenders);
    expect(lines).toEqual([
      "call media interval send video mode=L1T3 size=1280x720 fps=24 limited_by=bandwidth sender=main",
      "call media interval send video mode=- size=320x180 fps=30 limited_by=none sender=light",
    ]);
    for (const line of lines) expect(line).not.toContain("rid=");
  });

  it("shows a sender the browser sends nothing on as having no size", () => {
    const reading = readMedia([{ id: "OL", type: "outbound-rtp", kind: "video" }], new Set(), lightIds);
    expect(describeOutboundSenders("call media", reading.outboundVideoSenders)).toEqual([
      "call media send video mode=- size=- fps=- limited_by=- sender=light",
    ]);
    expect(reading.outboundVideo).toBeNull();
  });

  it("names the one sender main without a light one, and says nothing without a camera", () => {
    const single = readMedia([
      { id: "OT", type: "outbound-rtp", kind: "video", scalabilityMode: "L1T3", frameWidth: 640, frameHeight: 480, framesPerSecond: 30, qualityLimitationReason: "none" },
    ]);
    expect(describeOutboundSenders("call media", single.outboundVideoSenders)).toEqual([
      "call media send video mode=L1T3 size=640x480 fps=30 limited_by=none sender=main",
    ]);
    expect(describeOutboundSenders("call media", readMedia([audio()]).outboundVideoSenders)).toEqual([]);
  });

  it("are summed in the sender line, which names the main sender's codec", () => {
    const now = readSender(senders, lightIds);
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
      "call media interval video#1 pkts=1000 lost=10 nack=4 jb_ms=50 jb_target_ms=100 jb_net_ms=20 pli=1 freezes=2 freeze_s=1.50 decoded=300 dropped=3 layer=main",
      "call media interval video#2 pkts=1000 lost=10 nack=4 jb_ms=50 jb_target_ms=100 jb_net_ms=20 pli=1 freezes=2 freeze_s=1.50 decoded=300 dropped=3 layer=light",
      "call media interval video#3 pkts=1000 lost=10 nack=4 jb_ms=50 jb_target_ms=100 jb_net_ms=20 pli=1 freezes=2 freeze_s=1.50 decoded=300 dropped=3 layer=-",
      "call media interval audio#4 pkts=500 lost=5 nack=0 jb_ms=0 jb_target_ms=- jb_net_ms=- concealed=1.0%",
    ]);
  });
});

describe("what a remote tile showed", () => {
  it("is one exact line per peer", () => {
    expect(
      describeShown(
        "call media interval",
        "peerX",
        { lightSwitches: 2, lightMs: 2314.6, stallMaxMs: 187.4, stallTotalMs: 361.5 },
        "middle",
      ),
    ).toBe(
      "call media interval shown peer=peerX want=middle given=- light_switches=2 light_ms=2315 stall_max_ms=187 stall_total_ms=362",
    );
  });

  it("adds what the tile observed on its two elements, when it is given", () => {
    const shown = { lightSwitches: 0, lightMs: 2000, stallMaxMs: 190, stallTotalMs: 720 };
    // The rule keeps the light copy while the heavy picture plays.
    expect(
      describeShown("call media interval", "peerX", shown, "middle", "middle", {
        heavy: "yes",
        heavyFrames: 57,
        heavyGapMaxMs: 203.4,
        lightFrames: 56,
        shownNow: "light",
      }),
    ).toBe(
      "call media interval shown peer=peerX want=middle given=middle light_switches=0 light_ms=2000" +
        " stall_max_ms=190 stall_total_ms=720 now=light heavy=yes heavy_frames=57 heavy_gap_max_ms=203 light_frames=56",
    );
    // The heavy picture has never produced a frame.
    expect(
      describeShown("l", "p", shown, "main", "main", {
        heavy: "never",
        heavyFrames: 0,
        heavyGapMaxMs: 0,
        lightFrames: 60,
        shownNow: "light",
      }),
    ).toContain(" now=light heavy=never heavy_frames=0 heavy_gap_max_ms=0 light_frames=60");
    const byPeer = new Map([["a1", shown]]);
    expect(
      describeShownPeers(
        "l",
        byPeer,
        () => "main",
        () => null,
        () => undefined,
      )[0],
    ).not.toContain("heavy=");
  });

  it("lists every peer, for the interval and for the total", () => {
    const byPeer = new Map([
      ["a1", { lightSwitches: 0, lightMs: 0, stallMaxMs: 0, stallTotalMs: 0 }],
      ["b2", { lightSwitches: 1, lightMs: 1200, stallMaxMs: 950, stallTotalMs: 1100 }],
    ]);
    const wantOf = (peerId: string): "main" | "light" => (peerId === "a1" ? "main" : "light");
    expect(describeShownPeers("call media total", byPeer, wantOf)).toEqual([
      "call media total shown peer=a1 want=main given=- light_switches=0 light_ms=0 stall_max_ms=0 stall_total_ms=0",
      "call media total shown peer=b2 want=light given=- light_switches=1 light_ms=1200 stall_max_ms=950 stall_total_ms=1100",
    ]);
    const givenOf = (peerId: string): "middle" | null => (peerId === "a1" ? "middle" : null);
    expect(describeShownPeers("call media total", byPeer, wantOf, givenOf)).toEqual([
      "call media total shown peer=a1 want=main given=middle light_switches=0 light_ms=0 stall_max_ms=0 stall_total_ms=0",
      "call media total shown peer=b2 want=light given=- light_switches=1 light_ms=1200 stall_max_ms=950 stall_total_ms=1100",
    ]);
    expect(describeShownPeers("call media total", new Map(), wantOf)).toEqual([]);
  });

  it("says given= beside want=: each of the three names, and - when the SFU has not said", () => {
    const none = { lightSwitches: 0, lightMs: 0, stallMaxMs: 0, stallTotalMs: 0 };
    for (const given of ["main", "middle", "light"] as const) {
      expect(describeShown("l", "p", none, "main", given)).toContain(` want=main given=${given} `);
    }
    expect(describeShown("l", "p", none, "main", null)).toContain(" want=main given=- ");
    expect(describeShown("l", "p", none, "main")).toContain(" want=main given=- ");
  });

  it("says in one line whose shown picture a video-given frame changed, and nothing when nobody's", () => {
    expect(describeGivenChanges("call video given", [])).toBeNull();
    expect(
      describeGivenChanges("call video given", [{ peer: "a", wanted: "main", given: "middle" }]),
    ).toBe("call video given peer=a want=main given=middle");
    expect(
      describeGivenChanges("call video given", [
        { peer: "a", wanted: "main", given: "light" },
        { peer: "b", wanted: "middle", given: null },
      ]),
    ).toBe("call video given peer=a want=main given=light | peer=b want=middle given=-");
  });

  it("says want= and given= right before light_switches, and keeps every other field's name", () => {
    const line = describeShown(
      "call media interval",
      "p",
      { lightSwitches: 0, lightMs: 0, stallMaxMs: 0, stallTotalMs: 0 },
      "light",
    );
    expect(line.split(" ").slice(3)).toEqual([
      "shown",
      "peer=p",
      "want=light",
      "given=-",
      "light_switches=0",
      "light_ms=0",
      "stall_max_ms=0",
      "stall_total_ms=0",
    ]);
  });
});

describe("the middle sender and the middle track in the counters", () => {
  const out = (id: string, width: number): Record<string, unknown> => ({
    id, type: "outbound-rtp", kind: "video", codecId: id === "OM" ? "CV" : "CX",
    frameWidth: width, frameHeight: (width * 9) / 16, framesPerSecond: 30,
    framesEncoded: 100, packetsSent: 200, targetBitrate: 100_000,
  });
  // The browser's order, not ours.
  const entries = [
    out("OL", 320),
    out("OD", 640),
    out("OM", 1280),
    { id: "CV", type: "codec", mimeType: "video/VP8" },
    { id: "CX", type: "codec", mimeType: "video/H264" },
  ];

  it("gives the middle sender its own send line, between main and light", () => {
    const reading = readMedia(entries, new Set(), new Set(["OL"]), new Set(), new Set(["OD"]));
    expect(reading.outboundVideoSenders.map((s) => s.sender)).toEqual(["main", "middle", "light"]);
    expect(reading.outboundVideo?.frameWidth).toBe(1280);
    expect(describeOutboundSenders("call media interval", reading.outboundVideoSenders)).toEqual([
      "call media interval send video mode=- size=1280x720 fps=30 limited_by=- sender=main",
      "call media interval send video mode=- size=640x360 fps=30 limited_by=- sender=middle",
      "call media interval send video mode=- size=320x180 fps=30 limited_by=- sender=light",
    ]);
  });

  it("shows a middle sender that is not sending as having no size", () => {
    const reading = readMedia(
      [{ id: "OD", type: "outbound-rtp", kind: "video" }],
      new Set(),
      new Set(),
      new Set(),
      new Set(["OD"]),
    );
    expect(describeOutboundSenders("call media", reading.outboundVideoSenders)).toEqual([
      "call media send video mode=- size=- fps=- limited_by=- sender=middle",
    ]);
  });

  it("sums all three in the sender line and still names the main sender's codec", () => {
    const now = readSender(entries, new Set(["OL"]), new Set(["OD"]));
    expect(now?.framesEncoded).toBe(300);
    expect(now?.packetsSent).toBe(600);
    expect(now?.targetBitrate).toBe(300_000);
    expect(now?.videoCodec).toBe("video/VP8");
  });

  it("names a middle track's receiver layer=middle", () => {
    const r = readMedia(
      [
        video({ id: "V1", trackIdentifier: "track-main" }),
        video({ id: "V2", trackIdentifier: "track-mid" }),
        video({ id: "V3", trackIdentifier: "track-light" }),
      ],
      new Set(["track-light"]),
      new Set(),
      new Set(["track-mid"]),
    );
    expect([...r.inbound.values()].map((i) => i.layer)).toEqual(["main", "middle", "light"]);
    const lines = describeDeltas("call media interval", deltaOf(null, r));
    expect(lines.map((l) => l.slice(l.lastIndexOf(" ") + 1))).toEqual([
      "layer=main",
      "layer=middle",
      "layer=light",
    ]);
    // Every other field of a video line is where it was.
    expect(lines[1]).toBe(
      "call media interval video#2 pkts=1000 lost=10 nack=4 jb_ms=50 jb_target_ms=100 jb_net_ms=20 pli=1 freezes=2 freeze_s=1.50 decoded=300 dropped=3 layer=middle",
    );
  });
});

describe("InboundCounterLog (the one-to-one call)", () => {
  it("logs each interval's change and the whole call once at the end", () => {
    const log = new InboundCounterLog();
    const first = log.interval([audio(), video()]);
    expect(first).toEqual([
      "call media interval audio#1 pkts=500 lost=5 nack=0 jb_ms=0 jb_target_ms=- jb_net_ms=- concealed=1.0%",
      "call media interval video#2 pkts=1000 lost=10 nack=4 jb_ms=50 jb_target_ms=100 jb_net_ms=20 pli=1 freezes=2 freeze_s=1.50 decoded=300 dropped=3 layer=-",
    ]);
    const second = log.interval([
      audio({ packetsReceived: 600, concealedSamples: 9_600, totalSamplesReceived: 576_000 }),
      video({ packetsReceived: 1100 }),
    ]);
    expect(second[0]).toBe(
      "call media interval audio#1 pkts=100 lost=0 nack=0 jb_ms=- jb_target_ms=- jb_net_ms=- concealed=5.0%",
    );
    expect(second[1]).toMatch(/^call media interval video#2 pkts=100 /);
    const total = log.total();
    expect(total[0]).toBe(
      "call media total audio#1 pkts=600 lost=5 nack=0 jb_ms=0 jb_target_ms=- jb_net_ms=- concealed=1.7%",
    );
    expect(total).toHaveLength(2);
    expect(log.total()).toEqual([]);
  });

  it("says nothing at the end of a call that was never read", () => {
    expect(new InboundCounterLog().total()).toEqual([]);
  });

  it("ignores everything but the receivers", () => {
    const log = new InboundCounterLog();
    expect(
      log.interval([
        { id: "CP1", type: "candidate-pair", bytesReceived: 9 },
        { id: "OT1", type: "outbound-rtp", kind: "video", bytesSent: 9 },
      ]),
    ).toEqual([]);
  });
});
