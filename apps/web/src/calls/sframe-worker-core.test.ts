// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The SFrame worker's engine (`sframe-worker-core.ts`) through the real wasm
 * core, over fake streams: a transform created from an `rtctransform` event
 * and one created from a message carrying transferred streams are the same
 * pipeline, and the control messages reach both.
 *
 * Nothing here runs a browser. The streams are ordinary web streams of
 * frame-shaped objects; whether a browser's own encoded frames survive the
 * transfer to a worker, and what their metadata carries on each interface, is
 * not something this suite can see.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  createSFrameWorkerCore,
  isSFrameTransformOptions,
  type EncodedFrame,
  type SFrameWorkerCore,
} from "./sframe-worker-core";
import type { SFrameTransformOptions } from "./sframe-transform";

const SEED = new Uint8Array(32).fill(0x42);
const SEED_2 = new Uint8Array(32).fill(0x43);
const LEAF = 3;

const concat = (...parts: readonly (readonly number[] | Uint8Array)[]): Uint8Array =>
  Uint8Array.from(parts.flatMap((p) => Array.from(p)));

/** A VP8 key frame: tag (low bit 0), start code, 640 x 480, then data. */
const VP8_KEY = concat(
  [0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01],
  new TextEncoder().encode("a key frame's partitions"),
);
/** A VP8 delta frame: tag with the low bit 1. */
const VP8_DELTA = concat([0x11, 0x02, 0x00], new TextEncoder().encode("inter-frame residual"));
const OPUS = new TextEncoder().encode("twenty milliseconds of opus");

function frame(bytes: Uint8Array, type: string | undefined, metadata: unknown): EncodedFrame {
  return {
    ...(type === undefined ? {} : { type }),
    timestamp: 1234,
    data: bytes.slice().buffer as ArrayBuffer,
    getMetadata: () => metadata,
  };
}

const bytesOf = (f: EncodedFrame): Uint8Array => new Uint8Array(f.data);

/** A readable that yields `frames` and closes. */
function source(frames: readonly EncodedFrame[]): ReadableStream<EncodedFrame> {
  return new ReadableStream<EncodedFrame>({
    start(controller): void {
      for (const f of frames) controller.enqueue(f);
      controller.close();
    },
  });
}

/** A writable that keeps what it is given; `done` resolves when it closes. */
function sink(): { writable: WritableStream<EncodedFrame>; got: EncodedFrame[]; done: Promise<void> } {
  const got: EncodedFrame[] = [];
  let finish: () => void = () => {};
  const done = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const writable = new WritableStream<EncodedFrame>({
    write(f): void {
      got.push(f);
    },
    close(): void {
      finish();
    },
  });
  return { writable, got, done };
}

interface Host {
  readonly core: SFrameWorkerCore;
  /** Messages the engine posted to the main thread. */
  readonly posted: Record<string, unknown>[];
  readonly lines: () => string[];
}

function host(loadWasm: () => Promise<typeof wasm> = async () => wasm): Host {
  const posted: Record<string, unknown>[] = [];
  const core = createSFrameWorkerCore({
    loadWasm,
    post: (m) => posted.push(m as Record<string, unknown>),
  });
  return {
    core,
    posted,
    lines: () => posted.filter((m) => m.kind === "sframe-diag").map((m) => String(m.text)),
  };
}

const encrypt = (id: string, streamIndex: number, mediaKind: "audio" | "video"): SFrameTransformOptions => ({
  operation: "encrypt",
  id,
  seed: SEED,
  localLeaf: LEAF,
  streamIndex,
  mediaKind,
});
const decrypt = (id: string, seed: Uint8Array = SEED): SFrameTransformOptions => ({
  operation: "decrypt",
  id,
  seed,
});

/** Seal `frames` through a transform created from a MESSAGE carrying the
 *  streams (the insertable-streams interface). */
async function sealViaMessage(
  h: Host,
  options: SFrameTransformOptions,
  frames: readonly EncodedFrame[],
): Promise<EncodedFrame[]> {
  const out = sink();
  h.core.onMessage({ type: "encoded-streams", options, readable: source(frames), writable: out.writable });
  await out.done;
  return out.got;
}

/** The same through an `rtctransform` event's transformer. */
async function sealViaTransformer(
  h: Host,
  options: SFrameTransformOptions,
  frames: readonly EncodedFrame[],
): Promise<EncodedFrame[]> {
  const out = sink();
  h.core.onTransformer({ readable: source(frames), writable: out.writable, options });
  await out.done;
  return out.got;
}

const rewrap = (frames: readonly EncodedFrame[]): EncodedFrame[] =>
  frames.map((f) => frame(bytesOf(f), f.type, { payloadType: 96 }));

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("a transform over transferred streams (the insertable-streams interface)", () => {
  it("seals, and a receive transform created the other way opens it", async () => {
    const h = host();
    const sealed = await sealViaMessage(h, encrypt("s-video", 1, "video"), [
      frame(VP8_KEY, "key", { mimeType: "video/VP8" }),
      frame(VP8_DELTA, "delta", { mimeType: "video/VP8" }),
    ]);
    expect(sealed).toHaveLength(2);
    // Sealed, with the codec's own header left at the start (ADR-0071).
    expect(bytesOf(sealed[0]!)).not.toEqual(VP8_KEY);
    expect(bytesOf(sealed[0]!).subarray(0, 10)).toEqual(VP8_KEY.subarray(0, 10));
    expect(bytesOf(sealed[1]!).subarray(0, 3)).toEqual(VP8_DELTA.subarray(0, 3));
    expect(wasm.parseMediaFrame(bytesOf(sealed[0]!)).kid).toBe(BigInt((LEAF << 8) | 1));

    const opened = await sealViaTransformer(h, decrypt("r-1"), rewrap(sealed));
    expect(opened.map(bytesOf)).toEqual([VP8_KEY, VP8_DELTA]);
    expect(h.lines().some((l) => l.includes("op=encrypt") && l.includes("via=encoded-streams"))).toBe(true);
    expect(h.lines().some((l) => l.includes("op=decrypt") && l.includes("via=script-transform"))).toBe(true);
    // Both streams ended: no transform is left.
    await vi.waitFor(() => expect(h.core.liveCount()).toBe(0));
  });

  it("opens what a transform created from an rtctransform event sealed", async () => {
    const h = host();
    const sealed = await sealViaTransformer(h, encrypt("s-audio", 0, "audio"), [frame(OPUS, undefined, {})]);
    expect(bytesOf(sealed[0]!)).not.toEqual(OPUS);
    const opened = await sealViaMessage(h, decrypt("r-2"), rewrap(sealed));
    expect(opened.map(bytesOf)).toEqual([OPUS]);
    expect(h.lines().some((l) => l.includes("op=decrypt") && l.includes("via=encoded-streams"))).toBe(true);
  });

  it("builds the same frame either way: same KID, same clear prefix", async () => {
    const h = host();
    const input = (): EncodedFrame[] => [
      frame(VP8_KEY, "key", { mimeType: "video/VP8" }),
      // Named, so that a video transform does not hold it for a codec table.
      frame(OPUS, undefined, { mimeType: "audio/opus" }),
    ];
    for (const [index, kind] of [
      [1, "video"],
      [3, "video"],
      [4, "video"],
      [0, "audio"],
    ] as const) {
      const a = await sealViaMessage(h, encrypt(`m-${index}`, index, kind), input());
      const b = await sealViaTransformer(h, encrypt(`t-${index}`, index, kind), input());
      expect(a).toHaveLength(2);
      expect(b).toHaveLength(2);
      for (let i = 0; i < 2; i += 1) {
        const pa = wasm.parseMediaFrame(bytesOf(a[i]!));
        const pb = wasm.parseMediaFrame(bytesOf(b[i]!));
        expect(pa.kid).toBe(BigInt((LEAF << 8) | index));
        expect(pb.kid).toBe(pa.kid);
        expect(pb.clearPrefixLen).toBe(pa.clearPrefixLen);
        // Not the whole length: each key starts its counter at a random
        // 62-bit value (ADR-0069), and the header encodes the counter in as
        // many bytes as it needs, so two seals of one frame may differ by
        // the counter's width (at most seven bytes).
        const delta = Math.abs(bytesOf(b[i]!).byteLength - bytesOf(a[i]!).byteLength);
        expect(delta).toBeLessThanOrEqual(7);
      }
    }
  });

  it("drops a frame that does not open, and never passes it on", async () => {
    const h = host();
    const sealed = await sealViaMessage(h, encrypt("s", 1, "video"), [
      frame(VP8_KEY, "key", { mimeType: "video/VP8" }),
    ]);
    const tampered = bytesOf(sealed[0]!).slice();
    tampered[12] = (tampered[12] ?? 0) ^ 0xff;
    const opened = await sealViaMessage(h, decrypt("r"), [
      // Not a sealed frame at all (what an unsealed sender would have sent).
      frame(VP8_KEY, "key", {}),
      frame(tampered, "key", {}),
      ...rewrap(sealed),
    ]);
    expect(opened.map(bytesOf)).toEqual([VP8_KEY]);
    // Under another call's seed nothing opens.
    const other = await sealViaMessage(h, decrypt("r-other", SEED_2), rewrap(sealed));
    expect(other).toEqual([]);
  });
});

describe("control messages reach a transform created either way", () => {
  /** A sender held open so messages can arrive between frames. */
  function liveSender(
    h: Host,
    options: SFrameTransformOptions,
    via: "message" | "transformer",
  ): {
    /** Feed one frame. `expectOut` (the default): wait until it came out.
     *  Otherwise wait a moment and return what came out, if anything. */
    push: (f: EncodedFrame, expectOut?: boolean) => Promise<EncodedFrame | undefined>;
    close: () => Promise<void>;
    got: EncodedFrame[];
  } {
    let controller: ReadableStreamDefaultController<EncodedFrame> | null = null;
    const readable = new ReadableStream<EncodedFrame>({
      start(c): void {
        controller = c;
      },
    });
    const out = sink();
    if (via === "message") {
      h.core.onMessage({ type: "encoded-streams", options, readable, writable: out.writable });
    } else {
      h.core.onTransformer({ readable, writable: out.writable, options });
    }
    const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 20));
    return {
      got: out.got,
      push: async (f, expectOut = true): Promise<EncodedFrame | undefined> => {
        const before = out.got.length;
        controller?.enqueue(f);
        if (expectOut) await vi.waitFor(() => expect(out.got.length).toBeGreaterThan(before));
        else await settle();
        return out.got.length > before ? out.got[out.got.length - 1] : undefined;
      },
      close: async (): Promise<void> => {
        controller?.close();
        await out.done;
      },
    };
  }

  for (const via of ["message", "transformer"] as const) {
    it(`rekey: frames sealed after it open under the new seed only (${via})`, async () => {
      const h = host();
      const sender = liveSender(h, encrypt("s", 0, "audio"), via);
      const before = await sender.push(frame(OPUS, undefined, {}));
      h.core.onMessage({ type: "rekey", id: "s", seed: SEED_2, nowMs: 1000, overlapMs: 1 });
      const after = await sender.push(frame(OPUS, undefined, {}));
      await sender.close();
      expect(before).toBeDefined();
      expect(after).toBeDefined();
      const oldRing = new wasm.CallKeyRing(SEED);
      const newRing = new wasm.CallKeyRing(SEED_2);
      expect(oldRing.openMediaFrameWithOverlap(bytesOf(before!))).toEqual(OPUS);
      expect(newRing.openMediaFrameWithOverlap(bytesOf(after!))).toEqual(OPUS);
      expect(() => oldRing.openMediaFrameWithOverlap(bytesOf(after!))).toThrow();
    });

    it(`codecs: a video frame is held for the table, then sealed with its header clear (${via})`, async () => {
      const h = host();
      const sender = liveSender(h, encrypt("s", 1, "video"), via);
      // The frame names a payload type and no codec; no table has arrived.
      const early = await sender.push(frame(VP8_KEY, "key", { payloadType: 96 }), false);
      expect(early).toBeUndefined();
      expect(h.posted.some((m) => m.kind === "sframe-codecs-wanted" && m.id === "s")).toBe(true);
      h.core.onMessage({ type: "codecs", id: "s", codecs: [{ payloadType: 96, mimeType: "video/VP8" }] });
      await vi.waitFor(() => expect(sender.got).toHaveLength(1));
      await sender.close();
      expect(wasm.parseMediaFrame(bytesOf(sender.got[0]!)).clearPrefixLen).toBe(10);
    });

    it(`retarget: the sender seals under its new stream index from the next frame (${via})`, async () => {
      const h = host();
      const sender = liveSender(h, encrypt("s", 3, "video"), via);
      const light = await sender.push(frame(VP8_DELTA, "delta", { mimeType: "video/VP8" }));
      h.core.onMessage({ type: "retarget", id: "s", localLeaf: LEAF, streamIndex: 1 });
      const main = await sender.push(frame(VP8_DELTA, "delta", { mimeType: "video/VP8" }));
      await sender.close();
      expect(wasm.parseMediaFrame(bytesOf(light!)).kid).toBe(BigInt((LEAF << 8) | 3));
      expect(wasm.parseMediaFrame(bytesOf(main!)).kid).toBe(BigInt((LEAF << 8) | 1));
      expect(new wasm.CallKeyRing(SEED).openMediaFrameWithOverlap(bytesOf(main!))).toEqual(VP8_DELTA);
    });
  }

  it("a rekey that arrives while the core is still loading is applied", async () => {
    let release: (w: typeof wasm) => void = () => {};
    const loading = new Promise<typeof wasm>((resolve) => {
      release = resolve;
    });
    const h = host(() => loading);
    const out = sink();
    h.core.onMessage({
      type: "encoded-streams",
      options: encrypt("s", 0, "audio"),
      readable: source([frame(OPUS, undefined, {})]),
      writable: out.writable,
    });
    h.core.onMessage({ type: "rekey", id: "s", seed: SEED_2, nowMs: 1000, overlapMs: 1 });
    release(wasm);
    await out.done;
    expect(new wasm.CallKeyRing(SEED_2).openMediaFrameWithOverlap(bytesOf(out.got[0]!))).toEqual(OPUS);
  });
});

describe("a transform that cannot be built passes nothing (invariant #4)", () => {
  const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 20));

  it("ignores a message whose options are unusable, leaving its streams untouched", async () => {
    const h = host();
    for (const options of [
      undefined,
      null,
      {},
      { operation: "encrypt", id: "x" },
      { operation: "passthrough", id: "x", seed: SEED },
      { operation: "decrypt", id: "", seed: SEED },
      { operation: "decrypt", id: "x", seed: [1, 2, 3] },
    ]) {
      expect(isSFrameTransformOptions(options)).toBe(false);
      const out = sink();
      const readable = source([frame(OPUS, undefined, {})]);
      h.core.onMessage({ type: "encoded-streams", options, readable, writable: out.writable });
      await settle();
      expect(out.got).toEqual([]);
      expect(readable.locked).toBe(false);
    }
    expect(h.core.liveCount()).toBe(0);
  });

  it("ignores a message that carries no streams", async () => {
    const h = host();
    h.core.onMessage({ type: "encoded-streams", options: decrypt("r") });
    h.core.onMessage({ type: "encoded-streams", options: decrypt("r"), readable: {}, writable: {} });
    await settle();
    expect(h.core.liveCount()).toBe(0);
    expect(h.lines()).toEqual([]);
  });

  it("an encrypting transform with no leaf connects nothing", async () => {
    const h = host();
    const out = sink();
    const readable = source([frame(OPUS, undefined, {})]);
    h.core.onMessage({
      type: "encoded-streams",
      options: { operation: "encrypt", id: "s", seed: SEED, streamIndex: 0 },
      readable,
      writable: out.writable,
    });
    await settle();
    expect(out.got).toEqual([]);
    expect(readable.locked).toBe(false);
  });

  it("a seed the core refuses connects nothing", async () => {
    const h = host();
    const out = sink();
    const readable = source([frame(OPUS, undefined, {})]);
    h.core.onMessage({
      type: "encoded-streams",
      options: { operation: "decrypt", id: "r", seed: new Uint8Array(5) },
      readable,
      writable: out.writable,
    });
    await settle();
    expect(out.got).toEqual([]);
    expect(readable.locked).toBe(false);
    expect(h.lines().some((l) => l.includes("refused"))).toBe(true);
  });

  it("a core that does not load connects nothing, and says so", async () => {
    const h = host(async () => {
      throw new Error("wasm fetch failed");
    });
    const out = sink();
    const readable = source([frame(OPUS, undefined, {})]);
    h.core.onMessage({ type: "encoded-streams", options: decrypt("r"), readable, writable: out.writable });
    await settle();
    expect(out.got).toEqual([]);
    expect(readable.locked).toBe(false);
    expect(h.lines()).toEqual(["sframe transform stopped: id=r via=encoded-streams: wasm fetch failed"]);
  });

  it("a second set of streams under an id in use is left unconnected", async () => {
    const h = host();
    let controller: ReadableStreamDefaultController<EncodedFrame> | null = null;
    const first = sink();
    h.core.onMessage({
      type: "encoded-streams",
      options: decrypt("r"),
      readable: new ReadableStream<EncodedFrame>({
        start(c): void {
          controller = c;
        },
      }),
      writable: first.writable,
    });
    await vi.waitFor(() => expect(h.core.liveCount()).toBe(1));
    const second = sink();
    const readable = source([frame(OPUS, undefined, {})]);
    h.core.onMessage({ type: "encoded-streams", options: decrypt("r"), readable, writable: second.writable });
    await settle();
    expect(readable.locked).toBe(false);
    expect(second.got).toEqual([]);
    expect(h.lines().some((l) => l.includes("already in use"))).toBe(true);
    (controller as ReadableStreamDefaultController<EncodedFrame> | null)?.close();
    await first.done;
    await vi.waitFor(() => expect(h.core.liveCount()).toBe(0));
  });

  it("an rtctransform event with no usable options connects nothing", async () => {
    const h = host();
    const out = sink();
    const readable = source([frame(OPUS, undefined, {})]);
    h.core.onTransformer({ readable, writable: out.writable, options: undefined });
    await settle();
    expect(readable.locked).toBe(false);
    expect(out.got).toEqual([]);
  });
});
