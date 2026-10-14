// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A 1x1 call that ended while one of its awaits was pending stays ended.
 *
 * The step that was pending finishes later (a capture behind its permission
 * prompt, the relay credential, the remote answer being applied, the
 * transport statistics read before a call is given up on) and must then ask
 * again whether the call is still the one it started for: no device is left
 * open, no peer connection is built, the call does not come back as
 * `connecting`, nothing is published after its End, and a call that
 * recovered during the await is not ended.
 *
 * Headless: a fake `getUserMedia` whose captures the test settles by hand
 * and a fake `RTCPeerConnection` whose slow steps the test releases by hand.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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
  WebCallEndReason,
  WebCallState,
} from "./call-session";
import type { ParsedCallSignaling } from "../../wasm/lakko_ffi_wasm";

// ---- Fakes ----------------------------------------------------------------

class FakeTrack {
  enabled = true;
  readyState: "live" | "ended" = "live";
  constructor(readonly kind: "audio" | "video") {
    allTracks.push(this);
  }
  stop(): void {
    this.readyState = "ended";
  }
}

let allTracks: FakeTrack[] = [];

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

interface PendingCapture {
  grant(): FakeStream;
  refuse(): void;
}

let captures: PendingCapture[] = [];

function fakeGetUserMedia(constraints: MediaStreamConstraints): Promise<FakeStream> {
  return new Promise<FakeStream>((resolve, reject) => {
    captures.push({
      grant: (): FakeStream => {
        const tracks: FakeTrack[] = [];
        if (constraints.audio !== undefined && constraints.audio !== false) tracks.push(new FakeTrack("audio"));
        if (constraints.video !== undefined && constraints.video !== false) tracks.push(new FakeTrack("video"));
        const stream = new FakeStream(tracks);
        resolve(stream);
        return stream;
      },
      refuse: (): void => {
        const err = new Error("Permission dismissed");
        err.name = "NotAllowedError";
        reject(err);
      },
    });
  });
}

/** A step the test lets through by hand. */
class Gate {
  private release: () => void = () => {};
  readonly wait = new Promise<void>((resolve) => {
    this.release = resolve;
  });
  open(): void {
    this.release();
  }
}

/** The steps of the fake browser a test can hold. `null`: not held. */
const hold = {
  setRemoteDescription: null as Gate | null,
  createAnswer: null as Gate | null,
  getStats: null as Gate | null,
  replaceTrack: null as Gate | null,
};

interface FakeSender {
  track: FakeTrack | null;
  replaceTrack(t: FakeTrack): Promise<void>;
}

let pcs: FakePeerConnection[] = [];

class FakePeerConnection {
  localDescription: { type: string; sdp: string } | null = null;
  remoteDescription: { type: string; sdp: string } | null = null;
  currentRemoteDescription: { type: string; sdp: string } | null = null;
  signalingState = "stable";
  iceConnectionState = "new";
  connectionState = "new";
  iceGatheringState = "new";
  onnegotiationneeded: (() => void) | null = null;
  onicecandidate: ((ev: unknown) => void) | null = null;
  ontrack: ((ev: unknown) => void) | null = null;
  oniceconnectionstatechange: (() => void) | null = null;
  closed = false;
  private senders: FakeSender[] = [];
  constructor() {
    pcs.push(this);
  }
  addTrack(track: FakeTrack): FakeSender {
    const sender: FakeSender = {
      track,
      replaceTrack: async (t: FakeTrack): Promise<void> => {
        if (hold.replaceTrack !== null) await hold.replaceTrack.wait;
        sender.track = t;
      },
    };
    this.senders.push(sender);
    return sender;
  }
  getSenders(): FakeSender[] {
    return this.senders;
  }
  getTransceivers(): unknown[] {
    return [];
  }
  async createOffer(): Promise<{ type: string; sdp: string }> {
    return { type: "offer", sdp: "v=0 fake-offer" };
  }
  async createAnswer(): Promise<{ type: string; sdp: string }> {
    if (hold.createAnswer !== null) await hold.createAnswer.wait;
    return { type: "answer", sdp: "v=0 fake-answer" };
  }
  async setLocalDescription(desc?: { type: string; sdp: string }): Promise<void> {
    const type = desc?.type ?? (this.signalingState === "have-remote-offer" ? "answer" : "offer");
    this.localDescription = desc ?? { type, sdp: "v=0 fake-local" };
    this.signalingState = type === "offer" ? "have-local-offer" : "stable";
  }
  async setRemoteDescription(desc: { type: string; sdp: string }): Promise<void> {
    if (hold.setRemoteDescription !== null) await hold.setRemoteDescription.wait;
    this.remoteDescription = desc;
    this.signalingState = desc.type === "offer" ? "have-remote-offer" : "stable";
  }
  async addIceCandidate(): Promise<void> {}
  async getStats(): Promise<Map<string, unknown>> {
    if (hold.getStats !== null) await hold.getStats.wait;
    return new Map();
  }
  close(): void {
    this.closed = true;
  }
  setIce(state: string): void {
    this.iceConnectionState = state;
    this.oniceconnectionstatechange?.();
  }
}

function at<T>(arr: T[], i: number): T {
  const v = arr[i];
  if (v === undefined) throw new Error(`index ${i} out of range (len ${arr.length})`);
  return v;
}

async function settle(): Promise<void> {
  for (let i = 0; i < 50; i += 1) await Promise.resolve();
}

function watch(p: Promise<unknown>): { settled(): "pending" | "resolved" | "rejected" } {
  let state: "pending" | "resolved" | "rejected" = "pending";
  p.then(
    () => {
      state = "resolved";
    },
    () => {
      state = "rejected";
    },
  );
  return { settled: () => state };
}

const RING_MS = 45_000;
const CONNECT_MS = 30_000;
const ICE_GRACE_MS = 10_000;

interface Harness {
  session: CallSession;
  states: WebCallState[];
  reasons: (WebCallEndReason | undefined)[];
  published: string[];
  /** One entry per `onLocalStream` report. */
  localStreams: unknown[];
}

function makeSession(timing: Partial<CallSessionTiming> = {}): Harness {
  const h: Harness = {
    session: undefined as unknown as CallSession,
    states: [],
    reasons: [],
    published: [],
    localStreams: [],
  };
  const callbacks: CallSessionCallbacks = {
    onRemoteStream: () => {},
    onLocalStream: (stream) => {
      h.localStreams.push(stream);
    },
    onOutboundSignaling: (bytes) => h.published.push(new TextDecoder().decode(bytes)),
    onStateChange: (s, r) => {
      h.states.push(s);
      h.reasons.push(r);
    },
  };
  h.session = new CallSession(
    new Uint8Array(16),
    callbacks,
    {
      ringTimeoutMs: RING_MS,
      connectTimeoutMs: CONNECT_MS,
      iceDisconnectedGraceMs: ICE_GRACE_MS,
      ...timing,
    },
    { supported: () => true, attachSender: () => {}, attachReceiver: () => {} },
  );
  h.session.setCallKey(new Uint8Array(32), 3);
  return h;
}

function offer(): ParsedCallSignaling {
  return {
    kind: "offer",
    callId: new Uint8Array(16),
    sdp: "v=0 remote-offer",
    mediaAudio: true,
    mediaVideo: false,
  } as unknown as ParsedCallSignaling;
}

function answer(): ParsedCallSignaling {
  return { kind: "answer", callId: new Uint8Array(16), sdp: "v=0 remote-answer" } as unknown as ParsedCallSignaling;
}

function liveTracks(): FakeTrack[] {
  return allTracks.filter((t) => t.readyState === "live");
}

function openPcs(): FakePeerConnection[] {
  return pcs.filter((pc) => !pc.closed);
}

beforeEach(() => {
  allTracks = [];
  captures = [];
  pcs = [];
  hold.setRemoteDescription = null;
  hold.createAnswer = null;
  hold.getStats = null;
  hold.replaceTrack = null;
  vi.useFakeTimers();
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.spyOn(console, "info").mockImplementation(() => {});
  vi.stubGlobal("navigator", { mediaDevices: { getUserMedia: fakeGetUserMedia } });
  vi.stubGlobal("RTCPeerConnection", FakePeerConnection);
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

/** A call dialled and waiting for its answer, devices open. */
async function dialled(timing: Partial<CallSessionTiming> = {}): Promise<Harness & { pc: FakePeerConnection }> {
  const h = makeSession(timing);
  const dial = h.session.dial({ audio: true, video: false });
  await settle();
  at(captures, 0).grant();
  await dial;
  expect(h.session.getState()).toBe("outgoing");
  return { ...h, pc: at(pcs, 0) };
}

/** A call dialled, answered and connected. */
async function connected(timing: Partial<CallSessionTiming> = {}): Promise<Harness & { pc: FakePeerConnection }> {
  const h = await dialled(timing);
  await h.session.applyIncomingSignaling(answer());
  h.pc.setIce("connected");
  expect(h.session.getState()).toBe("connected");
  return h;
}

// ---- A1 / A6: the capture ---------------------------------------------------

describe("a 1x1 call that ends while its devices are being opened", () => {
  it("dial, hung up during the prompt, prompt then granted: the capture is released and no Offer follows the End", async () => {
    const h = makeSession();
    const dial = watch(h.session.dial({ audio: true, video: true }));
    await settle();

    await h.session.hangUp();
    const stream = at(captures, 0).grant();
    await settle();

    expect(dial.settled()).toBe("resolved");
    expect(stream.getTracks().every((t) => t.readyState === "ended")).toBe(true);
    expect(pcs).toEqual([]);
    expect(h.published).toEqual(["end:hungUp"]);
    expect(h.localStreams).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
    expect(h.session.getState()).toBe("done");
  });

  it("dial, hung up during the prompt, prompt then refused: dial() does not reject (nothing failed)", async () => {
    const h = makeSession();
    const dial = watch(h.session.dial({ audio: true, video: false }));
    await settle();

    await h.session.hangUp();
    at(captures, 0).refuse();
    await settle();

    // A rejection here is what the screens show as "could not start call".
    expect(dial.settled()).toBe("resolved");
    expect(h.reasons.filter((r) => r !== undefined)).toEqual(["hungUp", "hungUp"]);
    expect(h.published).toEqual(["end:hungUp"]);
  });

  it("control: a refused prompt on a live dial still fails the call", async () => {
    const h = makeSession();
    const dial = watch(h.session.dial({ audio: true, video: false }));
    await settle();
    at(captures, 0).refuse();
    await settle();
    expect(dial.settled()).toBe("rejected");
    expect(h.session.getState()).toBe("done");
    expect(h.reasons).toContain("error");
  });

  it("dial, hung up while the relay credential was being fetched: no peer connection is built", async () => {
    const h = makeSession();
    let grantTurn: (server: RTCIceServer | null) => void = () => {};
    h.session.setTurnProvider(
      () =>
        new Promise<RTCIceServer | null>((resolve) => {
          grantTurn = resolve;
        }),
    );
    const dial = watch(h.session.dial({ audio: true, video: false }));
    await settle();
    const stream = at(captures, 0).grant();
    await settle();
    expect(liveTracks()).toHaveLength(1);

    await h.session.hangUp();
    grantTurn(null);
    await settle();

    expect(dial.settled()).toBe("resolved");
    expect(stream.getTracks().every((t) => t.readyState === "ended")).toBe(true);
    expect(pcs).toEqual([]);
    expect(h.published).toEqual(["end:hungUp"]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("accept, the caller hung up during the prompt: the capture is released and no Answer is published", async () => {
    const h = makeSession();
    await h.session.applyIncomingSignaling(offer());
    const accept = watch(h.session.accept());
    await settle();

    await h.session.applyIncomingSignaling({
      kind: "end",
      callId: new Uint8Array(16),
      endReason: "hungUp",
    } as unknown as ParsedCallSignaling);
    const stream = at(captures, 0).grant();
    await settle();

    expect(accept.settled()).toBe("resolved");
    expect(stream.getTracks().every((t) => t.readyState === "ended")).toBe(true);
    expect(pcs).toEqual([]);
    expect(h.published).toEqual([]);
    expect(h.session.getState()).toBe("done");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("accept, ended while the answer was being made: the call does not come back as connecting", async () => {
    const h = makeSession();
    await h.session.applyIncomingSignaling(offer());
    hold.createAnswer = new Gate();
    const accept = watch(h.session.accept());
    await settle();
    at(captures, 0).grant();
    await settle();
    expect(openPcs()).toHaveLength(1);

    await h.session.hangUp();
    hold.createAnswer.open();
    await settle();

    expect(accept.settled()).toBe("resolved");
    expect(h.session.getState()).toBe("done");
    expect(h.states).not.toContain("connecting");
    expect(h.published).toEqual(["end:hungUp"]);
    expect(liveTracks()).toEqual([]);
    expect(openPcs()).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
  });
});

// ---- A3: device switch --------------------------------------------------------

describe("a 1x1 device switch that finishes after the call ended", () => {
  it("the new capture is stopped and nothing throws", async () => {
    const h = await connected();
    const switched = watch(h.session.switchAudioInput("mic-2"));
    await settle();
    expect(captures).toHaveLength(2);

    await h.session.hangUp();
    const stream = at(captures, 1).grant();
    await settle();

    // Before the fix this rejected on `this.localStream` being null, with
    // the new microphone still open.
    expect(switched.settled()).toBe("resolved");
    expect(stream.getTracks().every((t) => t.readyState === "ended")).toBe(true);
    expect(liveTracks()).toEqual([]);
  });

  it("ended while the sender was taking the new track: the new track is stopped", async () => {
    const h = await connected();
    hold.replaceTrack = new Gate();
    const switched = watch(h.session.switchAudioInput("mic-2"));
    await settle();
    at(captures, 1).grant();
    await settle();
    expect(liveTracks()).toHaveLength(2);

    await h.session.hangUp();
    hold.replaceTrack.open();
    await settle();

    expect(switched.settled()).toBe("resolved");
    expect(liveTracks()).toEqual([]);
  });

  it("control: a switch on a live call replaces the track", async () => {
    const h = await connected();
    const before = h.localStreams.length;
    const switched = h.session.switchAudioInput("mic-2");
    await settle();
    at(captures, 1).grant();
    await switched;
    expect(liveTracks()).toHaveLength(1);
    expect(h.localStreams).toHaveLength(before + 1);
    await h.session.hangUp();
  });
});

// ---- A5: the answer path -------------------------------------------------------

describe("an answer that finishes applying after the call ended", () => {
  it("hung up meanwhile: the call stays done, no connect timer is armed, and it does not fail a second time", async () => {
    const h = await dialled();
    hold.setRemoteDescription = new Gate();
    const applied = watch(h.session.applyIncomingSignaling(answer()));
    await settle();

    await h.session.hangUp();
    expect(h.session.getState()).toBe("done");
    hold.setRemoteDescription.open();
    await settle();

    expect(applied.settled()).toBe("resolved");
    expect(h.session.getState()).toBe("done");
    expect(h.states).not.toContain("connecting");
    expect(vi.getTimerCount()).toBe(0);

    // The connect window passes: nothing left to give up on.
    await vi.advanceTimersByTimeAsync(CONNECT_MS + 1_000);
    expect(h.session.getState()).toBe("done");
    expect(h.published).toEqual(["offer", "end:hungUp"]);
    expect(h.reasons).not.toContain("error");
  });

  it("the ring timed out meanwhile: the same", async () => {
    const h = await dialled();
    hold.setRemoteDescription = new Gate();
    void h.session.applyIncomingSignaling(answer());
    await settle();

    await vi.advanceTimersByTimeAsync(RING_MS + 1);
    expect(h.session.getState()).toBe("done");
    hold.setRemoteDescription.open();
    await settle();

    expect(h.session.getState()).toBe("done");
    expect(h.states).not.toContain("connecting");
    expect(vi.getTimerCount()).toBe(0);
    expect(h.published).toEqual(["offer", "end:timedOut"]);
  });

  it("control: an answer on a live dial moves the call to connecting", async () => {
    const h = await dialled();
    await h.session.applyIncomingSignaling(answer());
    expect(h.session.getState()).toBe("connecting");
    await h.session.hangUp();
  });
});

// ---- A8: giving up on the transport ----------------------------------------------

describe("a call given up on while its transport is being described", () => {
  it("connect timeout, connected during the statistics read: the call is not ended", async () => {
    const h = await dialled();
    await h.session.applyIncomingSignaling(answer());
    expect(h.session.getState()).toBe("connecting");

    hold.getStats = new Gate();
    await vi.advanceTimersByTimeAsync(CONNECT_MS + 1);
    // The timeout fired and is parked on getStats; the transport comes up.
    h.pc.setIce("connected");
    expect(h.session.getState()).toBe("connected");
    hold.getStats.open();
    hold.getStats = null;
    await settle();

    expect(h.session.getState()).toBe("connected");
    expect(h.published).toEqual(["offer"]);
    await h.session.hangUp();
  });

  it("ICE grace expired, recovered during the statistics read: the call is not ended", async () => {
    const h = await connected();
    h.pc.setIce("disconnected");
    hold.getStats = new Gate();
    await vi.advanceTimersByTimeAsync(ICE_GRACE_MS + 1);

    h.pc.setIce("connected");
    hold.getStats.open();
    hold.getStats = null;
    await settle();

    expect(h.session.getState()).toBe("connected");
    expect(h.published).toEqual(["offer"]);
    await h.session.hangUp();
  });

  it("ICE failed, restored during the statistics read: the call is not ended", async () => {
    const h = await connected();
    hold.getStats = new Gate();
    h.pc.setIce("failed");
    await settle();

    h.pc.setIce("connected");
    hold.getStats.open();
    hold.getStats = null;
    await settle();

    expect(h.session.getState()).toBe("connected");
    await h.session.hangUp();
  });

  it("control: a connect timeout that still holds ends the call as an error", async () => {
    const h = await dialled();
    await h.session.applyIncomingSignaling(answer());
    await vi.advanceTimersByTimeAsync(CONNECT_MS + 1);
    await settle();
    expect(h.session.getState()).toBe("done");
    expect(h.published).toEqual(["offer", "end:error"]);
  });

  it("control: an ICE failure that still holds ends the call as an error", async () => {
    const h = await connected();
    h.pc.setIce("failed");
    await settle();
    expect(h.session.getState()).toBe("done");
    expect(h.published).toEqual(["offer", "end:error"]);
  });
});
