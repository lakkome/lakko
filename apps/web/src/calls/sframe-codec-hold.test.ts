// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A video frame is not sealed before its codec is known (ADR-0071).
 *
 * The sender's codec table reaches the worker from the main thread after the
 * SFU's answer is applied; the encoder's first frame, a key frame, can get
 * there first. Sealed with nothing in the clear it is no use to a receiver
 * without the frame descriptor, which then has to ask for another. So the
 * frame is held for the table (`CodecResolver.ready`, `sealingStream`).
 */
import { describe, expect, it } from "vitest";

import {
  CODEC_WAIT_MS,
  createCodecResolver,
  MEDIA_CODEC_OTHER,
  type CodecFrame,
} from "./sframe-codec";
import { sealingStream, type SealState } from "./sframe-seal-stream";

const VP8 = 1;

function make(mediaKind: "audio" | "video" = "video") {
  const lines: string[] = [];
  const timers: { fn: () => void; ms: number }[] = [];
  let asked = 0;
  const resolver = createCodecResolver({
    mediaKind,
    codecFromMime: (mime) => (mime === "video/vp8" ? VP8 : MEDIA_CODEC_OTHER),
    vp8: VP8,
    vp8IsKeyFrame: (first) => ((first[0] ?? 1) & 1) === 0,
    diag: (text) => lines.push(text),
    askForCodecs: () => {
      asked += 1;
    },
    setTimeout: (fn, ms) => timers.push({ fn, ms }),
  });
  return { resolver, lines, timers, asked: () => asked };
}

/** A frame whose metadata names only its payload type (no `mimeType`). */
const byPt = (payloadType: number, type = "key"): CodecFrame => ({
  type,
  getMetadata: () => ({ payloadType }),
});

/** Let every settled promise's continuation run. */
const settle = async (): Promise<void> => {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
};

describe("CodecResolver.ready", () => {
  it("does not hold a frame whose codec is known, nor an audio frame", () => {
    const { resolver, asked } = make();
    expect(resolver.ready({ type: "key", getMetadata: () => ({ mimeType: "video/VP8" }) })).toBeNull();
    resolver.setCodecs([{ payloadType: 96, mimeType: "video/VP8" }]);
    expect(resolver.ready(byPt(96))).toBeNull();
    expect(asked()).toBe(0);

    const audio = make("audio");
    expect(audio.resolver.ready(byPt(111))).toBeNull();
    expect(audio.timers).toHaveLength(0);
  });

  it("holds a video frame of unknown codec until the table arrives", async () => {
    const { resolver, lines, timers, asked } = make();
    const wait = resolver.ready(byPt(96));
    expect(wait).not.toBeNull();
    expect(asked()).toBe(1);
    expect(timers.map((t) => t.ms)).toEqual([CODEC_WAIT_MS]);
    expect(lines).toHaveLength(1);
    expect(lines[0]).toContain("arrived before its codec was known");
    // A frame that asks meanwhile waits on the same promise; nothing is
    // asked or said twice.
    expect(resolver.ready(byPt(96))).toBe(wait);
    expect(asked()).toBe(1);

    let done = false;
    void wait?.then(() => {
      done = true;
    });
    await settle();
    expect(done).toBe(false);
    resolver.setCodecs([{ payloadType: 96, mimeType: "video/VP8" }]);
    await settle();
    expect(done).toBe(true);
    // Known now: sealed as VP8, and no further hold.
    expect(resolver.codecOf(byPt(96), new Uint8Array([0x10, 0, 0]))).toBe(VP8);
    expect(resolver.ready(byPt(96))).toBeNull();
    // The timer that fires later changes nothing.
    timers[0]?.fn();
    expect(resolver.ready(byPt(96))).toBeNull();
    expect(lines).toHaveLength(1);
  });

  it("gives up after the wait, says so, and does not hold again until a table arrives", async () => {
    const { resolver, lines, timers, asked } = make();
    const wait = resolver.ready(byPt(96));
    let done = false;
    void wait?.then(() => {
      done = true;
    });
    timers[0]?.fn();
    await settle();
    expect(done).toBe(true);
    expect(lines[1]).toContain(`no codec table came within ${CODEC_WAIT_MS} ms`);
    // Not held again: one wait, not one per frame.
    expect(resolver.ready(byPt(96))).toBeNull();
    expect(resolver.codecOf(byPt(96), new Uint8Array([0x10]))).toBe(MEDIA_CODEC_OTHER);
    expect(timers).toHaveLength(1);
    // A table arrives late: frames resolve, and nothing waits.
    resolver.setCodecs([{ payloadType: 96, mimeType: "video/VP8" }]);
    expect(resolver.ready(byPt(96))).toBeNull();
    expect(resolver.codecOf(byPt(96), new Uint8Array([0x10]))).toBe(VP8);
    expect(asked()).toBeGreaterThanOrEqual(1);
  });

  it("does not hold every frame for a table that lacks the frame's payload type", async () => {
    const { resolver, timers } = make();
    const wait = resolver.ready(byPt(120));
    expect(wait).not.toBeNull();
    resolver.setCodecs([{ payloadType: 96, mimeType: "video/VP8" }]);
    await settle();
    // Still unknown after the table came: one more hold is allowed for it...
    const second = resolver.ready(byPt(120));
    expect(second).not.toBeNull();
    timers[1]?.fn();
    await settle();
    // ...and then no more, until a new table.
    expect(resolver.ready(byPt(120))).toBeNull();
    expect(resolver.codecOf(byPt(120), new Uint8Array([0x10]))).toBe(MEDIA_CODEC_OTHER);
    expect(resolver.ready(byPt(120))).toBeNull();
    expect(timers).toHaveLength(2);
  });
});

describe("sealingStream with a hold", () => {
  interface TestFrame extends CodecFrame {
    data: ArrayBuffer;
    readonly name: string;
  }
  const testFrame = (name: string): TestFrame => ({
    name,
    type: "key",
    data: new Uint8Array([0x10, 1, 2, 3]).buffer as ArrayBuffer,
    getMetadata: () => ({ payloadType: 96 }),
  });

  it("seals nothing while the first frame waits, then every frame in order, with its codec", async () => {
    const { resolver } = make();
    const sealedAs: { name: string; codec: number }[] = [];
    let current = "";
    const state: SealState = {
      sealKey: {
        sealMediaFrame: (frame, codec) => {
          sealedAs.push({ name: current, codec });
          return frame;
        },
      },
      codecOf: (frame, bytes) => {
        current = (frame as TestFrame).name;
        return resolver.codecOf(frame, bytes);
      },
      ready: (frame) => resolver.ready(frame),
    };
    const stream = sealingStream<TestFrame>(state);
    const out: string[] = [];
    const reading = (async (): Promise<void> => {
      const reader = stream.readable.getReader();
      for (;;) {
        const next = await reader.read();
        if (next.done) return;
        out.push(next.value.name);
      }
    })();
    const writer = stream.writable.getWriter();
    const writes = [writer.write(testFrame("key0")), writer.write(testFrame("d1")), writer.write(testFrame("d2"))];
    await settle();
    // Held: not sealed blind, and the frames behind it wait their turn.
    expect(sealedAs).toEqual([]);
    expect(out).toEqual([]);

    resolver.setCodecs([{ payloadType: 96, mimeType: "video/VP8" }]);
    await Promise.all(writes);
    await writer.close();
    await reading;
    expect(out).toEqual(["key0", "d1", "d2"]);
    expect(sealedAs).toEqual([
      { name: "key0", codec: VP8 },
      { name: "d1", codec: VP8 },
      { name: "d2", codec: VP8 },
    ]);
  });

  it("a frame held in vain is still sealed, never passed on unsealed, and never dropped for the wait", async () => {
    const { resolver, timers } = make();
    const codecs: number[] = [];
    const state: SealState = {
      sealKey: {
        sealMediaFrame: (frame, codec) => {
          codecs.push(codec);
          return new Uint8Array([...frame, 0xaa]);
        },
      },
      codecOf: (frame, bytes) => resolver.codecOf(frame, bytes),
      ready: (frame) => resolver.ready(frame),
    };
    const stream = sealingStream<TestFrame>(state);
    const writer = stream.writable.getWriter();
    // A transform stream starts a frame only once a reader asks for one.
    const reading = stream.readable.getReader().read();
    const write = writer.write(testFrame("key0"));
    await settle();
    expect(codecs).toEqual([]);
    expect(timers).toHaveLength(1);
    timers[0]?.fn();
    await write;
    const got = await reading;
    expect(got.done).toBe(false);
    // Sealed (the stand-in's mark is there), with nothing in the clear.
    expect(new Uint8Array((got.value as TestFrame).data).at(-1)).toBe(0xaa);
    expect(codecs).toEqual([MEDIA_CODEC_OTHER]);
  });

  it("without a key the held frame is dropped like any other", async () => {
    const { resolver } = make();
    const state: SealState = { sealKey: null, ready: (frame) => resolver.ready(frame) };
    const stream = sealingStream<TestFrame>(state);
    const writer = stream.writable.getWriter();
    const reading = stream.readable.getReader().read();
    const write = writer.write(testFrame("key0"));
    await settle();
    resolver.setCodecs([{ payloadType: 96, mimeType: "video/VP8" }]);
    await write;
    await writer.close();
    const got = await reading;
    expect(got.done).toBe(true);
  });
});
