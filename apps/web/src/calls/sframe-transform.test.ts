// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The stream index a sender is sealed under (ADR-0035, ADR-0070 step 1).
 *
 * A participant's camera and its light sender are two video senders of one
 * leaf. Sealed under one index they would share a key and a nonce space, so
 * the light sender's index is stated by its caller and never read from the
 * track kind.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  attachSenderSFrame,
  disposeSFrameWorker,
  SFRAME_STREAM_AUDIO,
  SFRAME_STREAM_CAMERA,
  SFRAME_STREAM_CAMERA_LIGHT,
  SFRAME_STREAM_CAMERA_MIDDLE,
  SFRAME_STREAM_SCREEN_SHARE,
  streamIndexOfKind,
  syncSenderCodecs,
  type SFrameTransformOptions,
} from "./sframe-transform";

const made: SFrameTransformOptions[] = [];
/** Everything the main thread posted to the (stubbed) worker. */
const posted: unknown[] = [];
/** The (stubbed) worker's `message` listeners, to play the worker's side. */
const workerListeners: ((ev: { data: unknown }) => void)[] = [];

beforeEach(() => {
  made.length = 0;
  posted.length = 0;
  workerListeners.length = 0;
  vi.stubGlobal(
    "RTCRtpScriptTransform",
    class {
      constructor(_worker: unknown, options: SFrameTransformOptions) {
        made.push(options);
      }
    },
  );
  vi.stubGlobal(
    "Worker",
    class {
      addEventListener(_type: string, listener: (ev: { data: unknown }) => void): void {
        workerListeners.push(listener);
      }
      postMessage(message: unknown): void {
        posted.push(message);
      }
      terminate(): void {}
    },
  );
});

afterEach(() => {
  disposeSFrameWorker();
  vi.unstubAllGlobals();
});

const sender = (kind: "audio" | "video"): RTCRtpSender => ({ track: { kind }, transform: null }) as unknown as RTCRtpSender;
const seed = new Uint8Array(32).fill(5);

describe("SFrame stream indices", () => {
  it("are audio 0, camera 1, screen share 2 (reserved), light camera 3", () => {
    expect(SFRAME_STREAM_AUDIO).toBe(0);
    expect(SFRAME_STREAM_CAMERA).toBe(1);
    expect(SFRAME_STREAM_SCREEN_SHARE).toBe(2);
    expect(SFRAME_STREAM_CAMERA_LIGHT).toBe(3);
    expect(streamIndexOfKind("audio")).toBe(0);
    expect(streamIndexOfKind("video")).toBe(1);
  });
});

describe("the middle camera's stream index", () => {
  it("is 4, and no other stream's", () => {
    expect(SFRAME_STREAM_CAMERA_MIDDLE).toBe(4);
    const all = [
      SFRAME_STREAM_AUDIO,
      SFRAME_STREAM_CAMERA,
      SFRAME_STREAM_SCREEN_SHARE,
      SFRAME_STREAM_CAMERA_LIGHT,
      SFRAME_STREAM_CAMERA_MIDDLE,
    ];
    expect(new Set(all).size).toBe(all.length);
  });

  it("is what the middle sender is sealed under when its caller states it", () => {
    const middle = sender("video");
    attachSenderSFrame(middle, { seed, localLeaf: 4, streamIndex: SFRAME_STREAM_CAMERA_MIDDLE });
    expect(made).toHaveLength(1);
    expect(made[0]?.streamIndex).toBe(4);
    expect(made[0]?.operation).toBe("encrypt");
    expect(middle.transform).not.toBeNull();
  });
});

describe("attachSenderSFrame", () => {
  it("seals the microphone under 0 and the main camera under 1, from the track kind", () => {
    const mic = sender("audio");
    const camera = sender("video");
    attachSenderSFrame(mic, { seed, localLeaf: 4 });
    attachSenderSFrame(camera, { seed, localLeaf: 4 });
    expect(made.map((o) => o.streamIndex)).toEqual([0, 1]);
    expect(made.every((o) => o.operation === "encrypt" && o.localLeaf === 4)).toBe(true);
    expect(mic.transform).not.toBeNull();
    expect(camera.transform).not.toBeNull();
  });

  it("seals the light sender under the index its caller states, not the track kind's", () => {
    const light = sender("video");
    const id = attachSenderSFrame(light, { seed, localLeaf: 4, streamIndex: SFRAME_STREAM_CAMERA_LIGHT });
    expect(made).toHaveLength(1);
    expect(made[0]?.streamIndex).toBe(3);
    expect(made[0]?.id).toBe(id);
    expect(light.transform).not.toBeNull();
  });

  it("refuses an index outside one byte, and attaches nothing", () => {
    const light = sender("video");
    for (const bad of [256, -1, 1.5, Number.NaN]) {
      expect(() => attachSenderSFrame(light, { seed, localLeaf: 4, streamIndex: bad })).toThrow();
    }
    expect(made).toHaveLength(0);
    expect(light.transform).toBeNull();
  });

  it("refuses without the Encoded Transform API (no unsealed sender)", () => {
    vi.stubGlobal("RTCRtpScriptTransform", undefined);
    expect(() =>
      attachSenderSFrame(sender("video"), { seed, localLeaf: 4, streamIndex: SFRAME_STREAM_CAMERA_LIGHT }),
    ).toThrow(/invariant #4/);
  });
});

describe("telling the worker a sender's codecs (ADR-0071)", () => {
  it("states the track kind, and posts nothing while no codec is negotiated", () => {
    attachSenderSFrame(sender("video"), { seed, localLeaf: 4 });
    attachSenderSFrame(sender("audio"), { seed, localLeaf: 4 });
    expect(made.map((o) => o.mediaKind)).toEqual(["video", "audio"]);
    syncSenderCodecs();
    expect(posted).toEqual([]);
  });

  it("posts payload type and MIME type once negotiated, on sync and when the worker asks", () => {
    let codecs: { payloadType: number; mimeType: string }[] = [];
    const camera = {
      track: { kind: "video" },
      transform: null,
      getParameters: () => ({ codecs }),
    } as unknown as RTCRtpSender;
    const id = attachSenderSFrame(camera, { seed, localLeaf: 4 });
    expect(posted).toEqual([]);

    codecs = [
      { payloadType: 96, mimeType: "video/VP8" },
      { payloadType: 97, mimeType: "video/rtx" },
    ];
    syncSenderCodecs();
    expect(posted).toEqual([{ type: "codecs", id, codecs }]);

    for (const listener of workerListeners) listener({ data: { kind: "sframe-codecs-wanted", id } });
    expect(posted).toHaveLength(2);
    expect(posted[1]).toEqual(posted[0]);
    // An id nobody attached is ignored.
    for (const listener of workerListeners) listener({ data: { kind: "sframe-codecs-wanted", id: "nope" } });
    expect(posted).toHaveLength(2);
  });

  it("posts at once for a sender attached after negotiation", () => {
    const late = {
      track: { kind: "video" },
      transform: null,
      getParameters: () => ({ codecs: [{ payloadType: 96, mimeType: "video/VP8" }] }),
    } as unknown as RTCRtpSender;
    const id = attachSenderSFrame(late, { seed, localLeaf: 4 });
    expect(posted).toEqual([{ type: "codecs", id, codecs: [{ payloadType: 96, mimeType: "video/VP8" }] }]);
  });
});
