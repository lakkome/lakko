// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The background controller of a call session (ADR-0089): which track the
 * session publishes as its camera. The privacy rule pinned here: while a
 * background is selected, the raw camera track is never handed to the
 * session, not at capture, not when a background fails, not when it starts
 * again; and a failure is reported so the camera is turned off.
 */

import { afterEach, describe, expect, it } from "vitest";

import type { BackgroundChoice } from "./background-choice";
import { CameraBackgroundController, resolveStageMode } from "./background-controller";
import type { StageMode } from "./background-mode";
import { bindBackgroundPhotos } from "./background-photos";
import type { BackgroundStage, StageState } from "./background-stage";
import { cameraBackgroundState, onCameraBackgroundFailure, publishCameraBackground } from "./background-status";

/** A stand-in for a `MediaStreamTrack`: what the controller touches. */
class FakeTrack {
  enabled = true;
  stopped = false;
  constructor(readonly name: string) {}
  stop(): void {
    this.stopped = true;
  }
}

interface FakeStage extends BackgroundStage {
  readonly fakeTrack: FakeTrack;
  readonly modes: (StageMode | null)[];
  disposed: boolean;
  say(state: StageState): void;
}

function stageFactory(script: (n: number) => StageState): {
  factory: (raw: MediaStreamTrack, onState: (s: StageState) => void) => BackgroundStage;
  stages: FakeStage[];
} {
  const stages: FakeStage[] = [];
  const factory = (_raw: MediaStreamTrack, onState: (s: StageState) => void): BackgroundStage => {
    const n = stages.length;
    const fakeTrack = new FakeTrack(`processed-${n}`);
    const stage: FakeStage = {
      fakeTrack,
      track: fakeTrack as unknown as MediaStreamTrack,
      modes: [],
      disposed: false,
      say: onState,
      setMode(mode): void {
        stage.modes.push(mode);
      },
      setEnabled(): void {},
      dispose(): void {
        stage.disposed = true;
        fakeTrack.stop();
      },
    };
    stages.push(stage);
    onState("starting");
    const first = script(n);
    if (first !== "starting") queueMicrotask(() => onState(first));
    return stage;
  };
  return { factory, stages };
}

function harness(initial: BackgroundChoice, script: (n: number) => StageState): {
  controller: CameraBackgroundController;
  published: unknown[];
  stages: FakeStage[];
  setChoice(c: BackgroundChoice): void;
  failures: () => number;
} {
  const { factory, stages } = stageFactory(script);
  const published: unknown[] = [];
  let listener: ((c: BackgroundChoice) => void) | null = null;
  let failures = 0;
  const off = onCameraBackgroundFailure(() => {
    failures += 1;
  });
  cleanups.push(off);
  const controller = new CameraBackgroundController({
    label: "test",
    initialChoice: initial,
    stageFactory: factory,
    resolveMode: (c) => Promise.resolve<StageMode>({ kind: "blur", strong: c.kind === "blur" && c.strong }),
    subscribe: (l) => {
      listener = l;
      return () => {
        listener = null;
      };
    },
    replaceCameraTrack: (track) => {
      published.push(track);
      return Promise.resolve();
    },
    log: () => undefined,
  });
  cleanups.push(() => controller.dispose());
  return {
    controller,
    published,
    stages,
    setChoice: (c) => listener?.(c),
    failures: () => failures,
  };
}

const cleanups: (() => void)[] = [];
afterEach(() => {
  for (const c of cleanups.splice(0)) c();
  publishCameraBackground("off", null);
  bindBackgroundPhotos(null);
});

const tick = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

describe("CameraBackgroundController", () => {
  it("with no background selected, publishes the camera as it is", () => {
    const h = harness({ kind: "none" }, () => "working");
    const raw = new FakeTrack("raw");
    expect(h.controller.adopt(raw as unknown as MediaStreamTrack)).toBe(raw);
    expect(h.stages).toHaveLength(0);
    expect(cameraBackgroundState()).toBe("off");
  });

  it("a failing processor never gets the raw camera to a sender, and turns the camera off", async () => {
    const h = harness({ kind: "blur", strong: false }, () => "failed");
    const raw = new FakeTrack("raw");
    const out = h.controller.adopt(raw as unknown as MediaStreamTrack);
    expect(out).not.toBe(raw);
    expect(out).toBe(h.stages[0]?.track);
    await tick();
    expect(h.controller.state).toBe("failed");
    expect(cameraBackgroundState()).toBe("failed");
    expect(h.failures()).toBe(1);

    // The person turns the camera back on: it starts again, on a new stage,
    // and fails again. Still never the raw camera.
    h.controller.setCameraEnabled(false);
    h.controller.setCameraEnabled(true);
    await h.controller.settled();
    await tick();
    // Picking a background again starts it again too.
    h.setChoice({ kind: "blur", strong: true });
    await h.controller.settled();
    await tick();

    expect(h.published).not.toContain(raw);
    expect(h.published.length).toBeGreaterThan(0);
    expect(h.stages.length).toBeGreaterThanOrEqual(3);
    expect(h.failures()).toBe(3);
    // Every stage but the current one was let go.
    expect(h.stages.slice(0, -1).every((s) => s.disposed)).toBe(true);
    expect(raw.stopped).toBe(false);
  });

  it("a background that fails while the raw camera is already sent never re-sends it", async () => {
    // A background is selected mid-call over a raw camera, and that stage fails.
    const h = harness({ kind: "none" }, () => "failed");
    const raw = new FakeTrack("raw");
    expect(h.controller.adopt(raw as unknown as MediaStreamTrack)).toBe(raw);
    h.setChoice({ kind: "builtin", id: "dawn" });
    await h.controller.settled();
    await tick();
    expect(h.published).toEqual([h.stages[0]?.track]);
    expect(h.controller.track).toBe(h.stages[0]?.track);
    expect(h.failures()).toBe(1);
  });

  it("switching to none publishes the raw camera and lets the stage go", async () => {
    const h = harness({ kind: "blur", strong: false }, () => "working");
    const raw = new FakeTrack("raw");
    h.controller.adopt(raw as unknown as MediaStreamTrack);
    await tick();
    expect(h.controller.state).toBe("working");
    h.setChoice({ kind: "none" });
    await h.controller.settled();
    expect(h.published).toEqual([raw]);
    expect(h.stages[0]?.disposed).toBe(true);
    expect(h.controller.state).toBe("off");
  });

  it("a different background on a working stage is a new mode, not a new track", async () => {
    const h = harness({ kind: "blur", strong: false }, () => "working");
    h.controller.adopt(new FakeTrack("raw") as unknown as MediaStreamTrack);
    await tick();
    h.setChoice({ kind: "blur", strong: true });
    await h.controller.settled();
    await tick();
    expect(h.published).toEqual([]);
    expect(h.stages).toHaveLength(1);
    expect(h.stages[0]?.modes).toEqual([
      { kind: "blur", strong: false },
      { kind: "blur", strong: true },
    ]);
  });

  it("turning the camera off mirrors onto the raw camera", () => {
    const h = harness({ kind: "blur", strong: false }, () => "working");
    const raw = new FakeTrack("raw");
    h.controller.adopt(raw as unknown as MediaStreamTrack);
    h.controller.setCameraEnabled(false);
    expect(raw.enabled).toBe(false);
    expect(h.stages[0]?.fakeTrack.enabled).toBe(false);
  });

  it("a new camera lets the previous one and its stage go; abort lets the new ones go", () => {
    const h = harness({ kind: "blur", strong: false }, () => "working");
    const first = new FakeTrack("first");
    h.controller.adopt(first as unknown as MediaStreamTrack);
    const second = new FakeTrack("second");
    const prepared = h.controller.prepare(second as unknown as MediaStreamTrack);
    prepared.abort();
    expect(second.stopped).toBe(true);
    expect(h.stages[1]?.disposed).toBe(true);
    expect(first.stopped).toBe(false);
    const third = new FakeTrack("third");
    h.controller.prepare(third as unknown as MediaStreamTrack).commit();
    expect(first.stopped).toBe(true);
    expect(h.stages[0]?.disposed).toBe(true);
    h.controller.dispose();
    expect(third.stopped).toBe(true);
  });
});

describe("resolveStageMode", () => {
  it("a selected photo that cannot be read is blur, never the camera", async () => {
    bindBackgroundPhotos(null);
    expect(await resolveStageMode({ kind: "photo", id: "abc" })).toEqual({ kind: "blur", strong: false });
  });

  it("an unknown built-in is blur", async () => {
    expect(await resolveStageMode({ kind: "builtin", id: "nowhere" })).toEqual({ kind: "blur", strong: false });
  });

  it("strong blur stays strong", async () => {
    expect(await resolveStageMode({ kind: "blur", strong: true })).toEqual({ kind: "blur", strong: true });
  });
});
