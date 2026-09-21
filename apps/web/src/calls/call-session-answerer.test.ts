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
// The composers encode their kind (and, for End, the reason) as ASCII so a
// test can read back exactly what the session published.
vi.mock("../wasm", () => {
  const ascii = (s: string): Uint8Array => new TextEncoder().encode(s);
  return {
    loadWasm: async () => ({
      composeCallAnswerSignaling: (): Uint8Array => ascii("answer"),
      composeCallOfferSignaling: (): Uint8Array => ascii("offer"),
      composeCallIceCandidateSignaling: (): Uint8Array => ascii("ice"),
      composeCallEndSignaling: (_id: Uint8Array, reason: string): Uint8Array =>
        ascii(`end:${reason}`),
    }),
  };
});

import { CallSession } from "./call-session";
import type {
  CallSessionCallbacks,
  CallSessionTiming,
  VideoPublishState,
  WebCallEndReason,
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
  /** The session arms the generic frame descriptor on every video transceiver
   *  before each description (`frame-descriptor.ts`). An empty list is the
   *  honest answer for a fake that models senders and not transceivers: the
   *  arming is a no-op here and the answer path is what these tests are for. */
  getTransceivers(): unknown[] {
    return [];
  }
  closed = false;
  async createOffer(): Promise<{ type: string; sdp: string }> {
    return { type: "offer", sdp: "v=0 fake-offer" };
  }
  // Modelled on the real state machine where it matters: `createAnswer` is
  // only legal against an applied remote offer. The old answerer path reached
  // it in `stable`, and a fake that allowed that could not catch it.
  async createAnswer(): Promise<{ type: string; sdp: string }> {
    if (this.signalingState !== "have-remote-offer") {
      throw new Error(`InvalidStateError: createAnswer in ${this.signalingState}`);
    }
    return { type: "answer", sdp: "v=0 fake-answer" };
  }
  async setLocalDescription(desc?: { type: string; sdp: string }): Promise<void> {
    const type =
      desc?.type ?? (this.signalingState === "have-remote-offer" ? "answer" : "offer");
    this.localDescription = desc ?? { type, sdp: "v=0 fake-local" };
    this.signalingState = type === "offer" ? "have-local-offer" : "stable";
  }
  async setRemoteDescription(desc: { type: string; sdp: string }): Promise<void> {
    this.remoteDescription = desc;
    this.signalingState = desc.type === "offer" ? "have-remote-offer" : "stable";
  }
  addedCandidates: unknown[] = [];
  async addIceCandidate(init: unknown): Promise<void> {
    this.addedCandidates.push(init);
  }
  async getStats(): Promise<Map<string, unknown>> {
    return new Map();
  }
  close(): void {
    this.closed = true;
  }
  /** Drive an ICE transition the way the browser would. */
  setIce(state: string): void {
    this.iceConnectionState = state;
    this.oniceconnectionstatechange?.();
  }
}

let pcs: FakePeerConnection[] = [];

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
  pcs = [];
  vi.stubGlobal("navigator", {
    mediaDevices: { getUserMedia: fakeGetUserMedia },
  });
  vi.stubGlobal(
    "RTCPeerConnection",
    class extends FakePeerConnection {
      constructor(config?: unknown) {
        super(config);
        pcs.push(this);
      }
    },
  );
});

afterEach(() => {
  vi.useRealTimers();
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

function makeSession(timing: Partial<CallSessionTiming> = {}): {
  session: CallSession;
  states: WebCallState[];
  videoStates: VideoPublishState[];
  /** Every signaling body the session published, decoded from the ASCII
   *  the mocked composers emit (`offer`, `answer`, `ice`, `end:<reason>`). */
  published: string[];
  reasons: (WebCallEndReason | undefined)[];
} {
  const states: WebCallState[] = [];
  const reasons: (WebCallEndReason | undefined)[] = [];
  const videoStates: VideoPublishState[] = [];
  const published: string[] = [];
  const callbacks: CallSessionCallbacks = {
    onRemoteStream: () => {},
    onLocalStream: () => {},
    onOutboundSignaling: (bytes) => published.push(new TextDecoder().decode(bytes)),
    onStateChange: (s, r) => {
      states.push(s);
      reasons.push(r);
    },
    onVideoPublishState: (s) => videoStates.push(s),
  };
  const session = new CallSession(new Uint8Array(16), callbacks, timing);
  return { session, states, videoStates, published, reasons };
}

/** Let every pending promise continuation run (the session's handlers are
 *  chains of awaits over already-resolved fakes). */
async function settle(): Promise<void> {
  for (let i = 0; i < 20; i += 1) await Promise.resolve();
}

function fakeAnswer(): ParsedCallSignaling {
  return {
    kind: "answer",
    callId: new Uint8Array(16),
    sdp: "v=0 remote-answer",
  } as unknown as ParsedCallSignaling;
}

function fakeEnd(reason = "hungUp"): ParsedCallSignaling {
  return {
    kind: "end",
    callId: new Uint8Array(16),
    endReason: reason,
  } as unknown as ParsedCallSignaling;
}

function fakeCandidate(): ParsedCallSignaling {
  return {
    kind: "iceCandidate",
    callId: new Uint8Array(16),
    candidate: "candidate:1 1 udp 1 127.0.0.1 9 typ host",
    sdpMid: "0",
    sdpMLineIndex: 0,
  } as unknown as ParsedCallSignaling;
}

/** A group-control body (C-11). */
function fakeGroupStart(): ParsedCallSignaling {
  return {
    kind: "groupStart",
    callId: new Uint8Array(16),
    initiatorLeaf: 3,
  } as unknown as ParsedCallSignaling;
}

describe("1×1 answerer capture flag + Accept-with-video", () => {
  it("(a) audio-only offer → the answerer captures no video", async () => {
    const { session } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(false));
    await session.accept();
    // Exactly one capture (openPeerConnection, at accept), audio-only.
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
    // Ringing captures nothing at all.
    expect(gumCalls).toHaveLength(0);

    // Plain "Accept" (audio-only): the capture is audio-only, no video track.
    await session.accept();
    expect(gumCalls).toHaveLength(1);
    expect(at(gumCalls, 0).video).toBe(false);
    expect(at(gumStreams, 0).getVideoTracks()).toHaveLength(0);
    expect(session.getVideoPublishState()).toBe("off");
  });

  it("(b) Accept with video → addLocalVideo acquires the camera + adds a local video track (reuses the shipped path)", async () => {
    const { session, videoStates } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(true));
    // "Accept with video" = accept audio-only, then camOn=true drives the
    // shipped mid-call addLocalVideo (the camera-sync effect in Chat.tsx).
    await session.accept();
    expect(gumCalls).toHaveLength(1); // accept captures audio only
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

describe("1×1 answerer: ringing, repeats and accept", () => {
  it("ringing opens no microphone and builds no peer connection", async () => {
    const { session, states, published } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(false));
    expect(session.getState()).toBe("ringing");
    expect(states).toEqual(["ringing"]);
    expect(gumCalls).toHaveLength(0);
    expect(pcs).toHaveLength(0);
    expect(published).toEqual([]);
  });

  it("a repeated offer while ringing publishes nothing and Accept still answers once", async () => {
    const { session, published } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(false));
    // The same offer again — what the old rows effect re-fed on every poll.
    await session.applyIncomingSignaling(fakeOffer(false));
    await session.applyIncomingSignaling(fakeOffer(false));
    expect(published).toEqual([]);
    expect(session.getState()).toBe("ringing");

    await session.accept();
    expect(published).toEqual(["answer"]);
    expect(session.getState()).toBe("connecting");
    expect(at(pcs, 0).signalingState).toBe("stable");
  });

  it("accept applies the held offer and the candidates that arrived while ringing", async () => {
    const { session } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(false));
    await session.applyIncomingSignaling(fakeCandidate());
    await session.accept();
    expect(at(pcs, 0).remoteDescription?.type).toBe("offer");
    expect(at(pcs, 0).addedCandidates).toHaveLength(1);
  });

  it("a double Accept answers once", async () => {
    const { session, published } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(false));
    await Promise.all([session.accept(), session.accept()]);
    expect(published.filter((p) => p === "answer")).toHaveLength(1);
    expect(pcs).toHaveLength(1);
  });

  it("an offer is a renegotiation only once the call is connecting or connected", async () => {
    const { session, published } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(false));
    await session.accept();
    expect(session.getState()).toBe("connecting");
    // Now a genuine mid-call renegotiation: answered.
    await session.applyIncomingSignaling(fakeOffer(true));
    expect(published).toEqual(["answer", "answer"]);
    expect(session.getState()).toBe("connecting");
  });

  it("the caller hanging up while ringing ends it without a capture", async () => {
    const { session, reasons } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(false));
    await session.applyIncomingSignaling(fakeEnd("hungUp"));
    expect(session.getState()).toBe("done");
    expect(reasons.at(-1)).toBe("hungUp");
    // A late Accept tap does nothing.
    await session.accept();
    expect(gumCalls).toHaveLength(0);
  });

  it("an end that lands while Accept is capturing releases the capture", async () => {
    let release: (s: FakeStream) => void = () => {};
    vi.stubGlobal("navigator", {
      mediaDevices: {
        getUserMedia: (c: MediaStreamConstraints) =>
          new Promise<FakeStream>((resolve) => {
            gumCalls.push(c);
            release = resolve;
          }),
      },
    });
    const { session, published } = makeSession();
    await session.applyIncomingSignaling(fakeOffer(false));
    const accepting = session.accept();
    await settle();
    await session.applyIncomingSignaling(fakeEnd("hungUp"));
    const stream = new FakeStream([new FakeTrack("audio")]);
    release(stream);
    await accepting;
    expect(session.getState()).toBe("done");
    expect(published).not.toContain("answer");
    expect(pcs).toHaveLength(0);
    expect(at(stream.getTracks(), 0).readyState).toBe("ended");
  });

  it("group-control bodies never move the 1×1 state machine (C-11)", async () => {
    const { session, published, states } = makeSession();
    await session.applyIncomingSignaling(fakeGroupStart());
    expect(session.getState()).toBe("idle");
    expect(states).toEqual([]);
    expect(published).toEqual([]);
  });
});

describe("1×1 timeouts", () => {
  it("an unanswered incoming call ends as timedOut and says so", async () => {
    vi.useFakeTimers();
    const { session, published, reasons } = makeSession({ ringTimeoutMs: 1000 });
    await session.applyIncomingSignaling(fakeOffer(false));
    await vi.advanceTimersByTimeAsync(999);
    expect(session.getState()).toBe("ringing");
    await vi.advanceTimersByTimeAsync(1);
    expect(session.getState()).toBe("done");
    expect(published).toEqual(["end:timedOut"]);
    expect(reasons.at(-1)).toBe("timedOut");
  });

  it("accepting cancels the ring timeout", async () => {
    vi.useFakeTimers();
    const { session, published } = makeSession({
      ringTimeoutMs: 1000,
      connectTimeoutMs: 60_000,
    });
    await session.applyIncomingSignaling(fakeOffer(false));
    await session.accept();
    await vi.advanceTimersByTimeAsync(5000);
    expect(session.getState()).toBe("connecting");
    expect(published).toEqual(["answer"]);
  });

  it("a dial nobody answers ends as timedOut and says so", async () => {
    vi.useFakeTimers();
    const { session, published, reasons } = makeSession({ ringTimeoutMs: 2000 });
    await session.dial({ audio: true, video: false });
    expect(published).toEqual(["offer"]);
    await vi.advanceTimersByTimeAsync(2000);
    expect(session.getState()).toBe("done");
    expect(published).toEqual(["offer", "end:timedOut"]);
    expect(reasons.at(-1)).toBe("timedOut");
  });

  it("an answer late in the ring gets the whole connect window (section 3.4)", async () => {
    vi.useFakeTimers();
    const { session, published, reasons } = makeSession({
      ringTimeoutMs: 1000,
      connectTimeoutMs: 2000,
    });
    await session.dial({ audio: true, video: false });
    // Answered 100 ms before the ring would have run out.
    await vi.advanceTimersByTimeAsync(900);
    await session.applyIncomingSignaling(fakeAnswer());
    expect(session.getState()).toBe("connecting");
    // The old dial clock would have ended the call here; the connect clock
    // has 1900 ms left.
    await vi.advanceTimersByTimeAsync(1500);
    expect(session.getState()).toBe("connecting");
    expect(published).toEqual(["offer"]);
    // The connect window runs out: answered but never connected is a failure,
    // not a missed call.
    await vi.advanceTimersByTimeAsync(600);
    expect(session.getState()).toBe("done");
    expect(published).toEqual(["offer", "end:error"]);
    expect(reasons.at(-1)).toBe("error");
  });

  it("a connected call does not time out", async () => {
    vi.useFakeTimers();
    const { session, published } = makeSession({ connectTimeoutMs: 2000 });
    await session.dial({ audio: true, video: false });
    await session.applyIncomingSignaling(fakeAnswer());
    at(pcs, 0).setIce("connected");
    expect(session.getState()).toBe("connected");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(session.getState()).toBe("connected");
    expect(published).toEqual(["offer"]);
  });

  it("an ICE failure while a hang-up is publishing publishes one End", async () => {
    const { session, published } = makeSession();
    await session.dial({ audio: true, video: false });
    await session.applyIncomingSignaling(fakeAnswer());
    at(pcs, 0).setIce("connected");
    const hangUp = session.hangUp();
    // The transport dies in the same turn the hang-up started publishing.
    at(pcs, 0).setIce("failed");
    await hangUp;
    await settle();
    expect(published.filter((p) => p.startsWith("end:"))).toEqual(["end:hungUp"]);
  });

  it("a timeout racing a hang-up publishes one End", async () => {
    vi.useFakeTimers();
    const { session, published } = makeSession({ ringTimeoutMs: 1000 });
    await session.dial({ audio: true, video: false });
    const hangUp = session.hangUp();
    await vi.advanceTimersByTimeAsync(1000);
    await hangUp;
    expect(published.filter((p) => p.startsWith("end:"))).toEqual(["end:hungUp"]);
  });
});

describe("1×1 ICE disconnected grace", () => {
  async function connectedCaller(graceMs: number) {
    const made = makeSession({ iceDisconnectedGraceMs: graceMs });
    await made.session.dial({ audio: true, video: false });
    await made.session.applyIncomingSignaling(fakeAnswer());
    at(pcs, 0).setIce("connected");
    return made;
  }

  it("a brief disconnect that recovers keeps the call up", async () => {
    vi.useFakeTimers();
    const { session, published } = await connectedCaller(3000);
    at(pcs, 0).setIce("disconnected");
    await vi.advanceTimersByTimeAsync(2000);
    at(pcs, 0).setIce("connected");
    await vi.advanceTimersByTimeAsync(5000);
    expect(session.getState()).toBe("connected");
    expect(published).toEqual(["offer"]);
  });

  it("a disconnect that outlasts the grace ends the call", async () => {
    vi.useFakeTimers();
    const { session, published, reasons } = await connectedCaller(3000);
    at(pcs, 0).setIce("disconnected");
    await vi.advanceTimersByTimeAsync(2999);
    expect(session.getState()).toBe("connected");
    await vi.advanceTimersByTimeAsync(1);
    expect(session.getState()).toBe("done");
    expect(published).toEqual(["offer", "end:error"]);
    expect(reasons.at(-1)).toBe("error");
  });

  it("failed still ends the call at once", async () => {
    vi.useFakeTimers();
    const { session } = await connectedCaller(3000);
    at(pcs, 0).setIce("failed");
    await settle();
    expect(session.getState()).toBe("done");
  });
});
