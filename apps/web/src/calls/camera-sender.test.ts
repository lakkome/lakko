// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import {
  addGroupCallSender,
  CAMERA_DEGRADATION_PREFERENCE,
  CAMERA_SCALABILITY_MODE,
  preferCameraFramerate,
  requestCameraLayers,
} from "./camera-sender";

const track = (kind: "audio" | "video"): MediaStreamTrack => ({ kind }) as MediaStreamTrack;
const stream = {} as MediaStream;

function fakePc(opts: { refuseLayers?: boolean } = {}) {
  const sender = { id: "s" } as unknown as RTCRtpSender;
  const addTrack = vi.fn(() => sender);
  const addTransceiver = vi.fn(() => {
    if (opts.refuseLayers === true) throw new Error("OperationError");
    return { sender } as unknown as RTCRtpTransceiver;
  });
  return { pc: { addTrack, addTransceiver } as unknown as RTCPeerConnection, addTrack, addTransceiver, sender };
}

describe("addGroupCallSender", () => {
  it("sends the camera in temporal layers", () => {
    const f = fakePc();
    expect(addGroupCallSender(f.pc, track("video"), stream)).toBe(f.sender);
    expect(f.addTransceiver).toHaveBeenCalledWith(track("video"), {
      direction: "sendrecv",
      streams: [stream],
      sendEncodings: [{ scalabilityMode: CAMERA_SCALABILITY_MODE }],
    });
    expect(f.addTrack).not.toHaveBeenCalled();
  });

  it("adds the microphone as before", () => {
    const f = fakePc();
    addGroupCallSender(f.pc, track("audio"), stream);
    expect(f.addTrack).toHaveBeenCalledOnce();
    expect(f.addTransceiver).not.toHaveBeenCalled();
  });

  it("falls back to one layer when the browser refuses the mode", () => {
    const f = fakePc({ refuseLayers: true });
    expect(addGroupCallSender(f.pc, track("video"), stream)).toBe(f.sender);
    expect(f.addTrack).toHaveBeenCalledOnce();
  });
});

describe("requestCameraLayers", () => {
  function fakeSender(encodings: Record<string, unknown>[], refuse = false, extra: Record<string, unknown> = {}) {
    const setParameters = vi.fn(async () => {
      if (refuse) throw new Error("InvalidModificationError");
    });
    const sender = {
      getParameters: () => ({ encodings: encodings.map((e) => ({ ...e })), ...extra }),
      setParameters,
    } as unknown as RTCRtpSender;
    return { sender, setParameters };
  }

  it("asks for the layers on the first encoding, and for the frame-rate preference", async () => {
    const f = fakeSender([{ active: true }]);
    await requestCameraLayers(f.sender);
    expect(f.setParameters).toHaveBeenCalledWith({
      encodings: [{ active: true, scalabilityMode: CAMERA_SCALABILITY_MODE }],
      degradationPreference: CAMERA_DEGRADATION_PREFERENCE,
    });
  });

  it("does nothing when both are already set", async () => {
    const set = fakeSender([{ scalabilityMode: CAMERA_SCALABILITY_MODE }], false, {
      degradationPreference: CAMERA_DEGRADATION_PREFERENCE,
    });
    await requestCameraLayers(set.sender);
    expect(set.setParameters).not.toHaveBeenCalled();
  });

  it("asks for the frame-rate preference alone when the layers are set or there is no encoding", async () => {
    const set = fakeSender([{ scalabilityMode: CAMERA_SCALABILITY_MODE }]);
    await requestCameraLayers(set.sender);
    expect(set.setParameters).toHaveBeenCalledWith({
      encodings: [{ scalabilityMode: CAMERA_SCALABILITY_MODE }],
      degradationPreference: CAMERA_DEGRADATION_PREFERENCE,
    });
    const none = fakeSender([]);
    await requestCameraLayers(none.sender);
    expect(none.setParameters).toHaveBeenCalledWith({
      encodings: [],
      degradationPreference: CAMERA_DEGRADATION_PREFERENCE,
    });
  });

  it("keeps the call going when the browser refuses", async () => {
    const f = fakeSender([{}], true);
    await expect(requestCameraLayers(f.sender)).resolves.toBeUndefined();
  });
});

describe("preferCameraFramerate", () => {
  it("asks the sender to keep its frame rate when the bitrate falls short", async () => {
    const setParameters = vi.fn(async () => {});
    const sender = {
      getParameters: () => ({ encodings: [{ active: true }] }),
      setParameters,
    } as unknown as RTCRtpSender;
    await preferCameraFramerate(sender);
    expect(CAMERA_DEGRADATION_PREFERENCE).toBe("maintain-framerate");
    expect(setParameters).toHaveBeenCalledWith({
      encodings: [{ active: true }],
      degradationPreference: "maintain-framerate",
    });
  });

  it("does nothing when the preference is already set", async () => {
    const setParameters = vi.fn(async () => {});
    const sender = {
      getParameters: () => ({ encodings: [], degradationPreference: "maintain-framerate" }),
      setParameters,
    } as unknown as RTCRtpSender;
    await preferCameraFramerate(sender);
    expect(setParameters).not.toHaveBeenCalled();
  });

  it("leaves the call alone when the browser refuses", async () => {
    const sender = {
      getParameters: () => {
        throw new Error("InvalidStateError");
      },
    } as unknown as RTCRtpSender;
    await expect(preferCameraFramerate(sender)).resolves.toBeUndefined();
  });
});
