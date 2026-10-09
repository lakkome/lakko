// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A simulcast camera sender has two layers and one SFrame transform. Every
 * frame of either layer must be sealed with a counter value of its own, under
 * the one key, and stay the frame object the browser handed in.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { CallKeyRing, parseMediaFrame } from "../../wasm/lakko_ffi_wasm";
import { sealFrame, sealingStream, type SealState } from "./sframe-seal-stream";

const SEED = new Uint8Array(32).fill(0x42);
const LEAF = 3;
const VIDEO_STREAM = 1;

/** A stand-in for an encoded video frame: its bytes and the layer it is of. */
interface LayerFrame {
  data: ArrayBuffer;
  readonly rid: "q" | "f";
}

const frame = (rid: "q" | "f", text: string): LayerFrame => ({
  rid,
  data: new TextEncoder().encode(text).buffer as ArrayBuffer,
});

async function pump(state: SealState, frames: readonly LayerFrame[]): Promise<LayerFrame[]> {
  const stream = sealingStream<LayerFrame>(state);
  const out: LayerFrame[] = [];
  const reading = (async (): Promise<void> => {
    const reader = stream.readable.getReader();
    for (;;) {
      const next = await reader.read();
      if (next.done) return;
      out.push(next.value);
    }
  })();
  const writer = stream.writable.getWriter();
  for (const f of frames) await writer.write(f);
  await writer.close();
  await reading;
  return out;
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("sealing a simulcast sender's frames", () => {
  it("gives every frame of either layer its own counter under one key", async () => {
    const ring = new CallKeyRing(SEED);
    const state: SealState = { sealKey: ring.participantKey(LEAF, VIDEO_STREAM) };
    // The encoders' output interleaved, as one sender's transform sees it.
    const input = [
      frame("q", "q0"),
      frame("f", "f0"),
      frame("q", "q1"),
      frame("f", "f1"),
      frame("f", "f2"),
      frame("q", "q2"),
    ];
    const sealed = await pump(state, input);

    // The same objects, in order: the layer a frame belongs to is untouched.
    expect(sealed).toHaveLength(input.length);
    sealed.forEach((f, i) => expect(f).toBe(input[i]));
    expect(sealed.map((f) => f.rid)).toEqual(["q", "f", "q", "f", "f", "q"]);

    const headers = sealed.map((f) => parseMediaFrame(new Uint8Array(f.data)));
    // One counter, consumed once per frame whichever layer it is of.
    // The counter is 64-bit, a bigint (ADR-0069); the key starts at a random
    // value below 2^62, so the first frame's counter is read, not assumed.
    const counters: bigint[] = headers.map((h) => h.counter);
    const start = counters[0] ?? -1n;
    expect(start >= 1n && start <= 1n << 62n).toBe(true);
    expect(counters).toEqual([0n, 1n, 2n, 3n, 4n, 5n].map((i) => start + i));
    expect(new Set(counters).size).toBe(input.length);
    // One key for both layers: the same KID on every frame.
    const kid = BigInt((LEAF << 8) | VIDEO_STREAM);
    expect(headers.map((h) => h.kid)).toEqual(Array(input.length).fill(kid));
    // No codec was stated, so nothing is left in the clear.
    expect(headers.map((h) => h.clearPrefixLen)).toEqual(Array(input.length).fill(0));

    // A viewer receives one layer only: rising counters with gaps, and each
    // frame opens on its own.
    const receiver = new CallKeyRing(SEED);
    const opened = sealed
      .filter((f) => f.rid === "q")
      .map((f) => new TextDecoder().decode(receiver.openMediaFrameWithOverlap(new Uint8Array(f.data))));
    expect(opened).toEqual(["q0", "q1", "q2"]);
  });

  it("drops a frame rather than pass it on unsealed", async () => {
    const plain = frame("f", "plain");
    const before = plain.data;
    expect(sealFrame({ sealKey: null }, plain)).toBe(false);
    expect(plain.data).toBe(before);
    expect(await pump({ sealKey: null }, [plain])).toEqual([]);

    const failing: SealState = {
      sealKey: {
        sealMediaFrame: () => {
          throw new Error("counter exhausted");
        },
      },
    };
    expect(await pump(failing, [frame("q", "x"), frame("f", "y")])).toEqual([]);
  });

  it("seals the next frame under a key swapped in by a rekey", async () => {
    const ring = new CallKeyRing(SEED);
    const state: SealState = { sealKey: ring.participantKey(LEAF, VIDEO_STREAM) };
    const first = frame("q", "before");
    expect(sealFrame(state, first)).toBe(true);
    const other = new CallKeyRing(new Uint8Array(32).fill(0x07));
    state.sealKey = other.participantKey(LEAF, VIDEO_STREAM);
    const second = frame("f", "after");
    expect(sealFrame(state, second)).toBe(true);
    expect(new TextDecoder().decode(other.openMediaFrameWithOverlap(new Uint8Array(second.data)))).toBe("after");
    expect(() => other.openMediaFrameWithOverlap(new Uint8Array(first.data))).toThrow();
  });
});
