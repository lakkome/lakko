// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `speaking-monitor.ts` over fake receivers, fake streams and a fake clock:
 * where each person's level is read from, what a mute does, and what the one
 * timer costs.
 */
import { describe, expect, it } from "vitest";

import type { LevelReceiver, LevelSource, LevelStat } from "./audio-level";
import { FLOOR_ONSET_MS, SPEAKING_LEVEL, type SpeakingSnapshot } from "./speaking";
import type { VoiceReport } from "./voice-diagnostics";
import {
  browserTaps,
  MAX_STATS_READS_PER_TICK,
  REPORT_EVERY_TICKS,
  SAMPLE_INTERVAL_MS,
  SpeakingMonitor,
  type StreamLevelTap,
  type VoiceSource,
} from "./speaking-monitor";

const LOUD = SPEAKING_LEVEL * 5;

/** A receiver on the synchronization-source path. */
class SourceReceiver implements LevelReceiver {
  public level = 0;
  private stamp = 0;
  public getSynchronizationSources(): readonly LevelSource[] {
    this.stamp += 20;
    return [{ audioLevel: this.level, timestamp: this.stamp }];
  }
  public async getStats(): Promise<{ forEach(cb: (s: LevelStat) => void): void }> {
    throw new Error("the source path never asks");
  }
}

/** A receiver on the statistics path: no level on its sources. */
class StatsReceiver implements LevelReceiver {
  public level = 0;
  public calls = 0;
  private energy = 0;
  private duration = 0;
  public getSynchronizationSources(): readonly LevelSource[] {
    return [{ timestamp: 1 }];
  }
  public async getStats(): Promise<{ forEach(cb: (s: LevelStat) => void): void }> {
    this.calls += 1;
    this.duration += 0.1;
    this.energy += this.level * this.level * 0.1;
    const stat: LevelStat = {
      type: "inbound-rtp",
      kind: "audio",
      totalAudioEnergy: this.energy,
      totalSamplesDuration: this.duration,
    };
    return { forEach: (cb) => cb(stat) };
  }
}

/** A receiver that offers no level at all. */
class MuteReceiver implements LevelReceiver {
  public async getStats(): Promise<{ forEach(cb: (s: LevelStat) => void): void }> {
    const stat: LevelStat = { type: "inbound-rtp", kind: "audio" };
    return { forEach: (cb) => cb(stat) };
  }
}

interface FakeStream {
  /** `null`: the tap cannot hear (its audio context is not running). */
  level: number | null;
  audio: number;
}
function stream(level: number | null = 0, audio = 1): MediaStream & FakeStream {
  const s = { level, audio, getAudioTracks: () => Array.from({ length: s.audio }) };
  return s as unknown as MediaStream & FakeStream;
}

interface Rig {
  readonly monitor: SpeakingMonitor;
  readonly snapshots: SpeakingSnapshot[];
  readonly paths: string[];
  readonly taps: { opened: number; closed: number; released: number };
  readonly timers: { started: number; stopped: number; running: boolean };
  readonly reports: { voices: readonly VoiceReport[]; floor: string | null }[];
  readonly gestures: { resumed: number };
  /** Run the timer for `ms`, letting statistics promises settle between ticks. */
  run(ms: number): Promise<SpeakingSnapshot>;
}

function rig(): Rig {
  let now = 0;
  let tick: (() => void) | null = null;
  const snapshots: SpeakingSnapshot[] = [];
  const paths: string[] = [];
  const taps = { opened: 0, closed: 0, released: 0 };
  const timers = { started: 0, stopped: 0, running: false };
  const reports: Rig["reports"] = [];
  const gestures = { resumed: 0 };
  const monitor = new SpeakingMonitor((s) => snapshots.push(s), {
    nowMs: () => now,
    every: (ms, run) => {
      expect(ms).toBe(SAMPLE_INTERVAL_MS);
      timers.started += 1;
      timers.running = true;
      tick = run;
      return () => {
        timers.stopped += 1;
        timers.running = false;
        tick = null;
      };
    },
    openTap: (s): StreamLevelTap => {
      taps.opened += 1;
      return {
        read: () => (s as unknown as FakeStream).level,
        close: () => {
          taps.closed += 1;
        },
      };
    },
    closeTaps: () => {
      taps.released += 1;
    },
    onPath: (id, path) => paths.push(`${id}:${path}`),
    resumeTaps: () => {
      gestures.resumed += 1;
    },
    onReport: (voices, floor) => reports.push({ voices, floor }),
  });
  return {
    monitor,
    snapshots,
    paths,
    taps,
    timers,
    reports,
    gestures,
    run: async (ms) => {
      const until = now + ms;
      while (now < until) {
        now += SAMPLE_INTERVAL_MS;
        (tick as (() => void) | null)?.();
        // Let the statistics readings started by this tick fold in.
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();
      }
      return snapshots[snapshots.length - 1] as SpeakingSnapshot;
    },
  };
}

const me = (s: MediaStream, micOn = true): VoiceSource => ({ id: "local", isLocal: true, micOn, stream: s });
const peer = (
  id: string,
  receiver: LevelReceiver | undefined,
  s: MediaStream | undefined = stream(),
  micOn = true,
): VoiceSource => ({ id, isLocal: false, micOn, stream: s, receiver });

describe("where a level is read from", () => {
  it("reads someone else from their receiver's sources, with no tap and no statistics", async () => {
    const r = rig();
    const b = new SourceReceiver();
    r.monitor.sync([me(stream()), peer("b", b)]);
    b.level = LOUD;
    const snap = await r.run(FLOOR_ONSET_MS + 500);
    expect(snap.speakingIds.has("b")).toBe(true);
    expect(snap.activeSpeakerId).toBe("b");
    expect(r.paths).toContain("b:sync-source");
    // The one tap is this device's own microphone.
    expect(r.taps.opened).toBe(1);
  });

  it("reads someone else from their receiver's statistics where the sources carry no level", async () => {
    const r = rig();
    const b = new StatsReceiver();
    r.monitor.sync([me(stream()), peer("b", b)]);
    b.level = LOUD;
    const snap = await r.run(FLOOR_ONSET_MS + 800);
    expect(snap.activeSpeakerId).toBe("b");
    expect(r.paths).toContain("b:stats-energy");
    expect(r.taps.opened).toBe(1);
    b.level = 0;
    expect((await r.run(2_000)).speakingIds.has("b")).toBe(false);
  });

  it("falls back to the played stream when the receiver offers no level, and when there is none", async () => {
    const r = rig();
    const bStream = stream(LOUD);
    const cStream = stream(0);
    r.monitor.sync([me(stream()), peer("b", new MuteReceiver(), bStream), peer("c", undefined, cStream)]);
    const snap = await r.run(FLOOR_ONSET_MS + 800);
    expect(snap.activeSpeakerId).toBe("b");
    expect(r.paths).toContain("b:analyser");
    expect(r.paths).toContain("c:analyser");
    expect(r.taps.opened).toBe(3);
  });

  it("reads this device's own microphone from its stream and draws its ring", async () => {
    const r = rig();
    const mine = stream(LOUD);
    r.monitor.sync([me(mine), peer("b", new SourceReceiver())]);
    const snap = await r.run(1_000);
    expect(snap.speakingIds.has("local")).toBe(true);
    expect(r.paths).toContain("local:analyser");
  });

  it("opens a tap once a stream that arrived without audio gains its track", async () => {
    const r = rig();
    const late = stream(LOUD, 0);
    r.monitor.sync([me(stream()), peer("b", undefined, late)]);
    await r.run(500);
    expect(r.taps.opened).toBe(1);
    late.audio = 1;
    const snap = await r.run(1_000);
    expect(r.taps.opened).toBe(2);
    expect(snap.speakingIds.has("b")).toBe(true);
  });

  it("says once when a person has no source of any kind", async () => {
    const r = rig();
    r.monitor.sync([{ id: "b", isLocal: false, micOn: true, stream: undefined }]);
    await r.run(10_000);
    expect(r.paths).toEqual(["b:none"]);
  });
});

describe("a peer whose audio arrives after their video, in a separate track event", () => {
  // What the call hands over then: first the person's stream with only a
  // video track and no audio receiver, and after the second track event the
  // same stream object, now with its audio track, and the receiver.
  it("is measured from the receiver once it exists", async () => {
    const r = rig();
    const theirs = stream(0, 0);
    const mine = stream();
    r.monitor.sync([me(mine), peer("b", undefined, theirs)]);
    let snap = await r.run(1_000);
    expect(snap.speakingIds.has("b")).toBe(false);
    // The audio track event: same stream, now with audio, and a receiver.
    theirs.audio = 1;
    const receiver = new StatsReceiver();
    receiver.level = LOUD;
    r.monitor.sync([me(mine), peer("b", receiver, theirs)]);
    snap = await r.run(FLOOR_ONSET_MS + 800);
    expect(snap.speakingIds.has("b")).toBe(true);
    expect(snap.activeSpeakerId).toBe("b");
    expect(r.paths).toContain("b:stats-energy");
  });

  it("is measured from the stream when nobody says the stream changed", async () => {
    // The old reader looked at a stream's audio tracks once, when it was
    // first handed the stream, and was not handed it again because it was
    // the same object: such a person was never measured.
    const r = rig();
    const theirs = stream(LOUD, 0);
    r.monitor.sync([me(stream()), peer("b", undefined, theirs)]);
    await r.run(1_000);
    theirs.audio = 1;
    const snap = await r.run(FLOOR_ONSET_MS + 800);
    expect(snap.activeSpeakerId).toBe("b");
    expect(r.paths).toContain("b:analyser");
  });
});

describe("an audio context that is not running", () => {
  it("is no source, never silence, and the person is measured once it runs", async () => {
    const r = rig();
    const theirs = stream(null);
    r.monitor.sync([me(stream(null)), peer("b", undefined, theirs)]);
    await r.run(4_000);
    // Said once per person: there is no source. Nothing claims "analyser".
    expect(r.paths.sort()).toEqual(["b:none", "local:none"]);
    const last = r.reports[r.reports.length - 1];
    expect(last?.voices.map((v) => `${v.id}=${v.path}:${String(v.peak)}`)).toEqual([
      "local=none:null",
      "b=none:null",
    ]);
    // The context starts.
    theirs.level = LOUD;
    const snap = await r.run(FLOOR_ONSET_MS + 800);
    expect(snap.activeSpeakerId).toBe("b");
    expect(r.paths).toContain("b:analyser");
  });

  it("is asked to start on every gesture the call screen reports", () => {
    const r = rig();
    r.monitor.userGesture();
    r.monitor.userGesture();
    expect(r.gestures.resumed).toBe(2);
  });

  it("reads null through the browser's own taps while suspended, resumes on a gesture and says its state once per change", async () => {
    const said: string[] = [];
    let resumes = 0;
    class FakeContext {
      public state = "suspended";
      public listeners: (() => void)[] = [];
      public addEventListener(_: string, fn: () => void): void {
        this.listeners.push(fn);
      }
      public async resume(): Promise<void> {
        resumes += 1;
        if (allow) {
          this.state = "running";
          for (const fn of this.listeners) fn();
        }
      }
      public createMediaStreamSource(): { connect(): void; disconnect(): void } {
        return { connect: () => undefined, disconnect: () => undefined };
      }
      public createAnalyser(): { fftSize: number; getFloatTimeDomainData(b: Float32Array): void } {
        return { fftSize: 512, getFloatTimeDomainData: (b) => b.fill(0.5) };
      }
      public async close(): Promise<void> {}
    }
    let allow = false;
    const g = globalThis as { AudioContext?: unknown };
    const before = g.AudioContext;
    g.AudioContext = FakeContext;
    try {
      const taps = browserTaps((line) => said.push(line));
      const tap = taps.openTap(stream());
      await Promise.resolve();
      await Promise.resolve();
      expect(tap?.read()).toBeNull();
      expect(resumes).toBe(1);
      expect(said).toEqual(["[call] sound level audio context: suspended"]);
      // A gesture: the browser lets it start.
      allow = true;
      taps.resumeTaps();
      await Promise.resolve();
      await Promise.resolve();
      expect(resumes).toBe(2);
      expect(tap?.read()).toBeCloseTo(0.5);
      expect(said).toEqual([
        "[call] sound level audio context: suspended",
        "[call] sound level audio context: running",
      ]);
      // Running: a further gesture asks nothing.
      taps.resumeTaps();
      expect(resumes).toBe(2);
    } finally {
      g.AudioContext = before;
    }
  });
});

describe("the diagnostics report", () => {
  it("says every 2 seconds where each level came from, its peak, whether it counted, and the floor", async () => {
    const r = rig();
    const b = new SourceReceiver();
    const c = new StatsReceiver();
    b.level = 0.31;
    c.level = 0.005;
    r.monitor.sync([me(stream(0.2), false), peer("b", b), peer("c", c), peer("d", new MuteReceiver(), stream(null))]);
    await r.run(REPORT_EVERY_TICKS * SAMPLE_INTERVAL_MS);
    expect(r.reports).toHaveLength(1);
    const first = r.reports[0];
    expect(first?.floor).toBe("b");
    const byId = Object.fromEntries((first?.voices ?? []).map((v) => [v.id, v]));
    // Muted: no source is claimed and nothing counted.
    expect(byId.local).toMatchObject({ isLocal: true, path: "none", peak: null, speaking: false });
    expect(byId.b).toMatchObject({ path: "sync-source", speaking: true });
    expect(byId.b?.peak).toBeCloseTo(0.31);
    // Measured, and below the threshold.
    expect(byId.c).toMatchObject({ path: "stats-energy", speaking: false });
    expect(byId.c?.peak).toBeCloseTo(0.005);
    expect(byId.d).toMatchObject({ path: "none", peak: null, speaking: false });
    // The next interval starts from nothing.
    b.level = 0;
    await r.run(REPORT_EVERY_TICKS * SAMPLE_INTERVAL_MS);
    expect(r.reports).toHaveLength(2);
    expect(r.reports[1]?.voices.find((v) => v.id === "b")?.peak).toBe(0);
  });
});

describe("the floor and the stage's inputs", () => {
  it("does not let this device's own voice keep the floor from the person speaking", async () => {
    const r = rig();
    const b = new SourceReceiver();
    const c = new SourceReceiver();
    r.monitor.sync([me(stream(LOUD)), peer("b", b), peer("c", c)]);
    c.level = LOUD;
    const snap = await r.run(FLOOR_ONSET_MS + 500);
    expect(snap.speakingIds.has("local")).toBe(true);
    expect(snap.activeSpeakerId).toBe("c");
  });

  it("never counts a muted person as speaking, and stops at once on a mute", async () => {
    const r = rig();
    const mine = stream(LOUD);
    const b = new SourceReceiver();
    b.level = LOUD;
    r.monitor.sync([me(mine, false), peer("b", b)]);
    let snap = await r.run(FLOOR_ONSET_MS + 500);
    expect(snap.speakingIds.has("local")).toBe(false);
    expect(snap.activeSpeakerId).toBe("b");
    // b is muted (were a mute of someone else ever known to this device).
    r.monitor.sync([me(mine, false), peer("b", b, undefined, false)]);
    snap = await r.run(SAMPLE_INTERVAL_MS);
    expect(snap.speakingIds.has("b")).toBe(false);
    expect(snap.activeSpeakerId).toBeNull();
    // Unmuting this device brings its ring back.
    r.monitor.sync([me(mine, true), peer("b", b, undefined, false)]);
    snap = await r.run(1_000);
    expect(snap.speakingIds.has("local")).toBe(true);
  });

  it("forgets a person who left", async () => {
    const r = rig();
    const b = new SourceReceiver();
    b.level = LOUD;
    const mine = stream();
    r.monitor.sync([me(mine), peer("b", b)]);
    expect((await r.run(FLOOR_ONSET_MS + 500)).activeSpeakerId).toBe("b");
    r.monitor.sync([me(mine)]);
    const snap = await r.run(SAMPLE_INTERVAL_MS);
    expect(snap.activeSpeakerId).toBeNull();
    expect(snap.lastSpokeAtMs).toEqual({});
  });
});

describe("what it costs", () => {
  it("runs one timer for the call and stops it on dispose", async () => {
    const r = rig();
    const mine = stream();
    r.monitor.sync([me(mine), peer("b", new SourceReceiver())]);
    r.monitor.sync([me(mine), peer("b", new SourceReceiver()), peer("c", new StatsReceiver())]);
    expect(r.timers.started).toBe(1);
    await r.run(500);
    r.monitor.dispose();
    expect(r.timers.running).toBe(false);
    expect(r.timers.stopped).toBe(1);
    expect(r.taps.closed).toBe(r.taps.opened);
    expect(r.taps.released).toBe(1);
    const before = r.snapshots.length;
    await r.run(500);
    expect(r.snapshots.length).toBe(before);
  });

  it("stops the timer when nobody is left to read and starts it again", () => {
    const r = rig();
    r.monitor.sync([peer("b", new SourceReceiver())]);
    expect(r.timers.running).toBe(true);
    r.monitor.sync([]);
    expect(r.timers.running).toBe(false);
    r.monitor.sync([peer("b", new SourceReceiver())]);
    expect(r.timers.started).toBe(2);
  });

  it("starts a bounded number of statistics readings per tick and reaches everyone in turn", async () => {
    const r = rig();
    const receivers = Array.from({ length: 3 * MAX_STATS_READS_PER_TICK }, () => new StatsReceiver());
    r.monitor.sync(receivers.map((rx, i) => peer(`p${i}`, rx)));
    await r.run(SAMPLE_INTERVAL_MS);
    expect(receivers.reduce((n, rx) => n + rx.calls, 0)).toBe(MAX_STATS_READS_PER_TICK);
    await r.run(2 * SAMPLE_INTERVAL_MS);
    expect(receivers.every((rx) => rx.calls === 1)).toBe(true);
    await r.run(3 * SAMPLE_INTERVAL_MS);
    expect(receivers.every((rx) => rx.calls === 2)).toBe(true);
  });

  it("closes a person's tap when their stream is replaced", async () => {
    const r = rig();
    const first = stream(LOUD);
    r.monitor.sync([peer("b", undefined, first)]);
    await r.run(300);
    r.monitor.sync([peer("b", undefined, stream(0))]);
    await r.run(300);
    expect(r.taps.opened).toBe(2);
    expect(r.taps.closed).toBe(1);
  });
});
