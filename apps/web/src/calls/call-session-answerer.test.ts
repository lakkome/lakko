// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * 1×1 answerer "Accept with video" — CallSession-level coverage.
 *
 * The vitest env is plain `node` (no jsdom) and Chat.tsx is a SolidJS `.tsx`
 * component, so the Chat.tsx orchestration (the `remoteWantsVideo` signal + the
 * two accept actions + the button UI) is exercised by a live 2-browser smoke
 * test, not here. What IS unit-testable — and what these tests pin — is the
 * CallSession behaviour the orchestration composes:
 *
 *   (a) an incoming AUDIO-only offer → the answerer captures no video;
 *   (b) an incoming VIDEO offer + "Accept with video" (= accept audio-only,
 *       then camOn=true drives `addLocalVideo`) → the camera is acquired and a
 *       video track lands on the local self-preview stream — reusing the shipped
 *       mid-call `addLocalVideo` path, not a duplicated capture;
 *   (c) plain "Accept" on a VIDEO offer → the answerer's camera is NOT
 *       acquired-then-disabled (the offer's video flag no longer turns the
 *       answerer's camera on at capture time — the old bug).
 *
 * The DOM (`navigator.mediaDevices.getUserMedia`, `RTCPeerConnection`) and the
 * wasm signaling composer are mocked; the assertions read the recorded
 * getUserMedia constraints + returned streams + the session's public state.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The only wasm the exercised paths touch is `composeCallAnswerSignaling`
// (inside `accept()`); stub the whole `../wasm` surface the session imports so
// the test needs no real wasm load.
vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    composeCallAnswerSignaling: (): Uint8Array => new Uint8Array(),
    composeCallOfferSignaling: (): Uint8Array => new Uint8Array(),
    composeCallIceCandidateSignaling: (): Uint8Array => new Uint8Array(),
    composeCallEndSignaling: (): Uint8Array => new Uint8Array(),
  }),
}));

import { CallSession } from "./call-session";
import type {
  CallSessionCallbacks,
  VideoPublishState,
  WebCallState,
} from "./call-session";
import type { ParsedCallSignaling } from "../../wasm/lakko_ffi_wasm";

// -----------------------------------------------------------------
// Minimal DOM fakes (node env has no WebRTC / mediaDevices).
// -----------------------------------------------------------------

class FakeTrack {
  enabled = true;
  readyState: "live" | "ended" = "live";
  constructor(readonly kind: "audio" | "video") {}
  stop(): void {
    this.readyState = "ended";
  }
}

class FakeStream {
  constructor(private tracks: FakeTrack[] = []) {}
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
  removeTrack(t: FakeTrack): void {
    this.tracks = this.tracks.filter((x) => x !== t);
  }
}

interface FakeSender {
  track: FakeTrack | null;
  replaceTrack(t: FakeTrack): Promise<void>;
}

class FakePeerConnection {
  localDescription: { type: string; sdp: string } | null = null;
  remoteDescription: { type: string; sdp: string } | null = null;
  signalingState = "stable";
  iceConnectionState = "new";
  onnegotiationneeded: (() => void) | null = null;
  onicecandidate: ((ev: unknown) => void) | null = null;
  ontrack: ((ev: unknown) => void) | null = null;
  oniceconnectionstatechange: (() => void) | null = null;
  private senders: FakeSender[] = [];
  constructor(_config?: unknown) {}
  addTrack(track: FakeTrack, _stream: FakeStream): FakeSender {
    const sender: FakeSender = {
      track,
      replaceTrack: async (t: FakeTrack): Promise<void> => {
        sender.track = t;
      },
    };
    this.senders.push(sender);
    return sender;
  }
  getSenders(): FakeSender[] {
    return this.senders;
  }
  async createOffer(): Promise<{ type: string; sdp: string }> {
    return { type: "offer", sdp: "v=0 fake-offer" };
  }
  async createAnswer(): Promise<{ type: string; sdp: string }> {
    return { type: "answer", sdp: "v=0 fake-answer" };
  }
  async setLocalDescription(desc?: { type: string; sdp: string }): Promise<void> {
    this.localDescription = desc ?? { type: "answer", sdp: "v=0 fake-local" };
  }
  async setRemoteDescription(desc: { type: string; sdp: string }): Promise<void> {
    this.remoteDescription = desc;
  }
  async addIceCandidate(_init: unknown): Promise<void> {}
  async getStats(): Promise<Map<string, unknown>> {
    return new Map();
  }
  close(): void {}
}

let gumCalls: MediaStreamConstraints[] = [];
let gumStreams: FakeStream[] = [];

async function fakeGetUserMedia(
  constraints: MediaStreamConstraints,
): Promise<FakeStream> {
  gumCalls.push(constraints);
  const tracks: FakeTrack[] = [];
  if (constraints.audio) tracks.push(new FakeTrack("audio"));
  if (constraints.video) tracks.push(new FakeTrack("video"));
  const stream = new FakeStream(tracks);
  gumStreams.push(stream);
  return stream;
}

beforeEach(() => {
  gumCalls = [];
  gumStreams = [];
  vi.stubGlobal("navigator", {
    mediaDevices: { getUserMedia: fakeGetUserMedia },
  });
  vi.stubGlobal("RTCPeerConnection", FakePeerConnection);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

/** Index into a recorded array, failing loudly on an out-of-range read
 *  (keeps `noUncheckedIndexedAccess` happy without `!` in the assertions). */
function at<T>(arr: T[], i: number): T {
  const v = arr[i];
  if (v === undefined) throw new Error(`index ${i} out of range (len ${arr.length})`);
  return v;
}

/** A parsed incoming call OFFER whose `mediaVideo` flag we control. */
function fakeOffer(mediaVideo: boolean): ParsedCallSignaling {
  return {
    kind: "offer",
    callId: new Uint8Array(16),
    sdp: "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n",
    mediaAudio: true,
    mediaVideo,
  } as unknown as ParsedCallSignaling;
}

function makeSession(): {
  session: CallSession;
  states: WebCallState[];
  videoStates: VideoPublishState[];
} {
  const states: WebCallState[] = [];
  const videoStates: VideoPublishState[] = [];
  const callbacks: CallSessionCallbacks = {
    onRemoteStream: () => {},
    onLocalStream: () => {},
    onOutboundSignaling: () => {},
    onStateChange: (s) => states.push(s),
    onVideoPublishState: (s) => videoStates.push(s),
  };
  const session = new CallSession(new Uint8Array(16), callbacks);
  return { session, states, videoStates };
}

describe("1×1 answerer capture flag + Accept-with-video", () => {
  it("(a) audio-only offer → the answerer captures no video", async () => {
    const { session } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(false));
    // Exactly one capture (openPeerConnection), audio-only.
    expect(gumCalls).toHaveLength(1);
    expect(at(gumCalls, 0).audio).toBeTruthy();
    expect(at(gumCalls, 0).video).toBe(false);
    expect(at(gumStreams, 0).getVideoTracks()).toHaveLength(0);
    // No camera lifecycle kicked off. (The ringing banner's "Accept with
    // video" button is gated on the offer's mediaVideo flag, which is false
    // here — a Chat.tsx concern covered by the 2-browser smoke test.)
    expect(session.getVideoPublishState()).toBe("off");
  });

  it("(c) plain Accept on a VIDEO offer → camera NOT acquired-then-disabled", async () => {
    const { session } = makeSession();
    // The offerer advertises video, but the answerer must NOT capture its
    // camera from the offerer's flag (the old acquire-then-mute bug).
    await session.applyIncomingSignaling(fakeOffer(true));
    expect(gumCalls).toHaveLength(1);
    expect(at(gumCalls, 0).video).toBe(false);
    expect(at(gumStreams, 0).getVideoTracks()).toHaveLength(0);

    // Plain "Accept" (audio-only): still no camera acquired, no video track.
    await session.accept();
    expect(gumCalls).toHaveLength(1); // no second getUserMedia
    expect(at(gumStreams, 0).getVideoTracks()).toHaveLength(0);
    expect(session.getVideoPublishState()).toBe("off");
  });

  it("(b) Accept with video → addLocalVideo acquires the camera + adds a local video track (reuses the shipped path)", async () => {
    const { session, videoStates } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(true));
    // "Accept with video" = accept audio-only, then camOn=true drives the
    // shipped mid-call addLocalVideo (the camera-sync effect in Chat.tsx).
    await session.accept();
    expect(gumCalls).toHaveLength(1); // accept alone captures nothing new
    await session.addLocalVideo();

    // A second getUserMedia acquired the camera (video:true).
    expect(gumCalls).toHaveLength(2);
    expect(at(gumCalls, 1).video).toBe(true);
    // The captured camera track landed on the local self-preview stream
    // (at(gumStreams, 0) = the audio capture from openPeerConnection).
    expect(at(gumStreams, 0).getVideoTracks()).toHaveLength(1);
    // Lifecycle reached live (starting → live), which the UI mirrors.
    expect(videoStates).toContain("starting");
    expect(session.getVideoPublishState()).toBe("live");
  });

  it("addLocalVideo is idempotent — a second call re-enables, no re-capture", async () => {
    const { session } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(true));
    await session.accept();
    await session.addLocalVideo();
    expect(gumCalls).toHaveLength(2);
    // A redundant reactive tick (the camera-sync effect can fire twice) must
    // not re-acquire the camera.
    await session.addLocalVideo();
    expect(gumCalls).toHaveLength(2);
    expect(session.getVideoPublishState()).toBe("live");
  });
});
