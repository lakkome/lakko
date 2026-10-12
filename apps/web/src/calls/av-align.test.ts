// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The web side of the audio/video alignment rule. The pure parts run over
 * plain objects; the rule itself is the shared one in `crates/lakko-ui-rules`
 * (`av_align`), reached through the real wasm, so what is asserted of a
 * target is what the rule answers across the boundary this client uses.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

import {
  AV_ALIGN_DEFAULT_ON,
  AV_ALIGN_STORAGE_KEY,
  AV_READING_FALLBACK_MS,
  CallAvAlign,
  NO_TRACK_INPUT,
  avAlignEnabled,
  avReadingIntervalMs,
  browserHasJitterTarget,
  describeCapability,
  describeSync,
  hasJitterTarget,
  inboundSamples,
  probeJitterTarget,
  receiverSample,
  sharedAvAlignRules,
  shownTrack,
  trackInput,
  type AvAlignRule,
  type AvAlignRules,
  type AvVideoRole,
  type InboundSample,
  type JitterTargetReceiver,
  type ShownFacts,
} from "./av-align";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

// ----- fakes -----------------------------------------------------------------

/** A receiver of a browser that has the setter: the property lives on the
 *  prototype, as on `RTCRtpReceiver`, and records every write. */
class SettableReceiver {
  private value: number | null = null;
  readonly writes: (number | null)[] = [];
  get jitterBufferTarget(): number | null {
    return this.value;
  }
  set jitterBufferTarget(v: number | null) {
    this.writes.push(v);
    this.value = v;
  }
}

/** A receiver whose setter accepts a write and keeps nothing. */
class DeafReceiver {
  get jitterBufferTarget(): number | null {
    return null;
  }
  set jitterBufferTarget(_v: number | null) {
    // ignored
  }
}

/** A receiver whose setter throws from the `after`-th write on. */
class ThrowingReceiver {
  private value: number | null = null;
  writes = 0;
  constructor(private readonly after: number) {}
  get jitterBufferTarget(): number | null {
    return this.value;
  }
  set jitterBufferTarget(v: number | null) {
    this.writes += 1;
    if (this.writes > this.after) throw new Error("no");
    this.value = v;
  }
}

/** A stream whose jitter buffer holds `delayMs` and receives 50 packets and
 *  emits 50 frames a second; `at(second)` is its cumulative sample. */
function steady(delayMs: number, networkMs = 0): (second: number) => InboundSample {
  return (second) => ({
    packetsReceived: 50 * second,
    jitterBufferDelay: (delayMs / 1000) * 50 * second,
    jitterBufferEmittedCount: 50 * second,
    jitterBufferMinimumDelay: (networkMs / 1000) * 50 * second,
  });
}

function lines(log: string[]): (line: string) => void {
  return (line) => {
    log.push(line);
  };
}

// ----- the switch --------------------------------------------------------------

describe("the switch", () => {
  const stored = (value: string | null) => ({
    getItem: (key: string): string | null => (key === AV_ALIGN_STORAGE_KEY ? value : null),
  });

  it("is on by default", () => {
    expect(AV_ALIGN_DEFAULT_ON).toBe(true);
    expect(avAlignEnabled(null)).toBe(true);
    expect(avAlignEnabled(stored(null))).toBe(true);
    expect(avAlignEnabled(stored("something"))).toBe(true);
  });

  it("is switched off and on by the stored word", () => {
    for (const word of ["off", "OFF", " 0 ", "false", "no"]) {
      expect(avAlignEnabled(stored(word)), word).toBe(false);
    }
    for (const word of ["on", "1", "true", "yes"]) {
      expect(avAlignEnabled(stored(word)), word).toBe(true);
    }
  });

  it("keeps the default when the storage cannot be read", () => {
    expect(
      avAlignEnabled({
        getItem: () => {
          throw new Error("denied");
        },
      }),
    ).toBe(true);
  });

  it("reads the key it documents", () => {
    expect(AV_ALIGN_STORAGE_KEY).toBe("lakko.call.av-align");
  });
});

// ----- capability ----------------------------------------------------------------

describe("capability", () => {
  it("asks for the property's presence, on the object or its prototype", () => {
    expect(hasJitterTarget(new SettableReceiver())).toBe(true);
    expect(hasJitterTarget(SettableReceiver.prototype)).toBe(true);
    expect(hasJitterTarget({})).toBe(false);
    expect(hasJitterTarget(null)).toBe(false);
    expect(hasJitterTarget(undefined)).toBe(false);
  });

  it("finds no setter in a runtime without RTCRtpReceiver", () => {
    // The node test runtime has none.
    expect(browserHasJitterTarget()).toBe(false);
  });

  it("takes a receiver whose write reads back, and leaves it as it was", () => {
    const receiver = new SettableReceiver();
    expect(probeJitterTarget(receiver)).toBe(true);
    expect(receiver.writes).toEqual([0, null]);
    expect(receiver.jitterBufferTarget).toBeNull();
  });

  it("does not take a plain object that would accept any property", () => {
    const receiver: JitterTargetReceiver = {};
    expect(probeJitterTarget(receiver)).toBe(false);
    expect("jitterBufferTarget" in receiver).toBe(false);
  });

  it("does not take a setter that keeps nothing, or one that throws", () => {
    expect(probeJitterTarget(new DeafReceiver())).toBe(false);
    expect(probeJitterTarget(new ThrowingReceiver(0))).toBe(false);
  });

  it("does not write to a receiver that already holds a target", () => {
    const receiver = new SettableReceiver();
    receiver.jitterBufferTarget = 300;
    expect(probeJitterTarget(receiver)).toBe(true);
    expect(receiver.writes).toEqual([300]);
  });
});

// ----- statistics ------------------------------------------------------------------

describe("statistics as the rule's inputs", () => {
  it("keeps the inbound entries by track and counts those that name none", () => {
    const { byTrack, unidentified } = inboundSamples([
      { type: "inbound-rtp", kind: "audio", trackIdentifier: "a", packetsReceived: 10, jitterBufferDelay: 2, jitterBufferEmittedCount: 4 },
      { type: "inbound-rtp", kind: "video", packetsReceived: 5 },
      { type: "outbound-rtp", kind: "video", trackIdentifier: "x", packetsReceived: 99 },
      { type: "candidate-pair" },
    ]);
    expect(unidentified).toBe(1);
    expect([...byTrack.keys()]).toEqual(["a"]);
    expect(byTrack.get("a")).toEqual({
      packetsReceived: 10,
      jitterBufferDelay: 2,
      jitterBufferEmittedCount: 4,
      jitterBufferMinimumDelay: 0,
    });
  });

  it("reads one receiver's own report", () => {
    expect(receiverSample([{ type: "codec" }])).toBeNull();
    expect(
      receiverSample([{ type: "codec" }, { type: "inbound-rtp", packetsReceived: 7 }])?.packetsReceived,
    ).toBe(7);
  });

  it("averages the interval, not the whole call", () => {
    const prev: InboundSample = { packetsReceived: 100, jitterBufferDelay: 10, jitterBufferEmittedCount: 100, jitterBufferMinimumDelay: 5 };
    const cur: InboundSample = { packetsReceived: 150, jitterBufferDelay: 31, jitterBufferEmittedCount: 150, jitterBufferMinimumDelay: 10 };
    // 21 s over 50 emitted = 420 ms; 5 s over 50 = 100 ms.
    expect(trackInput(prev, cur)).toEqual({ flowing: true, delayMs: 420, networkMs: 100 });
  });

  it("leaves unmeasured what was not measured", () => {
    const prev: InboundSample = { packetsReceived: 100, jitterBufferDelay: 10, jitterBufferEmittedCount: 100, jitterBufferMinimumDelay: 0 };
    // Packets, nothing emitted.
    expect(trackInput(prev, { ...prev, packetsReceived: 120 })).toEqual({
      flowing: true,
      delayMs: undefined,
      networkMs: undefined,
    });
    // Emitted, no network figure from this browser.
    expect(
      trackInput(prev, { packetsReceived: 150, jitterBufferDelay: 20, jitterBufferEmittedCount: 150, jitterBufferMinimumDelay: 0 }),
    ).toEqual({ flowing: true, delayMs: 200, networkMs: undefined });
    // Nothing arrived.
    expect(trackInput(prev, prev)).toEqual(NO_TRACK_INPUT);
    // No entry at all.
    expect(trackInput(prev, null)).toEqual(NO_TRACK_INPUT);
  });

  it("measures from zero with no earlier sample or counters that went back", () => {
    const cur: InboundSample = { packetsReceived: 10, jitterBufferDelay: 3, jitterBufferEmittedCount: 10, jitterBufferMinimumDelay: 0 };
    expect(trackInput(null, cur)).toEqual({ flowing: true, delayMs: 300, networkMs: undefined });
    const before: InboundSample = { packetsReceived: 500, jitterBufferDelay: 90, jitterBufferEmittedCount: 500, jitterBufferMinimumDelay: 0 };
    expect(trackInput(before, cur)).toEqual({ flowing: true, delayMs: 300, networkMs: undefined });
  });
});

// ----- what is shown ---------------------------------------------------------------

describe("which track a tile draws", () => {
  const facts = (over: Partial<ShownFacts>): ShownFacts => ({
    hasTile: true,
    wanted: "main",
    given: null,
    hasMiddle: true,
    hasLight: true,
    lightOnTop: false,
    ...over,
  });

  it("is none without a tile", () => {
    expect(shownTrack(facts({ hasTile: false }))).toBeNull();
  });

  it("is the heavy picture asked for", () => {
    expect(shownTrack(facts({}))).toBe("main");
    expect(shownTrack(facts({ wanted: "middle" }))).toBe("middle");
    expect(shownTrack(facts({ wanted: "light" }))).toBe("light");
  });

  it("is the lower of what is wanted and what the SFU gives", () => {
    expect(shownTrack(facts({ given: "middle" }))).toBe("middle");
    expect(shownTrack(facts({ given: "light" }))).toBe("light");
    expect(shownTrack(facts({ wanted: "middle", given: "main" }))).toBe("middle");
  });

  it("is the main track for a middle picture of a person with no middle track", () => {
    expect(shownTrack(facts({ wanted: "middle", hasMiddle: false }))).toBe("main");
  });

  it("is the light copy while that is on top", () => {
    expect(shownTrack(facts({ lightOnTop: true }))).toBe("light");
    expect(shownTrack(facts({ wanted: "middle", lightOnTop: true }))).toBe("light");
    // No light track: nothing can be on top of the heavy picture.
    expect(shownTrack(facts({ lightOnTop: true, hasLight: false }))).toBe("main");
  });

  it("is a heavy picture when light is given of a person with no light track", () => {
    // The tile's own rule (`shownQuality`): what was wanted, never nothing.
    expect(shownTrack(facts({ given: "light", hasLight: false }))).toBe("main");
    expect(shownTrack(facts({ wanted: "light", hasLight: false }))).toBe("middle");
    expect(shownTrack(facts({ wanted: "light", hasLight: false, hasMiddle: false }))).toBe("main");
  });
});

// ----- the lines ---------------------------------------------------------------------

describe("the log lines", () => {
  it("say one person's sound and pictures", () => {
    expect(
      describeSync("call media interval", "p7", {
        audio: { delayMs: 420, askedMs: 0, reason: "free" },
        video: [
          { role: "main", delayMs: 431, askedMs: 0, skewMs: 11, reason: "engine-synced" },
          { role: "light", delayMs: 418, askedMs: 420, skewMs: -2, reason: "aligned" },
        ],
      }),
    ).toBe(
      "call media interval sync peer=p7 audio=420/ask=0/free" +
        " main=431/ask=0/skew=11/engine-synced light=418/ask=420/skew=-2/aligned",
    );
  });

  it("write a dash for what was not measured", () => {
    expect(
      describeSync("call media interval", "p7", {
        audio: { delayMs: undefined, askedMs: 0, reason: "not-flowing" },
        video: [{ role: "middle", delayMs: undefined, askedMs: 300, skewMs: undefined, reason: "no-sound" }],
      }),
    ).toBe("call media interval sync peer=p7 audio=-/ask=0/not-flowing middle=-/ask=300/skew=-/no-sound");
  });

  it("say what this browser can do", () => {
    expect(describeCapability("call media", "picture-waits-only", false, true, true, "receiver", 1000)).toBe(
      "call media sync capability=picture-waits-only audio_setter=no video_setter=yes" +
        " enabled=yes probe=receiver interval_ms=1000",
    );
  });
});

// ----- the shared rule across the boundary --------------------------------------------

describe("the shared rule", () => {
  it("is read once a second", () => {
    expect(sharedAvAlignRules.readingMs()).toBe(1000);
    expect(avReadingIntervalMs()).toBe(1000);
  });

  it("falls back to a second when the rule cannot say", () => {
    const broken = (readingMs: () => number): AvAlignRules => ({ ...sharedAvAlignRules, readingMs });
    expect(AV_READING_FALLBACK_MS).toBe(1000);
    expect(
      avReadingIntervalMs(
        broken(() => {
          throw new Error("not loaded");
        }),
      ),
    ).toBe(1000);
    expect(avReadingIntervalMs(broken(() => 0))).toBe(1000);
    expect(avReadingIntervalMs(broken(() => 60_000))).toBe(1000);
    expect(avReadingIntervalMs(broken(() => 500))).toBe(500);
  });

  it("names its capabilities and reasons", () => {
    expect(sharedAvAlignRules.capabilityName(true, true)).toBe("full");
    expect(sharedAvAlignRules.capabilityName(false, true)).toBe("picture-waits-only");
    expect(sharedAvAlignRules.capabilityName(true, false)).toBe("sound-waits-only");
    expect(sharedAvAlignRules.capabilityName(false, false)).toBe("observe-only");
    expect(sharedAvAlignRules.audioReasonName(wasm.AvAudioReason.Free)).toBe("free");
    expect(sharedAvAlignRules.videoReasonName(wasm.AvVideoReason.EngineSynced)).toBe("engine-synced");
  });
});

// ----- one call ------------------------------------------------------------------------

/** A person with sound at `soundMs` and the given pictures, read for
 *  `seconds` readings one second apart; `shown` is the track a tile draws. */
function runCall(
  call: CallAvAlign<JitterTargetReceiver>,
  streams: Record<string, (second: number) => InboundSample>,
  shown: AvVideoRole | null,
  seconds: number,
  from = 1,
): void {
  for (let s = from; s < from + seconds; s += 1) {
    const byTrack = new Map<string, InboundSample>();
    for (const [trackId, at] of Object.entries(streams)) byTrack.set(trackId, at(s));
    call.read(s * 1000, byTrack, () => shown);
  }
}

describe("a call", () => {
  it("says at its start what the browser can do, from the prototype", () => {
    const log: string[] = [];
    new CallAvAlign({ enabled: true, prototypeSettable: true, info: lines(log) }).dispose();
    new CallAvAlign({ enabled: true, prototypeSettable: false, info: lines(log) }).dispose();
    expect(log).toEqual([
      "call media sync capability=full audio_setter=yes video_setter=yes enabled=yes probe=prototype interval_ms=1000",
      "call media sync capability=observe-only audio_setter=no video_setter=no enabled=yes probe=prototype interval_ms=1000",
    ]);
  });

  it("makes an early light picture wait for the sound and never writes to main", () => {
    const call = new CallAvAlign<JitterTargetReceiver>({ enabled: true, prototypeSettable: true });
    const audio = new SettableReceiver();
    const main = new SettableReceiver();
    const light = new SettableReceiver();
    call.setReceiver("p1", "audio", audio, "a");
    call.setReceiver("p1", "main", main, "m");
    call.setReceiver("p1", "light", light, "l");
    // The probe of the first audio and the first video receiver.
    expect(audio.writes).toEqual([0, null]);
    expect(main.writes).toEqual([0, null]);
    expect(light.writes).toEqual([]);

    runCall(call, { a: steady(450), m: steady(460), l: steady(290) }, "light", 1);
    // The light track starts at the sound's delay.
    expect(light.writes).toEqual([450]);
    expect(light.jitterBufferTarget).toBe(450);
    // Main is synchronised by the browser: observed only.
    expect(main.writes).toEqual([0, null]);
    // The sound is the reference: nothing is asked of it.
    expect(audio.writes).toEqual([0, null]);
    expect(call.lines("call media interval")).toEqual([
      "call media interval sync peer=p1 audio=450/ask=0/free" +
        " main=460/ask=0/skew=10/engine-synced light=290/ask=450/skew=-160/started",
    ]);

    // The receiver answers; the rule finds it aligned and writes no more.
    runCall(call, { a: steady(450), m: steady(460), l: steady(450) }, "light", 3, 2);
    expect(light.writes).toEqual([450]);
    expect(call.lines("x")[0]).toContain("light=450/ask=450/skew=0/aligned");
  });

  it("gives every person their own rule", () => {
    const call = new CallAvAlign<JitterTargetReceiver>({ enabled: true, prototypeSettable: true });
    const l1 = new SettableReceiver();
    const l2 = new SettableReceiver();
    call.setReceiver("p1", "audio", new SettableReceiver(), "a1");
    call.setReceiver("p1", "light", l1, "l1");
    call.setReceiver("p2", "audio", new SettableReceiver(), "a2");
    call.setReceiver("p2", "light", l2, "l2");
    runCall(call, { a1: steady(400), l1: steady(290), a2: steady(600), l2: steady(290) }, "light", 1);
    expect(l1.jitterBufferTarget).toBe(400);
    expect(l2.jitterBufferTarget).toBe(600);
    expect(call.lines("x").map((line) => line.split(" ")[2])).toEqual(["peer=p1", "peer=p2"]);
  });

  it("gives a target back as null", () => {
    const call = new CallAvAlign<JitterTargetReceiver>({ enabled: true, prototypeSettable: true });
    const light = new SettableReceiver();
    call.setReceiver("p1", "audio", new SettableReceiver(), "a");
    call.setReceiver("p1", "light", light, "l");
    runCall(call, { a: steady(450), l: steady(290) }, "light", 1);
    expect(light.jitterBufferTarget).toBe(450);
    // The sound is gone for good: the rule gives the picture's target back.
    runCall(call, { l: steady(450) }, "light", 8, 2);
    expect(light.jitterBufferTarget).toBeNull();
    expect(light.writes.at(-1)).toBeNull();
  });

  it("only observes with the switch off, and does not even probe", () => {
    const log: string[] = [];
    const call = new CallAvAlign<JitterTargetReceiver>({
      enabled: false,
      prototypeSettable: true,
      info: lines(log),
    });
    const audio = new SettableReceiver();
    const light = new SettableReceiver();
    call.setReceiver("p1", "audio", audio, "a");
    call.setReceiver("p1", "light", light, "l");
    runCall(call, { a: steady(450), l: steady(290) }, "light", 6);
    expect(audio.writes).toEqual([]);
    expect(light.writes).toEqual([]);
    expect(log).toEqual([
      "call media sync capability=full audio_setter=yes video_setter=yes enabled=no probe=prototype interval_ms=1000",
    ]);
    // The skew is still measured: that is the log to compare against.
    expect(call.lines("call media interval")).toEqual([
      "call media interval sync peer=p1 audio=450/ask=0/no-setter light=290/ask=0/skew=-160/no-setter",
    ]);
  });

  it("observes in a browser without the setter", () => {
    const call = new CallAvAlign<JitterTargetReceiver>({ enabled: true, prototypeSettable: false });
    const audio: JitterTargetReceiver = {};
    const light: JitterTargetReceiver = {};
    call.setReceiver("p1", "audio", audio, "a");
    call.setReceiver("p1", "light", light, "l");
    runCall(call, { a: steady(450), l: steady(290) }, "light", 4);
    expect("jitterBufferTarget" in audio).toBe(false);
    expect("jitterBufferTarget" in light).toBe(false);
    expect(call.lines("x")[0]).toContain("light=290/ask=0/skew=-160/no-setter");
  });

  it("is corrected by a real receiver when the prototype was wrong", () => {
    const log: string[] = [];
    const call = new CallAvAlign<JitterTargetReceiver>({
      enabled: true,
      prototypeSettable: true,
      info: lines(log),
    });
    // The property is there and keeps nothing: video cannot be set.
    const light = new DeafReceiver();
    call.setReceiver("p1", "audio", new SettableReceiver(), "a");
    call.setReceiver("p1", "light", light, "l");
    expect(call.browserSettable("audio")).toBe(true);
    expect(call.browserSettable("video")).toBe(false);
    expect(log.at(-1)).toBe(
      "call media sync capability=sound-waits-only audio_setter=yes video_setter=no enabled=yes probe=receiver interval_ms=1000",
    );
    runCall(call, { a: steady(450), l: steady(290) }, "light", 3);
    expect(call.lines("x")[0]).toContain("light=290/ask=0/skew=-160/no-setter");
  });

  it("stops asking a person's receivers of a kind once a write throws, warning once", () => {
    const warnings: string[] = [];
    const call = new CallAvAlign<JitterTargetReceiver>({
      enabled: true,
      prototypeSettable: true,
      warn: (message) => {
        warnings.push(message);
      },
    });
    // The probe's two writes pass; the rule's first write throws.
    const light = new ThrowingReceiver(2);
    call.setReceiver("p1", "audio", new SettableReceiver(), "a");
    call.setReceiver("p1", "light", light, "l");
    runCall(call, { a: steady(450), l: steady(290) }, "light", 6);
    expect(light.writes).toBe(3);
    expect(warnings).toEqual([
      "call media sync peer=p1 light: jitterBufferTarget could not be set; video targets of this person are no longer asked",
    ]);
    // The person is still measured, as one whose video cannot be set.
    expect(call.lines("x")[0]).toContain("light=290/ask=0/skew=-160/no-setter");
    // Someone else's receivers are still asked.
    const other = new SettableReceiver();
    call.setReceiver("p2", "audio", new SettableReceiver(), "a2");
    call.setReceiver("p2", "light", other, "l2");
    runCall(call, { a: steady(450), l: steady(290), a2: steady(500), l2: steady(290) }, "light", 1, 7);
    expect(other.jitterBufferTarget).toBe(500);
  });

  it("tells a replaced receiver its target again", () => {
    const call = new CallAvAlign<JitterTargetReceiver>({ enabled: true, prototypeSettable: true });
    const first = new SettableReceiver();
    call.setReceiver("p1", "audio", new SettableReceiver(), "a");
    call.setReceiver("p1", "light", first, "l");
    runCall(call, { a: steady(400), l: steady(400) }, "light", 3);
    expect(first.writes).toEqual([0, null, 400]);
    // The same receiver registered again changes nothing.
    call.setReceiver("p1", "light", first, "l");
    runCall(call, { a: steady(400), l: steady(400) }, "light", 1, 4);
    expect(first.writes).toEqual([0, null, 400]);
    // A new receiver object under the same role.
    const second = new SettableReceiver();
    call.setReceiver("p1", "light", second, "l-new");
    runCall(call, { a: steady(400), "l-new": steady(400) }, "light", 1, 5);
    expect(second.writes).toEqual([400]);
  });

  it("tells every receiver again after the call's receivers were replaced", () => {
    const call = new CallAvAlign<JitterTargetReceiver>({ enabled: true, prototypeSettable: true });
    const light = new SettableReceiver();
    call.setReceiver("p1", "audio", new SettableReceiver(), "a");
    call.setReceiver("p1", "light", light, "l");
    runCall(call, { a: steady(400), l: steady(400) }, "light", 2);
    call.receiversReplaced();
    runCall(call, { a: steady(400), l: steady(400) }, "light", 1, 3);
    expect(light.writes).toEqual([0, null, 400, 400]);
  });

  it("leaves an ended track alone and keeps a later one under the same role", () => {
    const call = new CallAvAlign<JitterTargetReceiver>({ enabled: true, prototypeSettable: true });
    const old = new SettableReceiver();
    const next = new SettableReceiver();
    call.setReceiver("p1", "audio", new SettableReceiver(), "a");
    call.setReceiver("p1", "light", old, "l");
    call.setReceiver("p1", "light", next, "l2");
    // The old track's `ended` arrives after the new one was registered.
    call.removeReceiver("p1", "light", old);
    expect(call.has("p1", "light")).toBe(true);
    call.removeReceiver("p1", "light", next);
    expect(call.has("p1", "light")).toBe(false);
    runCall(call, { a: steady(400) }, null, 2);
    expect(call.lines("x")).toEqual(["x sync peer=p1 audio=400/ask=0/free"]);
    expect(call.receivers().map((r) => r.role)).toEqual(["audio"]);
  });

  it("frees a person's rule when they leave and every rule when the call ends", () => {
    const freed: number[] = [];
    let built = 0;
    const counting: AvAlignRules = {
      ...sharedAvAlignRules,
      create: (audioSettable, videoSettable): AvAlignRule => {
        const id = (built += 1);
        const rule = sharedAvAlignRules.create(audioSettable, videoSettable);
        const free = rule.free.bind(rule);
        rule.free = (): void => {
          freed.push(id);
          free();
        };
        return rule;
      },
    };
    const call = new CallAvAlign<JitterTargetReceiver>({
      enabled: true,
      prototypeSettable: true,
      rules: counting,
    });
    for (const peer of ["p1", "p2", "p3"]) {
      call.setReceiver(peer, "audio", new SettableReceiver(), `a-${peer}`);
    }
    runCall(call, { "a-p1": steady(300), "a-p2": steady(300), "a-p3": steady(300) }, null, 1);
    expect(built).toBe(3);
    call.removePeer("p2");
    expect(freed).toEqual([2]);
    expect(call.lines("x").length).toBe(2);
    // Someone who joins mid-call gets one.
    call.setReceiver("p4", "audio", new SettableReceiver(), "a-p4");
    runCall(call, { "a-p1": steady(300), "a-p3": steady(300), "a-p4": steady(300) }, null, 1, 2);
    expect(built).toBe(4);
    call.dispose();
    expect(freed).toEqual([2, 1, 3, 4]);
    // Nothing is read, registered or built afterwards.
    call.setReceiver("p5", "audio", new SettableReceiver(), "a-p5");
    runCall(call, { "a-p5": steady(300) }, null, 1, 3);
    expect(built).toBe(4);
    expect(call.lines("x")).toEqual([]);
  });

  it("waits for the rule while the wasm module is not ready, then follows it", () => {
    let ready = false;
    const late: AvAlignRules = {
      ...sharedAvAlignRules,
      create: (a, v) => {
        if (!ready) throw new Error("not loaded");
        return sharedAvAlignRules.create(a, v);
      },
    };
    const call = new CallAvAlign<JitterTargetReceiver>({ enabled: true, prototypeSettable: true, rules: late });
    const light = new SettableReceiver();
    call.setReceiver("p1", "audio", new SettableReceiver(), "a");
    call.setReceiver("p1", "light", light, "l");
    runCall(call, { a: steady(450), l: steady(290) }, "light", 2);
    expect(call.lines("x")).toEqual([]);
    expect(light.writes).toEqual([0, null]);
    ready = true;
    runCall(call, { a: steady(450), l: steady(290) }, "light", 1, 3);
    expect(light.jitterBufferTarget).toBe(450);
    call.dispose();
  });
});
