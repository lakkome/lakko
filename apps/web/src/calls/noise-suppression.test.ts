// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which microphone track the sender carries (ADR-0088, UX
 * `docs/ux/calls.md` section 4.1.3): the processed one while the switch is
 * on and the graph runs, the raw one in every other case.
 */

import { beforeEach, describe, expect, it } from "vitest";

import {
  NoiseSuppressionController,
  type DenoisedTrack,
  type DenoiserFactory,
} from "./noise-suppression";
import {
  NOISE_SUPPRESSION_DEFAULT_ON,
  NOISE_SUPPRESSION_STORAGE_KEY,
  noiseSuppressionOn,
  onNoiseSuppressionChange,
  readNoiseSuppression,
  resetNoiseSuppressionForTest,
  setNoiseSuppression,
  type SettingStorage,
} from "./noise-setting";

interface FakeTrack {
  readonly id: string;
  readonly kind: "audio";
  enabled: boolean;
  readyState: "live" | "ended";
  stopped: boolean;
  getSettings(): MediaTrackSettings;
  stop(): void;
}

function fakeTrack(id: string): FakeTrack {
  return {
    id,
    kind: "audio",
    enabled: true,
    readyState: "live",
    stopped: false,
    getSettings: () => ({ echoCancellation: true, noiseSuppression: true, autoGainControl: true }),
    stop(): void {
      this.stopped = true;
      this.readyState = "ended";
    },
  };
}

const asTrack = (t: FakeTrack): MediaStreamTrack => t as unknown as MediaStreamTrack;

interface Harness {
  readonly controller: NoiseSuppressionController;
  readonly sent: string[];
  readonly logs: string[];
  /** The failure callback of every build, newest last. */
  readonly failures: ((reason: string) => void)[];
  readonly built: FakeTrack[];
  readonly disposed: string[];
  setOn(on: boolean): void;
}

function harness(opts: {
  factory?: "ok" | "reject";
  initialOn?: boolean;
  /** Called while a build is in flight, before it resolves. */
  duringBuild?: (h: Harness) => void;
}): Harness {
  const sent: string[] = [];
  const logs: string[] = [];
  const failures: ((reason: string) => void)[] = [];
  const built: FakeTrack[] = [];
  const disposed: string[] = [];
  let listener: ((on: boolean) => void) | null = null;
  let h: Harness | null = null;
  let count = 0;
  const factory: DenoiserFactory = async (raw, onFailure) => {
    failures.push(onFailure);
    if (opts.duringBuild !== undefined && h !== null) opts.duringBuild(h);
    await Promise.resolve();
    if (opts.factory === "reject") throw new Error("no AudioWorklet");
    const track = fakeTrack(`processed-${count++}-of-${raw.id}`);
    built.push(track);
    const result: DenoisedTrack = {
      track: asTrack(track),
      dispose(): void {
        if (!disposed.includes(track.id)) disposed.push(track.id);
        track.stop();
      },
    };
    return result;
  };
  const controller = new NoiseSuppressionController({
    label: "test",
    replaceSenderTrack: async (track): Promise<void> => {
      await Promise.resolve();
      sent.push(track.id);
    },
    factory,
    log: (line) => logs.push(line),
    initialOn: opts.initialOn ?? true,
    subscribe: (l) => {
      listener = l;
      return () => {
        listener = null;
      };
    },
  });
  h = {
    controller,
    sent,
    logs,
    failures,
    built,
    disposed,
    setOn: (on) => listener?.(on),
  };
  return h;
}

const flush = async (): Promise<void> => {
  for (let i = 0; i < 10; i++) await Promise.resolve();
};

describe("NoiseSuppressionController", () => {
  it("swaps the processed track onto the sender when the graph runs", async () => {
    const h = harness({ factory: "ok" });
    const raw = fakeTrack("raw");
    await h.controller.attach(asTrack(raw));
    expect(h.sent).toEqual(["raw", "processed-0-of-raw"]);
    expect(h.controller.processedTrack?.id).toBe("processed-0-of-raw");
    expect(h.logs.at(-1)).toContain("rnnoise applied");
    expect(h.logs.at(-1)).toContain("echoCancellation=true");
  });

  it("falls back to the raw track when the graph cannot be built", async () => {
    const h = harness({ factory: "reject" });
    const raw = fakeTrack("raw");
    await h.controller.attach(asTrack(raw));
    // The sender only ever carries the raw microphone.
    expect(h.sent).toEqual(["raw"]);
    expect(h.controller.processedTrack).toBeNull();
    expect(h.logs.at(-1)).toContain("platform processing only (no AudioWorklet)");
  });

  it("sends the raw track and builds nothing while the switch is off", async () => {
    const h = harness({ factory: "ok", initialOn: false });
    await h.controller.attach(asTrack(fakeTrack("raw")));
    expect(h.sent).toEqual(["raw"]);
    expect(h.built).toEqual([]);
    expect(h.logs.at(-1)).toContain("off (switch)");
  });

  it("turning the switch off mid-call puts the raw track back and closes the graph", async () => {
    const h = harness({ factory: "ok" });
    await h.controller.attach(asTrack(fakeTrack("raw")));
    h.setOn(false);
    await flush();
    expect(h.sent).toEqual(["raw", "processed-0-of-raw", "raw"]);
    expect(h.disposed).toEqual(["processed-0-of-raw"]);
    h.setOn(true);
    await flush();
    expect(h.sent.at(-1)).toBe("processed-1-of-raw");
  });

  it("a graph that fails after it is on the sender gives the raw track back", async () => {
    const h = harness({ factory: "ok" });
    await h.controller.attach(asTrack(fakeTrack("raw")));
    h.failures.at(-1)?.("AudioContext suspended");
    await flush();
    expect(h.sent.at(-1)).toBe("raw");
    expect(h.disposed).toEqual(["processed-0-of-raw"]);
    expect(h.logs.at(-1)).toContain("platform processing only (AudioContext suspended)");
  });

  it("a graph that fails while it is being built never reaches the sender", async () => {
    const h = harness({
      factory: "ok",
      duringBuild: (x) => x.failures.at(-1)?.("worklet: init failed"),
    });
    await h.controller.attach(asTrack(fakeTrack("raw")));
    expect(h.sent).toEqual(["raw"]);
    expect(h.disposed).toEqual(["processed-0-of-raw"]);
  });

  it("a build overtaken by a microphone switch disposes of itself", async () => {
    const h = harness({
      factory: "ok",
      duringBuild: (x) => {
        if (x.built.length === 0 && x.failures.length === 1) x.controller.detach();
      },
    });
    await h.controller.attach(asTrack(fakeTrack("old")));
    // The old build was dropped, not put on the sender.
    expect(h.sent).toEqual(["old"]);
    expect(h.disposed).toEqual(["processed-0-of-old"]);
    await h.controller.attach(asTrack(fakeTrack("new")));
    expect(h.sent).toEqual(["old", "new", "processed-1-of-new"]);
  });

  it("mute follows onto the processed track, and dispose stops it", async () => {
    const h = harness({ factory: "ok" });
    const raw = fakeTrack("raw");
    await h.controller.attach(asTrack(raw));
    h.controller.setMuted(true);
    expect(h.built[0]?.enabled).toBe(false);
    h.controller.dispose();
    expect(h.built[0]?.stopped).toBe(true);
    h.setOn(false);
    await flush();
    expect(h.sent).toEqual(["raw", "processed-0-of-raw"]);
  });
});

describe("noise suppression setting", () => {
  beforeEach(() => resetNoiseSuppressionForTest());

  function memoryStorage(): SettingStorage & { data: Map<string, string> } {
    const data = new Map<string, string>();
    return {
      data,
      getItem: (k) => data.get(k) ?? null,
      setItem: (k, v) => {
        data.set(k, v);
      },
    };
  }

  it("gives the default when nothing is stored or the storage cannot be read", () => {
    expect(readNoiseSuppression(null)).toBe(NOISE_SUPPRESSION_DEFAULT_ON);
    const broken: SettingStorage = {
      getItem: () => {
        throw new Error("SecurityError");
      },
      setItem: () => {
        throw new Error("SecurityError");
      },
    };
    expect(readNoiseSuppression(broken)).toBe(NOISE_SUPPRESSION_DEFAULT_ON);
    expect(readNoiseSuppression(memoryStorage())).toBe(NOISE_SUPPRESSION_DEFAULT_ON);
  });

  it("stores the choice and tells subscribers once per change", () => {
    const storage = memoryStorage();
    const heard: boolean[] = [];
    setNoiseSuppression(true, storage);
    const off = onNoiseSuppressionChange((on) => heard.push(on));
    setNoiseSuppression(false, storage);
    setNoiseSuppression(false, storage);
    expect(storage.data.get(NOISE_SUPPRESSION_STORAGE_KEY)).toBe("off");
    expect(readNoiseSuppression(storage)).toBe(false);
    expect(noiseSuppressionOn()).toBe(false);
    expect(heard).toEqual([false]);
    off();
    setNoiseSuppression(true, storage);
    expect(heard).toEqual([false]);
  });

  it("keeps the choice for the page when the storage cannot be written", () => {
    const broken: SettingStorage = {
      getItem: () => null,
      setItem: () => {
        throw new Error("QuotaExceededError");
      },
    };
    setNoiseSuppression(false, broken);
    expect(noiseSuppressionOn()).toBe(false);
  });
});
