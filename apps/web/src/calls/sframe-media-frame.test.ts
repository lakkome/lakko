// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The sealed media frame of ADR-0071 through the real wasm core, the way the
 * transform worker drives it: the codec's own frame header stays in the clear
 * at the start of the frame, the SFrame header sits at the end, and every
 * clear byte is authenticated.
 *
 * Nothing here runs a browser. What it shows is that the bytes a browser's
 * RTP stack would be handed start with the VP8 header; whether Firefox's or
 * Safari's receive path then decodes is not something this suite can see.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, {
  CallKey,
  CallKeyRing,
  MediaCodec,
  mediaClearPrefixLen,
  mediaCodecFromMime,
  parseMediaFrame,
  redPrimary,
  vp8IsKeyFrame,
} from "../../wasm/lakko_ffi_wasm";
import {
  createCodecResolver,
  MEDIA_CODEC_OTHER,
  senderCodecEntries,
  type CodecFrame,
  type CodecResolver,
} from "./sframe-codec";
import { openFrame, sealFrame, type SealState } from "./sframe-seal-stream";
import { redReceive } from "./sframe-worker-core";

const SEED = new Uint8Array(32).fill(0x42);
const LEAF = 3;
const CAMERA = 1;
const KID = BigInt((LEAF << 8) | CAMERA);

/** The core's golden frame: seed 0x42 x 32, leaf 3, stream 1, counter 1. */
const GOLDEN_MEDIA_FRAME_HEX =
  "1002009d012a8002e0012dc56b47f2bfd3ebf051d8893d6d2b2b355f29f3980b60" +
  "e19c2555c9ad3a6ca9e95f08b228768c0f59a6509103010a0301";

const unhex = (hex: string): Uint8Array =>
  Uint8Array.from(hex.match(/.{2}/g) ?? [], (b) => Number.parseInt(b, 16));

const concat = (...parts: readonly (readonly number[] | Uint8Array)[]): Uint8Array =>
  Uint8Array.from(parts.flatMap((p) => Array.from(p)));

/** A VP8 key frame: tag (low bit 0), start code, 640 x 480, then data. */
const VP8_KEY = concat(
  [0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01],
  new TextEncoder().encode("lakko media frame golden v1"),
);
/** A VP8 delta frame: tag with the low bit 1. */
const VP8_DELTA = concat([0x11, 0x02, 0x00], new TextEncoder().encode("inter-frame residual"));

interface TestFrame extends CodecFrame {
  data: ArrayBuffer;
}

function videoFrame(bytes: Uint8Array, type: "key" | "delta", metadata: unknown): TestFrame {
  return { type, data: bytes.slice().buffer as ArrayBuffer, getMetadata: () => metadata };
}

interface Harness {
  readonly resolver: CodecResolver;
  readonly state: SealState;
  readonly lines: string[];
  readonly asked: { count: number };
}

function harness(mediaKind: "audio" | "video", stream = CAMERA): Harness {
  const lines: string[] = [];
  const asked = { count: 0 };
  const resolver = createCodecResolver({
    mediaKind,
    codecFromMime: (mime) => mediaCodecFromMime(mime),
    vp8: MediaCodec.Vp8,
    vp8IsKeyFrame: (first) => vp8IsKeyFrame(first),
    diag: (text) => lines.push(text),
    askForCodecs: () => {
      asked.count += 1;
    },
  });
  const state: SealState = {
    sealKey: new CallKeyRing(SEED).participantKey(LEAF, stream),
    codecOf: (frame, bytes) => resolver.codecOf(frame, bytes),
  };
  return { resolver, state, lines, asked };
}

const bytesOf = (frame: { data: ArrayBuffer }): Uint8Array => new Uint8Array(frame.data);

/** The worker's key-free read of a sealed frame. */
const readInfo = (wire: Uint8Array): { kid: bigint; isRedOpus: boolean } => {
  const info = parseMediaFrame(wire);
  return { kid: info.kid, isRedOpus: info.isRedOpus };
};

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("the core's values this client mirrors", () => {
  it("Other is 0, and MIME types map as the core says", () => {
    expect(MediaCodec.Other).toBe(MEDIA_CODEC_OTHER);
    expect(mediaCodecFromMime("video/VP8")).toBe(MediaCodec.Vp8);
    expect(mediaCodecFromMime("video/vp8")).toBe(MediaCodec.Vp8);
    expect(mediaCodecFromMime("audio/opus")).toBe(MediaCodec.Opus);
    expect(mediaCodecFromMime("video/H264")).toBe(MediaCodec.Other);
    expect(mediaCodecFromMime("video/VP9")).toBe(MediaCodec.Other);
    expect(mediaClearPrefixLen(MediaCodec.Vp8, VP8_KEY)).toBe(10);
    expect(mediaClearPrefixLen(MediaCodec.Vp8, VP8_DELTA)).toBe(3);
    expect(mediaClearPrefixLen(MediaCodec.Opus, VP8_KEY)).toBe(0);
    expect(mediaClearPrefixLen(MediaCodec.Other, VP8_KEY)).toBe(0);
  });

  it("opens the core's golden frame", () => {
    const wire = unhex(GOLDEN_MEDIA_FRAME_HEX);
    const info = parseMediaFrame(wire);
    expect(info.kid).toBe(KID);
    expect(info.counter).toBe(1n);
    expect(info.clearPrefixLen).toBe(10);
    expect(new CallKeyRing(SEED).openMediaFrameWithOverlap(wire)).toEqual(VP8_KEY);
    expect(new CallKey(SEED).participantKey(LEAF, CAMERA).openMediaFrame(wire)).toEqual(VP8_KEY);
  });
});

describe("sealing then opening a VP8 frame", () => {
  it("keeps the VP8 header at the start of a key frame and of a delta frame", () => {
    const { state } = harness("video");
    const key = videoFrame(VP8_KEY, "key", { mimeType: "video/VP8" });
    const delta = videoFrame(VP8_DELTA, "delta", { mimeType: "video/VP8" });
    expect(sealFrame(state, key)).toBe(true);
    expect(sealFrame(state, delta)).toBe(true);

    const keyWire = bytesOf(key);
    const deltaWire = bytesOf(delta);
    // What a receiver without the frame descriptor reads: the low bit of
    // byte 0 says key frame (0) or not (1), and a key frame's start code and
    // dimensions follow.
    expect(keyWire[0]! & 0x01).toBe(0);
    expect(deltaWire[0]! & 0x01).toBe(1);
    expect(vp8IsKeyFrame(keyWire)).toBe(true);
    expect(vp8IsKeyFrame(deltaWire)).toBe(false);
    expect(keyWire.slice(0, 10)).toEqual(VP8_KEY.slice(0, 10));
    expect(deltaWire.slice(0, 3)).toEqual(VP8_DELTA.slice(0, 3));
    expect(parseMediaFrame(keyWire).clearPrefixLen).toBe(10);
    expect(parseMediaFrame(deltaWire).clearPrefixLen).toBe(3);
    // The rest is not on the wire in the clear.
    const text = new TextDecoder("latin1").decode(keyWire);
    expect(text.includes("lakko media frame")).toBe(false);

    const ring = new CallKeyRing(SEED);
    const readKid = readInfo;
    expect(openFrame(ring, readKid, key)).toEqual({ what: "opened", kid: KID });
    expect(openFrame(ring, readKid, delta)).toEqual({ what: "opened", kid: KID });
    expect(bytesOf(key)).toEqual(VP8_KEY);
    expect(bytesOf(delta)).toEqual(VP8_DELTA);
  });

  it("drops a frame with any tampered region, and leaves its bytes alone", () => {
    const { state } = harness("video");
    const sealed = videoFrame(VP8_KEY, "key", { mimeType: "video/VP8" });
    expect(sealFrame(state, sealed)).toBe(true);
    const wire = bytesOf(sealed);
    const ring = new CallKeyRing(SEED);
    const readKid = readInfo;

    const tampered = (index: number, mask: number): { data: ArrayBuffer } => {
      const copy = wire.slice();
      copy[index] = copy[index]! ^ mask;
      return { data: copy.buffer as ArrayBuffer };
    };
    const n = wire.length;
    // A clear byte (key frame turned into a delta frame for a reader).
    const clear = tampered(0, 0x01);
    const before = clear.data;
    expect(openFrame(ring, readKid, clear).what).toBe("openFailed");
    expect(clear.data).toBe(before);
    // A clear byte (the advertised width), the ciphertext, the header.
    expect(openFrame(ring, readKid, tampered(6, 0x40)).what).toBe("openFailed");
    expect(openFrame(ring, readKid, tampered(12, 0x01)).what).toBe("openFailed");
    expect(openFrame(ring, readKid, tampered(n - 4, 0x01)).what).toBe("openFailed");
    // The trailer: the clear length moved by one still parses, and fails to
    // authenticate; the version byte does not parse at all.
    expect(openFrame(ring, readKid, tampered(n - 3, 0x01)).what).toBe("openFailed");
    expect(openFrame(ring, readKid, tampered(n - 1, 0xff)).what).toBe("badHeader");
    // Garbage, a truncated frame, another call's seed.
    expect(openFrame(ring, readKid, { data: new ArrayBuffer(0) }).what).toBe("badHeader");
    expect(openFrame(ring, readKid, { data: wire.slice(0, n - 1).buffer as ArrayBuffer }).what).not.toBe(
      "opened",
    );
    const foreign = new CallKeyRing(new Uint8Array(32).fill(0x07));
    expect(openFrame(foreign, readKid, { data: wire.slice().buffer as ArrayBuffer }).what).toBe(
      "openFailed",
    );
  });

  it("refuses a frame in the header-first layout", () => {
    const old = new CallKeyRing(SEED).participantKey(LEAF, CAMERA).sealFramed(VP8_KEY);
    const ring = new CallKeyRing(SEED);
    const outcome = openFrame(ring, readInfo, {
      data: old.slice().buffer as ArrayBuffer,
    });
    expect(outcome.what).not.toBe("opened");
  });

  it("opens an old-epoch frame inside the rekey window and not after it", () => {
    const { state } = harness("video");
    const sealed = videoFrame(VP8_KEY, "key", { mimeType: "video/VP8" });
    expect(sealFrame(state, sealed)).toBe(true);
    const wire = bytesOf(sealed);
    const ring = new CallKeyRing(SEED);
    ring.rekeyFromSeedAt(new Uint8Array(32).fill(0x09), 1000n, 1000n);
    expect(ring.openMediaFrameWithOverlap(wire)).toEqual(VP8_KEY);
    ring.pruneExpired(2000n);
    expect(() => ring.openMediaFrameWithOverlap(wire)).toThrow();
  });
});

describe("which codec a frame is sealed as", () => {
  it("takes the frame's own mimeType first", () => {
    const { resolver } = harness("video");
    const frame = videoFrame(VP8_KEY, "key", { mimeType: "video/VP8", payloadType: 96 });
    expect(resolver.codecOf(frame, VP8_KEY)).toBe(MediaCodec.Vp8);
  });

  it("else looks the payload type up in the sender's negotiated codecs", () => {
    const { resolver, state, asked, lines } = harness("video");
    const frame = videoFrame(VP8_KEY, "key", { payloadType: 96 });
    // Before the table arrives the codec is unknown: nothing clear, one
    // line, one request for the table. VP8 is not assumed.
    expect(sealFrame(state, frame)).toBe(true);
    expect(parseMediaFrame(bytesOf(frame)).clearPrefixLen).toBe(0);
    expect(asked.count).toBe(1);
    expect(lines).toHaveLength(1);

    resolver.setCodecs([
      { payloadType: 96, mimeType: "video/VP8" },
      { payloadType: 97, mimeType: "video/rtx" },
      { payloadType: 102, mimeType: "video/H264" },
    ]);
    const next = videoFrame(VP8_KEY, "key", { payloadType: 96 });
    expect(sealFrame(state, next)).toBe(true);
    expect(parseMediaFrame(bytesOf(next)).clearPrefixLen).toBe(10);
    expect(asked.count).toBe(1);

    // Another negotiated codec has no rule: nothing clear, and no request,
    // because the codec is known.
    const h264 = videoFrame(VP8_KEY, "key", { payloadType: 102 });
    expect(sealFrame(state, h264)).toBe(true);
    expect(parseMediaFrame(bytesOf(h264)).clearPrefixLen).toBe(0);
    expect(asked.count).toBe(1);
  });

  it("seals with nothing clear when a browser gives no metadata at all", () => {
    const { state, asked } = harness("video");
    const bare: TestFrame = { type: "key", data: VP8_KEY.slice().buffer as ArrayBuffer };
    const throwing: TestFrame = {
      type: "key",
      data: VP8_KEY.slice().buffer as ArrayBuffer,
      getMetadata: () => {
        throw new Error("no metadata");
      },
    };
    for (const frame of [bare, throwing]) {
      expect(sealFrame(state, frame)).toBe(true);
      expect(parseMediaFrame(bytesOf(frame)).clearPrefixLen).toBe(0);
    }
    expect(asked.count).toBe(1);
    // Still opens: the receiver never needs the codec.
    expect(new CallKeyRing(SEED).openMediaFrameWithOverlap(bytesOf(bare))).toEqual(VP8_KEY);
  });

  it("seals audio with nothing clear and never asks", () => {
    const { state, asked, lines } = harness("audio", 0);
    const opus = { data: Uint8Array.from([0x78, 1, 2, 3]).buffer as ArrayBuffer, getMetadata: () => ({ mimeType: "audio/opus" }) };
    const unknown = { data: Uint8Array.from([0x78, 1, 2, 3]).buffer as ArrayBuffer };
    for (const frame of [opus, unknown]) {
      expect(sealFrame(state, frame)).toBe(true);
      expect(parseMediaFrame(bytesOf(frame)).clearPrefixLen).toBe(0);
    }
    expect(asked.count).toBe(0);
    expect(lines).toHaveLength(0);
  });

  it("lets the VP8 frame tag decide when the browser's frame type disagrees, and says so", () => {
    const { state, lines } = harness("video");
    // The browser says "delta"; the tag says key frame: 10 bytes stay clear,
    // because that is what a receiver reading the tag will parse.
    const saysDelta = videoFrame(VP8_KEY, "delta", { mimeType: "video/VP8" });
    expect(sealFrame(state, saysDelta)).toBe(true);
    expect(parseMediaFrame(bytesOf(saysDelta)).clearPrefixLen).toBe(10);
    // The browser says "key"; the tag says delta: only 3 bytes stay clear.
    const saysKey = videoFrame(VP8_DELTA, "key", { mimeType: "video/VP8" });
    expect(sealFrame(state, saysKey)).toBe(true);
    expect(parseMediaFrame(bytesOf(saysKey)).clearPrefixLen).toBe(3);
    expect(lines).toHaveLength(1);
    expect(lines[0]).toContain("the tag decides");
    // Agreement says nothing.
    const agrees = videoFrame(VP8_KEY, "key", { mimeType: "video/VP8" });
    expect(sealFrame(state, agrees)).toBe(true);
    expect(lines).toHaveLength(1);
  });

  it("seals a frame shorter than its prefix, and an empty one", () => {
    const { state } = harness("video");
    const ring = new CallKeyRing(SEED);
    for (const bytes of [Uint8Array.from([0x10, 0x02, 0x00, 0x9d]), Uint8Array.from([0x11]), new Uint8Array(0)]) {
      const frame = videoFrame(bytes, "key", { mimeType: "video/VP8" });
      expect(sealFrame(state, frame)).toBe(true);
      expect(parseMediaFrame(bytesOf(frame)).clearPrefixLen).toBe(bytes.length);
      expect(ring.openMediaFrameWithOverlap(bytesOf(frame))).toEqual(bytes);
    }
  });
});

describe("reading a sender's negotiated codecs", () => {
  it("keeps payload type and MIME type, and survives a sender that gives none", () => {
    expect(
      senderCodecEntries({
        getParameters: () => ({
          codecs: [
            { payloadType: 96, mimeType: "video/VP8" },
            { payloadType: "x", mimeType: "video/VP9" },
            { payloadType: 111 },
          ],
        }),
      }),
    ).toEqual([{ payloadType: 96, mimeType: "video/VP8" }]);
    expect(senderCodecEntries({})).toEqual([]);
    expect(senderCodecEntries({ getParameters: () => ({}) })).toEqual([]);
    expect(
      senderCodecEntries({
        getParameters: () => {
          throw new Error("closed");
        },
      }),
    ).toEqual([]);
  });
});

describe("audio RED containers (ADR-0071, amended 2026-10-26)", () => {
  /** A container as Chrome's encoder hands it over with `audio/red`
   *  negotiated: one redundant block of 3 bytes, then the primary. */
  const CONTAINER = concat([0xef, 0x0f, 0x00, 0x03, 0x6f], [9, 9, 9], new TextEncoder().encode("primary"));
  const PRIMARY = new TextEncoder().encode("primary");
  const red = redReceive({ redPrimary }, false);
  const redWhereBrowserDecodes = redReceive({ redPrimary }, true);

  const audioFrame = (bytes: Uint8Array, metadata: unknown): TestFrame => ({
    data: bytes.slice().buffer as ArrayBuffer,
    getMetadata: () => metadata,
  });

  const sealedRed = (): Uint8Array => {
    const { state } = harness("audio", 0);
    const frame = audioFrame(CONTAINER, { mimeType: "audio/red", payloadType: 63 });
    expect(sealFrame(state, frame)).toBe(true);
    return bytesOf(frame);
  };

  it("a sender seals a RED container marked as one, with nothing in the clear", () => {
    expect(mediaCodecFromMime("audio/red")).toBe(MediaCodec.RedOpus);
    const wire = sealedRed();
    const info = parseMediaFrame(wire);
    expect(info.isRedOpus).toBe(true);
    expect(info.clearPrefixLen).toBe(0);
    const { state } = harness("audio", 0);
    const opus = audioFrame(PRIMARY, { mimeType: "audio/opus", payloadType: 111 });
    expect(sealFrame(state, opus)).toBe(true);
    expect(parseMediaFrame(bytesOf(opus)).isRedOpus).toBe(false);
  });

  it("a receiver whose decoder expects Opus gets the primary frame", () => {
    const ring = new CallKeyRing(SEED);
    const frame = audioFrame(sealedRed(), { mimeType: "audio/opus", payloadType: 109 });
    expect(openFrame(ring, readInfo, frame, red).what).toBe("opened");
    expect(bytesOf(frame)).toEqual(PRIMARY);
  });

  it("a receiver that negotiated RED gets the container whole", () => {
    const ring = new CallKeyRing(SEED);
    const frame = audioFrame(sealedRed(), { mimeType: "audio/red", payloadType: 63 });
    expect(openFrame(ring, readInfo, frame, red).what).toBe("opened");
    expect(bytesOf(frame)).toEqual(CONTAINER);
  });

  it("with no codec in the metadata, the browser's own RED support decides", () => {
    const ring = new CallKeyRing(SEED);
    const firefox = audioFrame(sealedRed(), {});
    expect(openFrame(ring, readInfo, firefox, red).what).toBe("opened");
    expect(bytesOf(firefox)).toEqual(PRIMARY);
    const chrome = audioFrame(sealedRed(), {});
    expect(openFrame(ring, readInfo, chrome, redWhereBrowserDecodes).what).toBe("opened");
    expect(bytesOf(chrome)).toEqual(CONTAINER);
  });

  it("an Opus frame is never read as a container", () => {
    const { state } = harness("audio", 0);
    const opus = audioFrame(CONTAINER, { mimeType: "audio/opus", payloadType: 111 });
    expect(sealFrame(state, opus)).toBe(true);
    const ring = new CallKeyRing(SEED);
    expect(openFrame(ring, readInfo, opus, red).what).toBe("opened");
    expect(bytesOf(opus)).toEqual(CONTAINER);
  });
});
