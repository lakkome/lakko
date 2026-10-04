// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import {
  addCameraSender,
  addGroupCallSender,
  CAMERA_DEGRADATION_PREFERENCE,
  CAMERA_LOW_MAX_BITRATE,
  CAMERA_LOW_SCALE_DOWN,
  CAMERA_RID_FULL,
  CAMERA_RID_LOW,
  CAMERA_SCALABILITY_MODE,
  CAMERA_SIMULCAST_ENABLED,
  cameraSimulcastEncodings,
  cameraSingleEncoding,
  preferCameraFramerate,
  requestCameraLayers,
} from "./camera-sender";

const track = (kind: "audio" | "video"): MediaStreamTrack => ({ kind }) as MediaStreamTrack;
const stream = {} as MediaStream;

/** A peer connection whose `addTransceiver` refuses the first `refuse`
 *  attempts, the way a browser without simulcast or without the mode would. */
function fakePc(opts: { refuse?: number } = {}) {
  const sender = {
    id: "s",
    getParameters: () => ({ encodings: [], degradationPreference: CAMERA_DEGRADATION_PREFERENCE }),
    setParameters: async () => {},
  } as unknown as RTCRtpSender;
  const addTrack = vi.fn(() => sender);
  let refusals = opts.refuse ?? 0;
  const addTransceiver = vi.fn((_track: MediaStreamTrack, _init: RTCRtpTransceiverInit) => {
    if (refusals > 0) {
      refusals -= 1;
      throw new Error("OperationError");
    }
    return { sender } as unknown as RTCRtpTransceiver;
  });
  return { pc: { addTrack, addTransceiver } as unknown as RTCPeerConnection, addTrack, addTransceiver, sender };
}

const SIMULCAST = [
  { rid: "q", scaleResolutionDownBy: 4, maxBitrate: 150_000, scalabilityMode: "L1T3" },
  { rid: "f", scalabilityMode: "L1T3" },
];

describe("the camera's encodings", () => {
  it("are the light layer then the full one, under the names the SFU expects", () => {
    expect(CAMERA_RID_LOW).toBe("q");
    expect(CAMERA_RID_FULL).toBe("f");
    expect(CAMERA_LOW_SCALE_DOWN).toBe(4);
    expect(CAMERA_LOW_MAX_BITRATE).toBe(150_000);
    expect(cameraSimulcastEncodings()).toEqual(SIMULCAST);
    expect(cameraSimulcastEncodings(false)).toEqual([
      { rid: "q", scaleResolutionDownBy: 4, maxBitrate: 150_000 },
      { rid: "f" },
    ]);
    expect(cameraSingleEncoding()).toEqual([{ scalabilityMode: CAMERA_SCALABILITY_MODE }]);
  });
});

describe("addGroupCallSender (the first publish)", () => {
  it("sends the camera as two simulcast layers", () => {
    expect(CAMERA_SIMULCAST_ENABLED).toBe(true);
    const f = fakePc();
    expect(addGroupCallSender(f.pc, track("video"), stream)).toBe(f.sender);
    expect(f.addTransceiver).toHaveBeenCalledOnce();
    expect(f.addTransceiver).toHaveBeenCalledWith(track("video"), {
      direction: "sendrecv",
      streams: [stream],
      sendEncodings: SIMULCAST,
    });
    expect(f.addTrack).not.toHaveBeenCalled();
  });

  it("falls back to one encoding when the browser refuses simulcast", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const f = fakePc({ refuse: 2 });
    expect(addGroupCallSender(f.pc, track("video"), stream)).toBe(f.sender);
    expect(f.addTransceiver.mock.calls[2]?.[1].sendEncodings).toEqual([
      { scalabilityMode: CAMERA_SCALABILITY_MODE },
    ]);
    expect(f.addTrack).not.toHaveBeenCalled();
    warn.mockRestore();
  });

  it("sends one encoding when simulcast is switched off", () => {
    const f = fakePc();
    expect(addCameraSender(f.pc, track("video"), stream, false)).toBe(f.sender);
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
});

describe("addCameraSender (the first publish and the mid-call camera-on)", () => {
  it("asks for the same two layers on a transceiver of its own", () => {
    const f = fakePc();
    expect(addCameraSender(f.pc, track("video"), stream, true)).toBe(f.sender);
    expect(f.addTransceiver).toHaveBeenCalledWith(track("video"), {
      direction: "sendrecv",
      streams: [stream],
      sendEncodings: SIMULCAST,
    });
    expect(f.addTrack).not.toHaveBeenCalled();
  });

  it("keeps simulcast without the mode when the browser refuses the mode", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const f = fakePc({ refuse: 1 });
    expect(addCameraSender(f.pc, track("video"), stream, true)).toBe(f.sender);
    expect(f.addTransceiver).toHaveBeenCalledTimes(2);
    expect(f.addTransceiver.mock.calls[1]?.[1].sendEncodings).toEqual([
      { rid: "q", scaleResolutionDownBy: 4, maxBitrate: 150_000 },
      { rid: "f" },
    ]);
    expect(f.addTrack).not.toHaveBeenCalled();
    expect(warn).toHaveBeenCalledOnce();
    warn.mockRestore();
  });

  it("falls back to one encoding, and says so, when the browser refuses simulcast", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const f = fakePc({ refuse: 2 });
    expect(addCameraSender(f.pc, track("video"), stream, true)).toBe(f.sender);
    expect(f.addTransceiver).toHaveBeenCalledTimes(3);
    expect(f.addTransceiver.mock.calls[2]?.[1].sendEncodings).toEqual([
      { scalabilityMode: CAMERA_SCALABILITY_MODE },
    ]);
    expect(f.addTrack).not.toHaveBeenCalled();
    expect(warn).toHaveBeenCalledTimes(2);
    warn.mockRestore();
  });

  it("adds the track plainly when the browser refuses every encoding", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const f = fakePc({ refuse: 3 });
    expect(addCameraSender(f.pc, track("video"), stream, true)).toBe(f.sender);
    expect(f.addTransceiver).toHaveBeenCalledTimes(3);
    expect(f.addTrack).toHaveBeenCalledOnce();
    expect(f.addTrack).toHaveBeenCalledWith(track("video"), stream);
    warn.mockRestore();
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
