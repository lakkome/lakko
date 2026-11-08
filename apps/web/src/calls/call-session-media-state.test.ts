// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one-to-one `mediaState` call signal (2026-11-07), headless: once the
 * call connects the session tells the other side its camera and microphone
 * state, again on every change and never twice for the same state, never as
 * a wake; and a mute chosen before the capture holds for the captured
 * microphone and is what the first state says. The other side's tile shows
 * the mute mark from it (`docs/ux/group-call-grid.md` 7.2).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const sent = vi.hoisted(() => ({ media: [] as string[] }));

vi.mock("../wasm", () => {
  const ascii = (s: string): Uint8Array => new TextEncoder().encode(s);
  return {
    loadWasm: async () => ({
      composeCallAnswerSignaling: (): Uint8Array => ascii("answer"),
      composeCallOfferSignaling: (): Uint8Array => ascii("offer"),
      composeCallIceCandidateSignaling: (): Uint8Array => ascii("ice"),
      composeCallEndSignaling: (): Uint8Array => ascii("end"),
      composeCallMediaStateSignaling: (_id: Uint8Array, camera: boolean, mic: boolean): Uint8Array =>
        ascii(`media:${camera ? "on" : "off"}:${mic ? "on" : "off"}`),
    }),
  };
});

import { CallSession, type CallSessionCallbacks } from "./call-session";
import type { ParsedCallSignaling } from "../../wasm/lakko_ffi_wasm";

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
  sent.media = [];
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

async function connectedCall(
  sealFails = false,
  media: { audio: boolean; video: boolean } = { audio: true, video: false },
  beforeDial?: (session: CallSession) => void,
): Promise<{
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
    onOutboundSignaling: (bytes, wake) => {
      const text = new TextDecoder().decode(bytes);
      if (text.startsWith("media:")) sent.media.push(`${text}|${wake}`);
    },
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
  beforeDial?.(session);
  await session.dial(media);
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

describe("CallSession media state", () => {
  it("tells the other side its state once the call connects, never as a wake", async () => {
    await connectedCall();
    expect(sent.media).toEqual(["media:off:on|none"]);
  });

  it("sends a mute and an unmute, and nothing for an unchanged state", async () => {
    const { session } = await connectedCall();
    session.setMicrophoneEnabled(false);
    await settle();
    session.setMicrophoneEnabled(false);
    await settle();
    session.setMicrophoneEnabled(true);
    await settle();
    expect(sent.media).toEqual(["media:off:on|none", "media:off:off|none", "media:off:on|none"]);
  });

  it("a mute chosen before the capture holds and is the first state sent", async () => {
    let session: CallSession | null = null;
    const call = await connectedCall(false, { audio: true, video: false }, (s) => {
      session = s;
      s.setMicrophoneEnabled(false);
    });
    expect(session).not.toBeNull();
    const audio = call.pc.transceivers.map((t) => t.sender.track).filter((t) => t?.kind === "audio");
    expect(audio.length).toBeGreaterThan(0);
    expect(audio.every((t) => t?.enabled === false)).toBe(true);
    expect(sent.media).toEqual(["media:off:off|none"]);
  });

  it("follows the camera on a video call", async () => {
    const { session } = await connectedCall(false, { audio: true, video: true });
    expect(sent.media).toEqual(["media:on:on|none"]);
    session.setCameraEnabled(false);
    await settle();
    expect(sent.media).toEqual(["media:on:on|none", "media:off:on|none"]);
  });

  it("says nothing before the call connects", async () => {
    const callbacks: CallSessionCallbacks = {
      onRemoteStream: () => {},
      onOutboundSignaling: (bytes) => {
        const text = new TextDecoder().decode(bytes);
        if (text.startsWith("media:")) sent.media.push(text);
      },
      onStateChange: () => {},
    };
    const session = new CallSession(new Uint8Array(16), callbacks, { ringTimeoutMs: 45_000 }, {
      supported: () => true,
      attachSender: () => {},
      attachReceiver: () => {},
    });
    session.setCallKey(new Uint8Array(32), 3);
    await session.dial({ audio: true, video: false });
    session.setMicrophoneEnabled(false);
    await settle();
    expect(sent.media).toEqual([]);
    void session.hangUp("hungUp");
  });
});
