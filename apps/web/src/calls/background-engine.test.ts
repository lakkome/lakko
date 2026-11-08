// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The frame gate on the web (ADR-0089 section 3): while a background is
 * selected, a camera frame the background was not applied to never leaves.
 *
 * Driven two ways: with a scripted segmenter (every failure the engine can
 * meet, each in isolation), and with the real `lakko_segment.wasm` (the Rust
 * gate and health watch themselves) under a compositor that fails. The
 * second is skipped where the module was not built (`npm run build:segment`).
 */

import { describe, expect, it } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import {
  ACTION_DROP,
  ACTION_DROP_AND_STOP,
  ACTION_SEND_CAMERA,
  ACTION_SEND_PROCESSED,
  BackgroundEngine,
  SEG_FAILED,
  SEG_STARTING,
  SEG_WORKING,
  type FrameCompositor,
  type FrameSegmenter,
} from "./background-engine";
import { SegmentInstance } from "./background-segment-wasm";

/** A camera frame: an object whose identity a test can follow. */
interface Frame {
  readonly raw: true;
  readonly n: number;
}
interface Processed {
  readonly processed: true;
  readonly from: number;
}

/** The shared gate, as the Rust rule has it. */
function gate(selected: boolean, state: number): number {
  if (!selected) return ACTION_SEND_CAMERA;
  if (state === SEG_WORKING) return ACTION_SEND_PROCESSED;
  if (state === SEG_STARTING) return ACTION_DROP;
  return ACTION_DROP_AND_STOP;
}

function scriptedSegmenter(states: { onFrame: number; segment: number; throwOn?: "segment" | "onFrame" }): FrameSegmenter {
  return {
    onFrame: (): number => {
      if (states.throwOn === "onFrame") throw new Error("boom");
      return states.onFrame;
    },
    inputBuffer: (): Uint8Array => new Uint8Array(256 * 144 * 4),
    segment: (): number => {
      if (states.throwOn === "segment") throw new Error("boom");
      return states.segment;
    },
    mask: (): Uint8Array => new Uint8Array(256 * 144).fill(255),
    frameAction: gate,
    restart: (): number => SEG_STARTING,
  };
}

function compositor(opts: { throwOnComposite?: boolean; throwOnInput?: boolean } = {}): FrameCompositor<Frame, Processed, string> {
  return {
    writeModelInput: (): void => {
      if (opts.throwOnInput === true) throw new Error("gl lost");
    },
    composite: (frame): Processed => {
      if (opts.throwOnComposite === true) throw new Error("gl lost");
      return { processed: true, from: frame.n };
    },
  };
}

/** Runs `count` frames; returns every output and whether the engine asked
 *  to stop the camera. */
function drive(engine: BackgroundEngine<Frame, Processed, string>, count: number): { outputs: unknown[]; stopped: boolean } {
  const outputs: unknown[] = [];
  let stopped = false;
  for (let n = 0; n < count; n++) {
    const frame: Frame = { raw: true, n };
    const outcome = engine.process(frame);
    if (outcome.kind === "send") {
      expect(outcome.output).not.toBe(frame);
      outputs.push(outcome.output);
    }
    if (outcome.kind === "stop") stopped = true;
  }
  return { outputs, stopped };
}

function isRaw(x: unknown): boolean {
  return typeof x === "object" && x !== null && "raw" in x;
}

describe("the frame gate with a failing processor", () => {
  let t = 0;
  const clock = (): number => (t += 50);

  const failures: [string, FrameSegmenter, FrameCompositor<Frame, Processed, string>][] = [
    ["the model never loads", scriptedSegmenter({ onFrame: SEG_FAILED, segment: SEG_FAILED }), compositor()],
    ["every run fails", scriptedSegmenter({ onFrame: SEG_STARTING, segment: SEG_FAILED }), compositor()],
    ["the model throws", scriptedSegmenter({ onFrame: SEG_STARTING, segment: SEG_WORKING, throwOn: "segment" }), compositor()],
    ["the watch throws", scriptedSegmenter({ onFrame: SEG_WORKING, segment: SEG_WORKING, throwOn: "onFrame" }), compositor()],
    [
      "the compositor loses its context drawing",
      scriptedSegmenter({ onFrame: SEG_WORKING, segment: SEG_WORKING }),
      compositor({ throwOnComposite: true }),
    ],
    [
      "the compositor cannot make the model input",
      scriptedSegmenter({ onFrame: SEG_STARTING, segment: SEG_WORKING }),
      compositor({ throwOnInput: true }),
    ],
  ];

  for (const [name, seg, comp] of failures) {
    it(`sends no camera frame, and stops the camera, when ${name}`, () => {
      const engine = new BackgroundEngine<Frame, Processed, string>(seg, comp, clock, 0);
      engine.setMode("blur");
      const { outputs, stopped } = drive(engine, 20);
      expect(outputs.filter(isRaw)).toEqual([]);
      expect(outputs).toEqual([]);
      expect(stopped).toBe(true);
      expect(engine.state).toBe("failed");
    });
  }

  it("drops every frame while starting, and sends none raw", () => {
    const engine = new BackgroundEngine<Frame, Processed, string>(
      scriptedSegmenter({ onFrame: SEG_STARTING, segment: SEG_STARTING }),
      compositor(),
      clock,
      0,
    );
    engine.setMode("blur");
    const { outputs, stopped } = drive(engine, 20);
    expect(outputs).toEqual([]);
    expect(stopped).toBe(false);
    expect(engine.state).toBe("starting");
  });

  it("drops frames while the picture is still loading, even when working", () => {
    const engine = new BackgroundEngine<Frame, Processed, string>(
      scriptedSegmenter({ onFrame: SEG_WORKING, segment: SEG_WORKING }),
      compositor(),
      clock,
      0,
    );
    const { outputs } = drive(engine, 5);
    expect(outputs).toEqual([]);
  });

  it("never passes the camera even if the gate answers 'send the camera'", () => {
    const seg = scriptedSegmenter({ onFrame: SEG_WORKING, segment: SEG_WORKING });
    const engine = new BackgroundEngine<Frame, Processed, string>(
      { ...seg, frameAction: (): number => ACTION_SEND_CAMERA },
      compositor(),
      clock,
      0,
    );
    engine.setMode("blur");
    expect(drive(engine, 5).outputs).toEqual([]);
  });

  it("sends only processed frames while working", () => {
    const engine = new BackgroundEngine<Frame, Processed, string>(
      scriptedSegmenter({ onFrame: SEG_WORKING, segment: SEG_WORKING }),
      compositor(),
      clock,
      0,
    );
    engine.setMode("blur");
    const { outputs } = drive(engine, 5);
    expect(outputs).toHaveLength(5);
    expect(outputs.every((o) => (o as Processed).processed)).toBe(true);
  });
});

const segmentWasm = fileURLToPath(new URL("../../wasm/lakko_segment.wasm", import.meta.url));

describe.skipIf(!existsSync(segmentWasm))("the frame gate with the real lakko_segment.wasm", () => {
  it("a compositor that fails lets no camera frame through, and the Rust gate stops the camera", async () => {
    const module = await WebAssembly.compile(readFileSync(segmentWasm));
    const seg = await SegmentInstance.create(module);
    let now = 1;
    const clock = (): number => now;
    expect(seg.init(clock())).toBe(true);
    const engine = new BackgroundEngine<Frame, Processed, string>(seg, compositor({ throwOnComposite: true }), clock, 0);
    engine.setMode("blur");
    const outputs: unknown[] = [];
    let stopped = false;
    for (let n = 0; n < 10; n++) {
      now += 33;
      const outcome = engine.process({ raw: true, n });
      if (outcome.kind === "send") outputs.push(outcome.output);
      if (outcome.kind === "stop") stopped = true;
    }
    expect(outputs).toEqual([]);
    expect(stopped).toBe(true);
  }, 60_000);

  it("a model that sees frames and no masks fails by its own watch, with nothing sent", async () => {
    const module = await WebAssembly.compile(readFileSync(segmentWasm));
    const seg = await SegmentInstance.create(module);
    let now = 1;
    expect(seg.init(now)).toBe(true);
    // Frames keep arriving and no run ever happens (inference interval
    // longer than the test): the Rust watch fails it after 15 s without a
    // first mask.
    const engine = new BackgroundEngine<Frame, Processed, string>(
      { ...seg, segment: (): number => SEG_STARTING, onFrame: (t) => seg.onFrame(t), frameAction: (s, st) => seg.frameAction(s, st), inputBuffer: () => seg.inputBuffer(), mask: () => seg.mask(), restart: (t) => seg.restart(t) },
      compositor(),
      () => now,
      Number.MAX_SAFE_INTEGER,
    );
    engine.setMode("blur");
    let stopped = false;
    const outputs: unknown[] = [];
    for (let n = 0; n < 600 && !stopped; n++) {
      now += 33;
      const outcome = engine.process({ raw: true, n });
      if (outcome.kind === "send") outputs.push(outcome.output);
      if (outcome.kind === "stop") stopped = true;
    }
    expect(outputs).toEqual([]);
    expect(stopped).toBe(true);
  }, 60_000);

  it("a working model sends processed frames only", async () => {
    const module = await WebAssembly.compile(readFileSync(segmentWasm));
    const seg = await SegmentInstance.create(module);
    let now = 1;
    expect(seg.init(now)).toBe(true);
    const engine = new BackgroundEngine<Frame, Processed, string>(seg, compositor(), () => now, 0);
    engine.setMode("blur");
    const outputs: unknown[] = [];
    for (let n = 0; n < 3; n++) {
      now += 33;
      const outcome = engine.process({ raw: true, n });
      if (outcome.kind === "send") outputs.push(outcome.output);
    }
    expect(outputs.length).toBeGreaterThan(0);
    expect(outputs.filter(isRaw)).toEqual([]);
    // The shared parameters reach the compositor.
    expect(seg.blur(false, 720).radius).toBe(7);
    expect(seg.builtin(0, 4, 2)?.length).toBe(32);
    expect(seg.builtin(6, 4, 2)).toBeNull();
  }, 60_000);
});
