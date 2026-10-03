// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import { addGroupCallSender, CAMERA_SCALABILITY_MODE, requestCameraLayers } from "./camera-sender";

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
  function fakeSender(encodings: Record<string, unknown>[], refuse = false) {
    const setParameters = vi.fn(async () => {
      if (refuse) throw new Error("InvalidModificationError");
    });
    const sender = {
      getParameters: () => ({ encodings: encodings.map((e) => ({ ...e })) }),
      setParameters,
    } as unknown as RTCRtpSender;
    return { sender, setParameters };
  }

  it("asks for the layers on the first encoding", async () => {
    const f = fakeSender([{ active: true }]);
    await requestCameraLayers(f.sender);
    expect(f.setParameters).toHaveBeenCalledWith({
      encodings: [{ active: true, scalabilityMode: CAMERA_SCALABILITY_MODE }],
    });
  });

  it("does nothing when the mode is already set or there is no encoding", async () => {
    const set = fakeSender([{ scalabilityMode: CAMERA_SCALABILITY_MODE }]);
    await requestCameraLayers(set.sender);
    expect(set.setParameters).not.toHaveBeenCalled();
    const none = fakeSender([]);
    await requestCameraLayers(none.sender);
    expect(none.setParameters).not.toHaveBeenCalled();
  });

  it("keeps the call going when the browser refuses", async () => {
    const f = fakeSender([{}], true);
    await expect(requestCameraLayers(f.sender)).resolves.toBeUndefined();
  });
});
