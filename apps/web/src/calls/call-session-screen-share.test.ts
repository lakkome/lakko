// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A screen share on a one-to-one call (ADR-0083 section 4), headless: the
 * share's tracks go on transceivers of their own, sealed under stream index 2
 * (picture) and 5 (sound), never the camera's; the SDP that leaves this
 * device names the share's stream with the `~screen` suffix; a share that
 * cannot be sealed leaves nothing behind; and the other person's share
 * stream is not taken for their camera.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const sentSdp = vi.hoisted(() => ({ offers: [] as string[] }));

vi.mock("../wasm", () => {
  const ascii = (s: string): Uint8Array => new TextEncoder().encode(s);
  return {
    loadWasm: async () => ({
      composeCallAnswerSignaling: (): Uint8Array => ascii("answer"),
      composeCallOfferSignaling: (_id: Uint8Array, sdp: string): Uint8Array => {
        sentSdp.offers.push(sdp);
        return ascii("offer");
      },
      composeCallIceCandidateSignaling: (): Uint8Array => ascii("ice"),
      composeCallEndSignaling: (): Uint8Array => ascii("end"),
    }),
  };
});

import { CallSession, type CallSessionCallbacks } from "./call-session";
import type { ParsedCallSignaling } from "../../wasm/lakko_ffi_wasm";
import { ScreenShareNotSealedError } from "./screen-share-tracks";

class FakeTrack {
  enabled = true;
  readyState: "live" | "ended" = "live";
  constructor(readonly kind: "audio" | "video") {}
  stop(): void {
    this.readyState = "ended";
  }
}

class FakeStream {
  constructor(
    private tracks: FakeTrack[] = [],
    readonly id = "camera-stream",
  ) {}
  getTracks(): FakeTrack[] {
    return this.tracks;
  }
  getAudioTracks(): FakeTrack[] {
    return this.tracks.filter((t) => t.kind === "audio");
  }
  getVideoTracks(): FakeTrack[] {
    return this.tracks.filter((t) => t.kind === "video");
  }
  addTrack(t: FakeTrack): void {
    this.tracks.push(t);
  }
}

interface FakeTransceiver {
  sender: { track: FakeTrack | null };
  receiver: { track: FakeTrack };
  streamIds: string[];
  direction: string;
  stopped: boolean;
  stop(): void;
}

class FakePc {
  localDescription: { type: string; sdp: string } | null = null;
  remoteDescription: unknown = null;
  signalingState = "stable";
  iceConnectionState = "new";
  onnegotiationneeded: (() => void) | null = null;
  onicecandidate: unknown = null;
  ontrack: ((ev: unknown) => void) | null = null;
  oniceconnectionstatechange: (() => void) | null = null;
  transceivers: FakeTransceiver[] = [];
  constructor() {
    pcs.push(this);
  }
  private add(track: FakeTrack, streamIds: string[], direction: string): FakeTransceiver {
    const t: FakeTransceiver = {
      sender: { track },
      receiver: { track: new FakeTrack(track.kind) },
      streamIds,
      direction,
      stopped: false,
      stop(): void {
        t.stopped = true;
      },
    };
    this.transceivers.push(t);
    return t;
  }
  addTrack(track: FakeTrack, stream: FakeStream): { track: FakeTrack | null } {
    return this.add(track, [stream.id], "sendrecv").sender;
  }
  addTransceiver(track: FakeTrack, init: { direction: string; streams: FakeStream[] }): FakeTransceiver {
    if (refuseTransceiver) throw new Error("OperationError");
    return this.add(track, init.streams.map((s) => s.id), init.direction);
  }
  removeTrack(): void {}
  getSenders(): Array<{ track: FakeTrack | null }> {
    return this.transceivers.filter((t) => !t.stopped).map((t) => t.sender);
  }
  getTransceivers(): FakeTransceiver[] {
    return this.transceivers.filter((t) => !t.stopped);
  }
  private sdp(): string {
    return this.getTransceivers()
      .map((t) => `m=${t.sender.track?.kind ?? "x"}\r\na=msid:${t.streamIds[0] ?? "-"} track`)
      .join("\r\n");
  }
  async createOffer(): Promise<{ type: string; sdp: string }> {
    return { type: "offer", sdp: this.sdp() };
  }
  async setLocalDescription(desc?: { type: string; sdp: string }): Promise<void> {
    this.localDescription = desc ?? { type: "offer", sdp: this.sdp() };
    this.signalingState = this.localDescription.type === "offer" ? "have-local-offer" : "stable";
  }
  async setRemoteDescription(desc: { type: string }): Promise<void> {
    this.remoteDescription = desc;
    this.signalingState = desc.type === "offer" ? "have-remote-offer" : "stable";
  }
  async addIceCandidate(): Promise<void> {}
  async getStats(): Promise<Map<string, unknown>> {
    return new Map();
  }
  close(): void {}
  setIce(state: string): void {
    this.iceConnectionState = state;
    this.oniceconnectionstatechange?.();
  }
}

let pcs: FakePc[] = [];
let refuseTransceiver = false;

async function settle(): Promise<void> {
  for (let i = 0; i < 30; i += 1) await Promise.resolve();
  await new Promise((r) => setTimeout(r, 0));
}

beforeEach(() => {
  pcs = [];
  refuseTransceiver = false;
  sentSdp.offers = [];
  vi.stubGlobal("navigator", {
    mediaDevices: {
      getUserMedia: async (c: MediaStreamConstraints) =>
        new FakeStream(c.video ? [new FakeTrack("audio"), new FakeTrack("video")] : [new FakeTrack("audio")]),
    },
  });
  vi.stubGlobal("RTCPeerConnection", FakePc);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

async function connectedCall(sealFails = false): Promise<{
  session: CallSession;
  pc: FakePc;
  sealed: Array<{ sender: unknown; streamIndex: number | undefined }>;
  screens: unknown[];
  cameras: unknown[];
}> {
  const sealed: Array<{ sender: unknown; streamIndex: number | undefined }> = [];
  const screens: unknown[] = [];
  const cameras: unknown[] = [];
  const callbacks: CallSessionCallbacks = {
    onRemoteStream: (s) => cameras.push(s),
    onRemoteScreenStream: (s) => screens.push(s),
    onOutboundSignaling: () => {},
    onStateChange: () => {},
  };
  const session = new CallSession(new Uint8Array(16), callbacks, { ringTimeoutMs: 45_000 }, {
    supported: () => true,
    attachSender: (sender, opts) => {
      if (sealFails && opts.streamIndex === 2) throw new Error("transform refused");
      sealed.push({ sender, streamIndex: opts.streamIndex });
    },
    attachReceiver: () => {},
  });
  session.setCallKey(new Uint8Array(32), 3);
  await session.dial({ audio: true, video: false });
  await session.applyIncomingSignaling({
    kind: "answer",
    callId: new Uint8Array(16),
    sdp: "v=0 answer",
  } as unknown as ParsedCallSignaling);
  const pc = pcs[0];
  if (pc === undefined) throw new Error("no peer connection");
  pc.setIce("connected");
  await settle();
  return { session, pc, sealed, screens, cameras };
}

const capture = (): FakeStream => new FakeStream([new FakeTrack("video"), new FakeTrack("audio")], "cap-1");

describe("CallSession screen share", () => {
  it("seals the share under 2 and 5 on transceivers of its own", async () => {
    const { session, pc, sealed } = await connectedCall();
    const before = pc.transceivers.length;
    await session.attachScreenShare(capture() as unknown as MediaStream);
    const added = pc.transceivers.slice(before);
    expect(added.map((t) => t.direction)).toEqual(["sendonly", "sendonly"]);
    const indices = sealed.filter((s) => added.some((t) => t.sender === s.sender)).map((s) => s.streamIndex);
    expect(indices).toEqual([2, 5]);
  });

  it("names the share's stream as a share in the offer it sends", async () => {
    const { session, pc } = await connectedCall();
    await session.attachScreenShare(capture() as unknown as MediaStream);
    pc.onnegotiationneeded?.();
    await settle();
    const offer = sentSdp.offers.at(-1) ?? "";
    expect(offer).toContain("a=msid:cap-1~screen");
    expect(offer).toContain("a=msid:camera-stream track");
    // The browser's own description is left as it made it.
    expect(pc.localDescription?.sdp).not.toContain("~screen");
  });

  it("leaves nothing of a share it could not seal", async () => {
    const { session, pc } = await connectedCall(true);
    const before = pc.transceivers.length;
    await expect(session.attachScreenShare(capture() as unknown as MediaStream)).rejects.toBeInstanceOf(
      ScreenShareNotSealedError,
    );
    expect(pc.transceivers.slice(before).every((t) => t.stopped)).toBe(true);
    pc.onnegotiationneeded?.();
    await settle();
    expect(sentSdp.offers.at(-1) ?? "").not.toContain("~screen");
  });

  it("stops naming a share once it is detached", async () => {
    const { session, pc } = await connectedCall();
    await session.attachScreenShare(capture() as unknown as MediaStream);
    session.detachScreenShare();
    pc.signalingState = "stable";
    pc.onnegotiationneeded?.();
    await settle();
    expect(sentSdp.offers.at(-1) ?? "").not.toContain("~screen");
  });

  it("hands the other person's share stream over apart from their camera", async () => {
    const { pc, screens, cameras } = await connectedCall();
    const share = new FakeStream([new FakeTrack("video")], "peer-stream~screen");
    const camera = new FakeStream([new FakeTrack("video")], "peer-stream");
    pc.ontrack?.({ receiver: {}, track: share.getTracks()[0], streams: [share] });
    pc.ontrack?.({ receiver: {}, track: camera.getTracks()[0], streams: [camera] });
    expect(screens).toEqual([share]);
    expect(cameras).toEqual([camera]);
  });
});
