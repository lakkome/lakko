// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Attaching the SFrame transform on a browser that has no
 * `RTCRtpScriptTransform` but has the insertable-streams interface
 * (`createEncodedStreams()`), as Edge 133 was seen to be.
 *
 * Headless: the sender, the receiver, the streams and the worker are fakes.
 * What is pinned is what the main thread does with them: the streams go to
 * the worker with the SAME options the script-transform path passes, once per
 * sender or receiver, and nothing is attached when the browser has neither
 * interface.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  attachReceiverSFrame,
  attachSenderSFrame,
  coverEncodedStreams,
  disposeSFrameWorker,
  isSFrameTransformSupported,
  rekeySFrame,
  SFRAME_STREAM_CAMERA,
  SFRAME_STREAM_CAMERA_LIGHT,
  SFRAME_STREAM_CAMERA_MIDDLE,
  type SFrameTransformOptions,
} from "./sframe-transform";

interface Posted {
  readonly message: Record<string, unknown>;
  readonly transfer: readonly unknown[] | undefined;
}

/** Everything the main thread posted to the (stubbed) worker. */
const posted: Posted[] = [];
/** Options the (stubbed) `RTCRtpScriptTransform` was constructed with. */
const scripted: SFrameTransformOptions[] = [];

class FakeEndpoint {
  transform: unknown = null;
  taken = 0;
  readonly streams = { readable: { r: Math.random() }, writable: { w: Math.random() } };
  constructor(readonly track: { kind: string } | null) {}
  createEncodedStreams(): { readable: unknown; writable: unknown } {
    this.taken += 1;
    // The real interface throws on a second call.
    if (this.taken > 1) throw new Error("InvalidStateError: streams already created");
    return this.streams;
  }
}

const asSender = (e: FakeEndpoint): RTCRtpSender => e as unknown as RTCRtpSender;
const asReceiver = (e: FakeEndpoint): RTCRtpReceiver => e as unknown as RTCRtpReceiver;

function stubWorker(): void {
  vi.stubGlobal(
    "Worker",
    class {
      addEventListener(): void {}
      postMessage(message: Record<string, unknown>, transfer?: readonly unknown[]): void {
        posted.push({ message, transfer });
      }
      terminate(): void {}
    },
  );
}

/** A page with the insertable-streams interface and no script transform. */
function stubEncodedStreamsBrowser(): void {
  vi.stubGlobal("RTCRtpSender", FakeEndpoint);
  vi.stubGlobal("RTCRtpReceiver", FakeEndpoint);
}

function stubScriptTransformBrowser(): void {
  vi.stubGlobal(
    "RTCRtpScriptTransform",
    class {
      constructor(_worker: unknown, options: SFrameTransformOptions) {
        scripted.push(options);
      }
    },
  );
}

const seed = new Uint8Array(32).fill(5);
const ofType = (type: string): Posted[] => posted.filter((p) => p.message.type === type);
const withoutId = (o: SFrameTransformOptions): Omit<SFrameTransformOptions, "id"> => {
  const { id: _id, ...rest } = o;
  return rest;
};

beforeEach(() => {
  posted.length = 0;
  scripted.length = 0;
  stubWorker();
});

afterEach(() => {
  disposeSFrameWorker();
  vi.unstubAllGlobals();
});

describe("on a browser with createEncodedStreams and no RTCRtpScriptTransform", () => {
  beforeEach(stubEncodedStreamsBrowser);

  it("is supported", () => {
    expect(isSFrameTransformSupported()).toBe(true);
  });

  it("takes a sender's streams and transfers them to the worker with its options", () => {
    const mic = new FakeEndpoint({ kind: "audio" });
    const id = attachSenderSFrame(asSender(mic), { seed, localLeaf: 4 });
    expect(mic.taken).toBe(1);
    // No script transform is set on this path.
    expect(mic.transform).toBeNull();
    const messages = ofType("encoded-streams");
    expect(messages).toHaveLength(1);
    const { message, transfer } = messages[0]!;
    expect(message.readable).toBe(mic.streams.readable);
    expect(message.writable).toBe(mic.streams.writable);
    expect(transfer).toEqual([mic.streams.readable, mic.streams.writable]);
    expect(message.options).toEqual({
      operation: "encrypt",
      id,
      seed,
      localLeaf: 4,
      streamIndex: 0,
      mediaKind: "audio",
    });
  });

  it("takes a receiver's streams and transfers them with the decrypt options", () => {
    const receiver = new FakeEndpoint({ kind: "video" });
    const id = attachReceiverSFrame(asReceiver(receiver), { seed });
    expect(receiver.taken).toBe(1);
    const messages = ofType("encoded-streams");
    expect(messages).toHaveLength(1);
    expect(messages[0]!.message.options).toEqual({ operation: "decrypt", id, seed, redDecodes: false });
    expect(messages[0]!.transfer).toEqual([receiver.streams.readable, receiver.streams.writable]);
  });

  it("passes the worker the same options the script-transform path passes", () => {
    const attachAll = (): void => {
      attachSenderSFrame(asSender(new FakeEndpoint({ kind: "audio" })), { seed, localLeaf: 9 });
      attachSenderSFrame(asSender(new FakeEndpoint({ kind: "video" })), { seed, localLeaf: 9 });
      attachSenderSFrame(asSender(new FakeEndpoint({ kind: "video" })), {
        seed,
        localLeaf: 9,
        streamIndex: SFRAME_STREAM_CAMERA_LIGHT,
      });
      attachSenderSFrame(asSender(new FakeEndpoint({ kind: "video" })), {
        seed,
        localLeaf: 9,
        streamIndex: SFRAME_STREAM_CAMERA_MIDDLE,
      });
      attachReceiverSFrame(asReceiver(new FakeEndpoint({ kind: "video" })), { seed });
    };
    attachAll();
    const viaStreams = ofType("encoded-streams").map(
      (p) => p.message.options as SFrameTransformOptions,
    );
    disposeSFrameWorker();
    stubScriptTransformBrowser();
    attachAll();
    expect(scripted).toHaveLength(5);
    expect(viaStreams.map(withoutId)).toEqual(scripted.map(withoutId));
    expect(viaStreams.map((o) => o.streamIndex)).toEqual([0, 1, 3, 4, undefined]);
  });

  it("takes a receiver's streams once: a second track event returns the same transform", () => {
    const receiver = new FakeEndpoint({ kind: "audio" });
    const first = attachReceiverSFrame(asReceiver(receiver), { seed });
    const second = attachReceiverSFrame(asReceiver(receiver), { seed });
    expect(second).toBe(first);
    expect(receiver.taken).toBe(1);
    expect(ofType("encoded-streams")).toHaveLength(1);
    // Same seed: nothing to rotate.
    expect(ofType("rekey")).toHaveLength(0);
  });

  it("brings a transform that is attached again to the newer seed", () => {
    const receiver = new FakeEndpoint({ kind: "audio" });
    const id = attachReceiverSFrame(asReceiver(receiver), { seed });
    const newer = new Uint8Array(32).fill(6);
    expect(attachReceiverSFrame(asReceiver(receiver), { seed: newer })).toBe(id);
    const rekeys = ofType("rekey");
    expect(rekeys).toHaveLength(1);
    expect(rekeys[0]!.message.id).toBe(id);
    expect(rekeys[0]!.message.seed).toBe(newer);
    // A transform already re-keyed to that seed by the session is left alone.
    expect(attachReceiverSFrame(asReceiver(receiver), { seed: newer })).toBe(id);
    expect(ofType("rekey")).toHaveLength(1);
    const third = new Uint8Array(32).fill(7);
    rekeySFrame(id, third);
    expect(attachReceiverSFrame(asReceiver(receiver), { seed: third })).toBe(id);
    expect(ofType("rekey")).toHaveLength(2);
    expect(receiver.taken).toBe(1);
  });

  it("moves a sender that is attached again under another stream index, never two transforms", () => {
    const sender = new FakeEndpoint({ kind: "video" });
    const id = attachSenderSFrame(asSender(sender), {
      seed,
      localLeaf: 4,
      streamIndex: SFRAME_STREAM_CAMERA_LIGHT,
    });
    expect(attachSenderSFrame(asSender(sender), { seed, localLeaf: 4 })).toBe(id);
    expect(sender.taken).toBe(1);
    expect(ofType("encoded-streams")).toHaveLength(1);
    expect(ofType("retarget").map((p) => p.message)).toEqual([
      { type: "retarget", id, localLeaf: 4, streamIndex: SFRAME_STREAM_CAMERA },
    ]);
    // The same role again: nothing more is said to the worker.
    expect(attachSenderSFrame(asSender(sender), { seed, localLeaf: 4 })).toBe(id);
    expect(ofType("retarget")).toHaveLength(1);
  });

  it("rekey reaches a transform created over transferred streams, by its id", () => {
    const id = attachSenderSFrame(asSender(new FakeEndpoint({ kind: "audio" })), { seed, localLeaf: 1 });
    const newer = new Uint8Array(32).fill(8);
    rekeySFrame(id, newer);
    const rekeys = ofType("rekey");
    expect(rekeys).toHaveLength(1);
    expect(rekeys[0]!.message).toMatchObject({ type: "rekey", id, seed: newer, overlapMs: 1000 });
  });

  it("refuses an index outside one byte before taking any streams", () => {
    const sender = new FakeEndpoint({ kind: "video" });
    expect(() => attachSenderSFrame(asSender(sender), { seed, localLeaf: 4, streamIndex: 256 })).toThrow();
    expect(sender.taken).toBe(0);
    expect(posted).toHaveLength(0);
  });

  it("lets the browser's refusal through when the connection was not built for it", () => {
    const sender = new FakeEndpoint({ kind: "audio" });
    sender.taken = 1; // the next call throws, as on a connection without the flag
    expect(() => attachSenderSFrame(asSender(sender), { seed, localLeaf: 4 })).toThrow(/InvalidStateError/);
    expect(ofType("encoded-streams")).toHaveLength(0);
  });

  describe("coverEncodedStreams", () => {
    it("attaches every receiver nothing attached, and reports a sender with a track and no transform", () => {
      const mic = new FakeEndpoint({ kind: "audio" });
      const camera = new FakeEndpoint({ kind: "video" });
      const idle = new FakeEndpoint(null);
      const announced = new FakeEndpoint({ kind: "audio" });
      const unannounced = new FakeEndpoint({ kind: "video" });
      attachSenderSFrame(asSender(mic), { seed, localLeaf: 2 });
      const announcedId = attachReceiverSFrame(asReceiver(announced), { seed });
      const pc = {
        getSenders: () => [mic, camera, idle].map(asSender),
        getReceivers: () => [announced, unannounced].map(asReceiver),
      };
      const cover = coverEncodedStreams(pc, seed);
      expect(cover.unsealedSenders).toEqual([asSender(camera)]);
      expect(cover.attachedReceiverIds).toHaveLength(1);
      expect(cover.attachedReceiverIds[0]).not.toBe(announcedId);
      expect(unannounced.taken).toBe(1);
      expect(announced.taken).toBe(1);
      // The unsealed sender's streams are NOT taken here: its index is its
      // caller's to state, and untaken it sends nothing.
      expect(camera.taken).toBe(0);

      // Once everything is covered, a second look does nothing.
      attachSenderSFrame(asSender(camera), { seed, localLeaf: 2 });
      const again = coverEncodedStreams(pc, seed);
      expect(again).toEqual({ attachedReceiverIds: [], unsealedSenders: [] });
      expect(unannounced.taken).toBe(1);
    });

    it("does not throw on a closed connection or a receiver that refuses", () => {
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      const refusing = new FakeEndpoint({ kind: "audio" });
      refusing.taken = 1;
      expect(
        coverEncodedStreams(
          { getSenders: () => [], getReceivers: () => [asReceiver(refusing)] },
          seed,
        ),
      ).toEqual({ attachedReceiverIds: [], unsealedSenders: [] });
      expect(warn).toHaveBeenCalledTimes(1);
      expect(
        coverEncodedStreams(
          {
            getSenders: () => {
              throw new Error("closed");
            },
            getReceivers: () => [],
          },
          seed,
        ),
      ).toEqual({ attachedReceiverIds: [], unsealedSenders: [] });
      warn.mockRestore();
    });
  });
});

describe("on a browser with RTCRtpScriptTransform", () => {
  beforeEach(() => {
    stubEncodedStreamsBrowser();
    stubScriptTransformBrowser();
  });

  it("uses the script transform and never takes the encoded streams", () => {
    const sender = new FakeEndpoint({ kind: "video" });
    const receiver = new FakeEndpoint({ kind: "video" });
    attachSenderSFrame(asSender(sender), { seed, localLeaf: 4 });
    attachReceiverSFrame(asReceiver(receiver), { seed });
    expect(scripted.map((o) => o.operation)).toEqual(["encrypt", "decrypt"]);
    expect(sender.transform).not.toBeNull();
    expect(receiver.transform).not.toBeNull();
    expect(sender.taken).toBe(0);
    expect(receiver.taken).toBe(0);
    expect(ofType("encoded-streams")).toHaveLength(0);
  });

  it("coverEncodedStreams does nothing", () => {
    const receiver = new FakeEndpoint({ kind: "video" });
    const sender = new FakeEndpoint({ kind: "video" });
    expect(
      coverEncodedStreams(
        { getSenders: () => [asSender(sender)], getReceivers: () => [asReceiver(receiver)] },
        seed,
      ),
    ).toEqual({ attachedReceiverIds: [], unsealedSenders: [] });
    expect(receiver.taken).toBe(0);
    expect(scripted).toHaveLength(0);
  });
});

describe("on a browser with neither interface", () => {
  it("is unsupported, and every attach refuses with nothing posted (invariant #4)", () => {
    expect(isSFrameTransformSupported()).toBe(false);
    const sender = new FakeEndpoint({ kind: "audio" });
    const receiver = new FakeEndpoint({ kind: "audio" });
    expect(() => attachSenderSFrame(asSender(sender), { seed, localLeaf: 4 })).toThrow(/invariant #4/);
    expect(() => attachReceiverSFrame(asReceiver(receiver), { seed })).toThrow(/invariant #4/);
    expect(sender.taken).toBe(0);
    expect(receiver.taken).toBe(0);
    expect(sender.transform).toBeNull();
    expect(posted).toHaveLength(0);
  });

  it("is unsupported when only senders can take their streams", () => {
    vi.stubGlobal("RTCRtpSender", FakeEndpoint);
    expect(isSFrameTransformSupported()).toBe(false);
  });
});
