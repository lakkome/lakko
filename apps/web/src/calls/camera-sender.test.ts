// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import {
  addCopySender,
  CAMERA_COPY_KINDS,
  CAMERA_MIDDLE_MAX_BITRATE,
  CAMERA_MIDDLE_SCALE_DOWN,
  cameraCopyEncoding,
  cameraMiddleEncoding,
  configureCopySender,
  removeCopySender,
  setCopySenderActive,
  addCameraSender,
  addCameraSenderMidCall,
  addGroupCallSender,
  addLightSender,
  CAMERA_DEGRADATION_PREFERENCE,
  CAMERA_LIGHT_MAX_BITRATE,
  CAMERA_LIGHT_SCALE_DOWN,
  CAMERA_SCALABILITY_MODE,
  cameraLightEncoding,
  cameraSingleEncoding,
  configureLightSender,
  preferCameraFramerate,
  removeLightSender,
  requestCameraLayers,
} from "./camera-sender";
import * as cameraSender from "./camera-sender";

interface FakeTrack {
  kind: "audio" | "video";
  stopped: boolean;
  stop(): void;
  clone(): FakeTrack;
  clones: FakeTrack[];
}

function fakeTrack(kind: "audio" | "video"): FakeTrack {
  const t: FakeTrack = {
    kind,
    stopped: false,
    clones: [],
    stop(): void {
      t.stopped = true;
    },
    clone(): FakeTrack {
      const c = fakeTrack(kind);
      t.clones.push(c);
      return c;
    },
  };
  return t;
}
const track = (kind: "audio" | "video"): MediaStreamTrack => fakeTrack(kind) as unknown as MediaStreamTrack;
const stream = {} as MediaStream;

/** A peer connection whose `addTransceiver` refuses whatever `refuse` says,
 *  the way a browser without the mode or the encoding would. */
function fakePc(opts: { refuse?: (init: RTCRtpTransceiverInit) => boolean; setParametersFails?: boolean } = {}) {
  const setParameters = vi.fn(async (_p: RTCRtpSendParameters) => {
    if (opts.setParametersFails === true) throw new Error("InvalidModificationError");
  });
  const sender = {
    id: "s",
    getParameters: () => ({ encodings: [{ active: true }] }),
    setParameters,
  } as unknown as RTCRtpSender;
  const addTrack = vi.fn((_track: MediaStreamTrack, _stream: MediaStream) => sender);
  const removeTrack = vi.fn();
  const stop = vi.fn();
  const addTransceiver = vi.fn((_track: MediaStreamTrack, init: RTCRtpTransceiverInit) => {
    if (opts.refuse?.(init) === true) throw new Error("OperationError");
    return { sender, stop } as unknown as RTCRtpTransceiver;
  });
  return {
    pc: { addTrack, addTransceiver, removeTrack } as unknown as RTCPeerConnection,
    addTrack,
    addTransceiver,
    removeTrack,
    stop,
    sender,
    setParameters,
  };
}

const ONE_ENCODING = [{ scalabilityMode: "L1T3" }];
const LIGHT_ENCODING = { scaleResolutionDownBy: 4, maxBitrate: 150_000 };

describe("simulcast", () => {
  it("is gone from the publish path: no switch, no rid, no two-layer encodings", () => {
    const exported = Object.keys(cameraSender);
    expect(exported).not.toContain("CAMERA_SIMULCAST_ENABLED");
    expect(exported).not.toContain("CAMERA_RID_LOW");
    expect(exported).not.toContain("CAMERA_RID_FULL");
    expect(exported).not.toContain("cameraSimulcastEncodings");
  });
});

describe("the main camera sender", () => {
  it("has one encoding, in temporal layers, with no rid", () => {
    expect(CAMERA_SCALABILITY_MODE).toBe("L1T3");
    expect(cameraSingleEncoding()).toEqual(ONE_ENCODING);
    expect(cameraSingleEncoding()).toHaveLength(1);
    expect(cameraSingleEncoding()[0]).not.toHaveProperty("rid");
  });

  it("is created with that one encoding on the first publish", () => {
    const f = fakePc();
    const t = track("video");
    expect(addGroupCallSender(f.pc, t, stream)).toBe(f.sender);
    expect(f.addTransceiver).toHaveBeenCalledOnce();
    expect(f.addTransceiver).toHaveBeenCalledWith(t, {
      direction: "sendrecv",
      streams: [stream],
      sendEncodings: ONE_ENCODING,
    });
    expect(f.addTrack).not.toHaveBeenCalled();
    // The same through the function the first publish reaches it by.
    const g = fakePc();
    addCameraSender(g.pc, t, stream);
    expect(g.addTransceiver.mock.calls[0]?.[1].sendEncodings).toEqual(ONE_ENCODING);
  });

  it("is added plainly, and asked for the layers, when the browser refuses the mode", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const f = fakePc({ refuse: () => true });
    const t = track("video");
    expect(addCameraSender(f.pc, t, stream)).toBe(f.sender);
    expect(f.addTransceiver).toHaveBeenCalledOnce();
    expect(f.addTrack).toHaveBeenCalledOnce();
    expect(f.addTrack).toHaveBeenCalledWith(t, stream);
    await Promise.resolve();
    expect(f.setParameters).toHaveBeenCalledWith({
      encodings: [{ active: true, scalabilityMode: "L1T3" }],
      degradationPreference: "maintain-framerate",
    });
    warn.mockRestore();
  });

  it("mid-call too gets a transceiver of its own, never one addTrack would reuse", () => {
    // `addTrack` mid-call takes the transceiver the SFU opened for another
    // person's camera, and the SFU does not take what arrives there as this
    // person's main picture.
    const f = fakePc();
    const t = track("video");
    expect(addCameraSenderMidCall(f.pc, t, stream)).toBe(f.sender);
    expect(f.addTrack).not.toHaveBeenCalled();
    expect(f.addTransceiver).toHaveBeenCalledOnce();
    const [given, init] = f.addTransceiver.mock.calls[0] ?? [];
    expect(given).toBe(t);
    expect(init?.direction).toBe("sendrecv");
    expect(init?.streams).toEqual([stream]);
  });

  it("leaves the microphone as before", () => {
    const f = fakePc();
    addGroupCallSender(f.pc, track("audio"), stream);
    expect(f.addTrack).toHaveBeenCalledOnce();
    expect(f.addTransceiver).not.toHaveBeenCalled();
  });
});

describe("the light camera sender", () => {
  it("is a quarter of the size, at most 150 kbit/s, with no rid", () => {
    expect(CAMERA_LIGHT_SCALE_DOWN).toBe(4);
    expect(CAMERA_LIGHT_MAX_BITRATE).toBe(150_000);
    expect(cameraLightEncoding()).toEqual(LIGHT_ENCODING);
    expect(cameraLightEncoding()).not.toHaveProperty("rid");
  });

  it("is a clone of the camera on a send-only transceiver of its own", async () => {
    const f = fakePc();
    const camera = fakeTrack("video");
    const added = addLightSender(f.pc, camera as unknown as MediaStreamTrack);
    expect(added).not.toBeNull();
    expect(camera.clones).toHaveLength(1);
    expect(added?.configured).toBe(true);
    expect(added?.light.track).toBe(camera.clones[0]);
    expect(added?.light.sender).toBe(f.sender);
    expect(f.addTransceiver).toHaveBeenCalledOnce();
    expect(f.addTransceiver).toHaveBeenCalledWith(camera.clones[0], {
      direction: "sendonly",
      sendEncodings: [LIGHT_ENCODING],
    });
    expect(f.addTrack).not.toHaveBeenCalled();
    // And it keeps its frame rate like the main one.
    await Promise.resolve();
    expect(f.setParameters).toHaveBeenCalledWith({
      encodings: [{ active: true }],
      degradationPreference: CAMERA_DEGRADATION_PREFERENCE,
    });
  });

  it("first fallback: added plainly when the browser throws on the encoding, to be set afterwards", async () => {
    const f = fakePc({ refuse: (init) => init.sendEncodings !== undefined });
    const camera = fakeTrack("video");
    const added = addLightSender(f.pc, camera as unknown as MediaStreamTrack);
    expect(added?.configured).toBe(false);
    expect(camera.clones).toHaveLength(1);
    expect(f.addTransceiver).toHaveBeenCalledTimes(2);
    expect(f.addTransceiver.mock.calls[1]).toEqual([camera.clones[0], { direction: "sendonly" }]);
    expect(f.setParameters).not.toHaveBeenCalled();

    expect(await configureLightSender(f.sender)).toBe(true);
    expect(f.setParameters).toHaveBeenCalledOnce();
    expect(f.setParameters).toHaveBeenCalledWith({
      encodings: [{ active: true, ...LIGHT_ENCODING }],
      degradationPreference: "maintain-framerate",
    });
  });

  it("second fallback: says so when setParameters is refused too, or there is no encoding", async () => {
    const f = fakePc({ setParametersFails: true });
    expect(await configureLightSender(f.sender)).toBe(false);
    const none = {
      getParameters: () => ({ encodings: [] }),
      setParameters: vi.fn(),
    } as unknown as RTCRtpSender;
    expect(await configureLightSender(none)).toBe(false);
  });

  it("is not added, and its clone is stopped, when the browser takes no second transceiver", () => {
    const f = fakePc({ refuse: () => true });
    const camera = fakeTrack("video");
    const refused = vi.fn();
    expect(addLightSender(f.pc, camera as unknown as MediaStreamTrack, refused)).toBeNull();
    expect(camera.clones[0]?.stopped).toBe(true);
    expect(refused).toHaveBeenCalledOnce();
  });

  it("is removed by stopping its clone, its track and its transceiver, each best effort", () => {
    const f = fakePc();
    const camera = fakeTrack("video");
    const added = addLightSender(f.pc, camera as unknown as MediaStreamTrack);
    if (added === null) throw new Error("no light sender");
    f.removeTrack.mockImplementation(() => {
      throw new Error("InvalidStateError");
    });
    removeLightSender(f.pc, added.light);
    expect(camera.clones[0]?.stopped).toBe(true);
    expect(f.removeTrack).toHaveBeenCalledWith(f.sender);
    expect(f.stop).toHaveBeenCalledOnce();
  });
});

describe("the middle camera sender", () => {
  it("is half the size, at most 500 kbit/s, with no rid, and says whether it sends", () => {
    expect(CAMERA_MIDDLE_SCALE_DOWN).toBe(2);
    expect(CAMERA_MIDDLE_MAX_BITRATE).toBe(500_000);
    expect(cameraMiddleEncoding(true)).toEqual({
      scaleResolutionDownBy: 2,
      maxBitrate: 500_000,
      active: true,
    });
    expect(cameraMiddleEncoding(false)).toEqual({
      scaleResolutionDownBy: 2,
      maxBitrate: 500_000,
      active: false,
    });
    expect(cameraMiddleEncoding(true)).not.toHaveProperty("rid");
    // The light sender's encoding never carries the flag.
    expect(cameraCopyEncoding("light")).toEqual(LIGHT_ENCODING);
    expect(CAMERA_COPY_KINDS).toEqual(["light", "middle"]);
  });

  it("is a clone of the camera on a send-only transceiver of its own, asked to keep its frame rate", async () => {
    const f = fakePc();
    const camera = fakeTrack("video");
    const added = addCopySender(f.pc, camera as unknown as MediaStreamTrack, "middle", undefined, false);
    expect(added?.configured).toBe(true);
    expect(camera.clones).toHaveLength(1);
    expect(added?.copy.track).toBe(camera.clones[0]);
    expect(added?.copy.sender).toBe(f.sender);
    expect(f.addTransceiver).toHaveBeenCalledOnce();
    expect(f.addTransceiver).toHaveBeenCalledWith(camera.clones[0], {
      direction: "sendonly",
      sendEncodings: [{ scaleResolutionDownBy: 2, maxBitrate: 500_000, active: false }],
    });
    expect(f.addTrack).not.toHaveBeenCalled();
    await Promise.resolve();
    expect(f.setParameters).toHaveBeenCalledWith({
      encodings: [{ active: true }],
      degradationPreference: CAMERA_DEGRADATION_PREFERENCE,
    });
  });

  it("first fallback: added plainly, then set with setParameters, flag included", async () => {
    const f = fakePc({ refuse: (init) => init.sendEncodings !== undefined });
    const camera = fakeTrack("video");
    const added = addCopySender(f.pc, camera as unknown as MediaStreamTrack, "middle", undefined, false);
    expect(added?.configured).toBe(false);
    expect(f.addTransceiver).toHaveBeenCalledTimes(2);
    expect(f.addTransceiver.mock.calls[1]).toEqual([camera.clones[0], { direction: "sendonly" }]);

    expect(await configureCopySender(f.sender, "middle", false)).toBe(true);
    expect(f.setParameters).toHaveBeenCalledWith({
      encodings: [{ scaleResolutionDownBy: 2, maxBitrate: 500_000, active: false }],
      degradationPreference: "maintain-framerate",
    });
  });

  it("second fallback: says so when setParameters is refused too", async () => {
    const f = fakePc({ setParametersFails: true });
    expect(await configureCopySender(f.sender, "middle", true)).toBe(false);
  });

  it("is not added, its clone is stopped and the reason is given, when the browser takes no further transceiver", () => {
    const f = fakePc({ refuse: () => true });
    const camera = fakeTrack("video");
    const refused = vi.fn();
    expect(addCopySender(f.pc, camera as unknown as MediaStreamTrack, "middle", refused, true)).toBeNull();
    expect(camera.clones[0]?.stopped).toBe(true);
    expect(refused).toHaveBeenCalledOnce();
    expect(String(refused.mock.calls[0]?.[0])).toContain("camera transceiver was refused");
  });

  it("is removed by stopping its clone, its track and its transceiver, each best effort", () => {
    const f = fakePc();
    const camera = fakeTrack("video");
    const added = addCopySender(f.pc, camera as unknown as MediaStreamTrack, "middle", undefined, true);
    if (added === null) throw new Error("no middle sender");
    f.removeTrack.mockImplementation(() => {
      throw new Error("InvalidStateError");
    });
    removeCopySender(f.pc, added.copy);
    expect(camera.clones[0]?.stopped).toBe(true);
    expect(f.removeTrack).toHaveBeenCalledWith(f.sender);
    expect(f.stop).toHaveBeenCalledOnce();
  });
});

describe("setCopySenderActive", () => {
  /** A sender that holds its parameters the way a browser does. */
  function holdingSender(first: Record<string, unknown> | undefined, fails = false) {
    let params = { encodings: first === undefined ? [] : [first], transactionId: "t" };
    const setParameters = vi.fn(async (p: typeof params) => {
      if (fails) throw new Error("InvalidStateError");
      params = p;
    });
    return {
      sender: {
        getParameters: () => ({ ...params, encodings: params.encodings.map((e) => ({ ...e })) }),
        setParameters,
      } as unknown as RTCRtpSender,
      setParameters,
      current: (): Record<string, unknown> | undefined => params.encodings[0],
    };
  }

  it("changes only the flag of the one encoding, with setParameters", async () => {
    const s = holdingSender({ scaleResolutionDownBy: 2, maxBitrate: 500_000, active: false });
    expect(await setCopySenderActive(s.sender, true)).toBe(true);
    expect(s.setParameters).toHaveBeenCalledOnce();
    expect(s.setParameters).toHaveBeenCalledWith({
      encodings: [{ scaleResolutionDownBy: 2, maxBitrate: 500_000, active: true }],
      transactionId: "t",
    });
    expect(await setCopySenderActive(s.sender, false)).toBe(true);
    expect(s.current()).toEqual({ scaleResolutionDownBy: 2, maxBitrate: 500_000, active: false });
  });

  it("asks nothing of the browser when the encoding already says so", async () => {
    const off = holdingSender({ active: false });
    expect(await setCopySenderActive(off.sender, false)).toBe(true);
    expect(off.setParameters).not.toHaveBeenCalled();
    // An encoding that does not say is active.
    const unsaid = holdingSender({ maxBitrate: 500_000 });
    expect(await setCopySenderActive(unsaid.sender, true)).toBe(true);
    expect(unsaid.setParameters).not.toHaveBeenCalled();
  });

  it("says so when the browser refuses or there is no encoding", async () => {
    expect(await setCopySenderActive(holdingSender({ active: false }, true).sender, true)).toBe(false);
    expect(await setCopySenderActive(holdingSender(undefined).sender, true)).toBe(false);
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
