// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A group call that is left, fails or is refused while one of its awaits is
 * pending starts nothing afterwards.
 *
 * The step that was pending (a capture behind its permission prompt, the
 * relay credential, the socket still opening, an offer on a peer connection
 * that was closed meanwhile) finishes later, and must then find that the
 * session is over: every captured track stopped, no peer connection built or
 * left open, no timer running, no offer sent. And whoever awaited
 * `connect()` is told at once, not when the pending step returns, which it
 * may never do.
 *
 * Headless: a fake `getUserMedia` whose captures the test resolves by hand,
 * a fake `RTCPeerConnection` and a fake WebSocket the test opens by hand.
 * Fake timers, so "no timer is left" is `vi.getTimerCount() === 0`.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const hooks = vi.hoisted(() => ({
  /** Every SDP offer handed to the signaling composer. */
  offers: [] as string[],
}));

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    composeSfuSdpOfferWithMiddle: (sdp: string): string => {
      hooks.offers.push(sdp);
      return "{}";
    },
    composeSfuVideoWants: (): string => "{}",
    middleSenderWanted: (): boolean => false,
    composeSfuSdpAnswer: (): string => "{}",
    composeSfuIceCandidate: (): string => "{}",
    composeSfuPing: (): string => "{}",
    composeSfuSignalRelayToCall: (): string => "{}",
    composeMediaState: (): Uint8Array => new Uint8Array([0]),
    validateSfuServerFrame: (raw: string): string => (JSON.parse(raw) as { kind: string }).kind,
  }),
}));

vi.mock("./sframe-transform", async (importOriginal) => {
  const real = await importOriginal<typeof import("./sframe-transform")>();
  return {
    ...real,
    isSFrameTransformSupported: (): boolean => true,
    attachSenderSFrame: (): string => "sender-transform",
    attachReceiverSFrame: (): string => "receiver-transform",
    rekeySFrame: (): void => {},
  };
});

import { SfuCallOverError, SfuCallSession, type SfuCallConfig } from "./sfu-call-session";
import { SfuSignalingClient, SfuSignalingClosedError } from "./sfu-signaling-client";

// ---- Fakes ----------------------------------------------------------------

interface FakeTrack {
  kind: "audio" | "video";
  enabled: boolean;
  readyState: "live" | "ended";
  id: string;
  stop(): void;
  clone(): FakeTrack;
  addEventListener(): void;
}

/** Every track a capture handed out, and every clone made of one. */
let allTracks: FakeTrack[] = [];

function fakeTrack(kind: "audio" | "video"): FakeTrack {
  const t: FakeTrack = {
    kind,
    enabled: true,
    readyState: "live",
    id: `t${allTracks.length + 1}`,
    stop(): void {
      t.readyState = "ended";
    },
    clone: (): FakeTrack => fakeTrack(kind),
    addEventListener(): void {},
  };
  allTracks.push(t);
  return t;
}

class FakeStream {
  id = "local";
  constructor(readonly tracks: FakeTrack[] = []) {}
  getTracks(): FakeTrack[] {
    return [...this.tracks];
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
    const i = this.tracks.indexOf(t);
    if (i >= 0) this.tracks.splice(i, 1);
  }
}

/** A capture the test settles by hand. */
interface PendingCapture {
  readonly constraints: MediaStreamConstraints;
  /** Grant it: the stream the browser would hand over. */
  grant(): FakeStream;
  /** Refuse it, as a dismissed permission prompt does. */
  refuse(): void;
}

let captures: PendingCapture[] = [];

function fakeGetUserMedia(constraints: MediaStreamConstraints): Promise<FakeStream> {
  return new Promise<FakeStream>((resolve, reject) => {
    captures.push({
      constraints,
      grant: (): FakeStream => {
        const tracks: FakeTrack[] = [];
        if (constraints.audio !== undefined && constraints.audio !== false) tracks.push(fakeTrack("audio"));
        if (constraints.video !== undefined && constraints.video !== false) tracks.push(fakeTrack("video"));
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

interface FakeSender {
  track: FakeTrack | null;
  getParameters(): { encodings: Record<string, unknown>[] };
  setParameters(): Promise<void>;
  replaceTrack(t: FakeTrack): Promise<void>;
  getStats(): Promise<Map<string, Record<string, unknown>>>;
}

let pcs: FakePc[] = [];
/** How the fake browser's `createOffer` behaves. */
const browser = {
  /** `createOffer` never settles, as on a peer connection closed meanwhile. */
  offerNeverSettles: false,
  /** `replaceTrack` waits for the test (`replaceGate.release()`). */
  replaceGate: null as { release(): void; wait: Promise<void> } | null,
};

class FakePc {
  senders: FakeSender[] = [];
  transceivers: { sender: FakeSender; receiver: { track: { kind: string } }; mid: string | null; direction: string; stop(): void }[] = [];
  onnegotiationneeded: (() => void) | null = null;
  ontrack: unknown = null;
  onicecandidate: unknown = null;
  oniceconnectionstatechange: unknown = null;
  iceConnectionState = "new";
  signalingState = "stable";
  localDescription: { type: string; sdp: string } | null = null;
  closed = false;

  constructor() {
    pcs.push(this);
  }

  private sender(track: FakeTrack | null, kind: string, direction: string): FakeSender {
    const sender: FakeSender = {
      track,
      getParameters: () => ({ encodings: [{}] }),
      async setParameters(): Promise<void> {},
      async replaceTrack(t: FakeTrack): Promise<void> {
        if (browser.replaceGate !== null) await browser.replaceGate.wait;
        sender.track = t;
      },
      getStats: async () => new Map(),
    };
    this.senders.push(sender);
    this.transceivers.push({ sender, receiver: { track: { kind } }, mid: null, direction, stop(): void {} });
    return sender;
  }

  addTrack(track: FakeTrack): FakeSender {
    return this.sender(track, track.kind, "sendrecv");
  }

  addTransceiver(trackOrKind: FakeTrack | string, init: { direction?: string } = {}): unknown {
    const track = typeof trackOrKind === "string" ? null : trackOrKind;
    const kind = typeof trackOrKind === "string" ? trackOrKind : trackOrKind.kind;
    this.sender(track, kind, init.direction ?? "sendrecv");
    return this.transceivers[this.transceivers.length - 1];
  }

  removeTrack(sender: FakeSender): void {
    sender.track = null;
  }

  getSenders(): FakeSender[] {
    return this.senders;
  }

  getTransceivers(): unknown[] {
    return this.transceivers;
  }

  createOffer(): Promise<{ type: string; sdp: string }> {
    if (browser.offerNeverSettles) return new Promise(() => {});
    return Promise.resolve({ type: "offer", sdp: "offer-1" });
  }

  async setLocalDescription(desc?: { type: string; sdp: string }): Promise<void> {
    this.localDescription = desc ?? { type: "offer", sdp: "offer-implicit" };
    this.signalingState = "have-local-offer";
  }

  async setRemoteDescription(): Promise<void> {
    this.signalingState = "stable";
  }

  async getStats(): Promise<Map<string, Record<string, unknown>>> {
    return new Map();
  }

  close(): void {
    this.closed = true;
  }
}

let sockets: FakeWebSocket[] = [];
/** Whether a new socket opens by itself, as soon as it is created. */
let socketsOpenThemselves = true;

class FakeWebSocket {
  static OPEN = 1;
  readyState = 0;
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((ev: { code: number; reason: string }) => void) | null = null;
  sent: string[] = [];
  closedByClient = false;
  constructor() {
    sockets.push(this);
    if (socketsOpenThemselves) queueMicrotask(() => this.open());
  }
  open(): void {
    this.readyState = 1;
    this.onopen?.();
  }
  send(data: string): void {
    this.sent.push(data);
  }
  close(): void {
    this.closedByClient = true;
    this.readyState = 3;
  }
}

function config(over: Partial<SfuCallConfig> = {}): SfuCallConfig {
  return {
    sfuBaseUrl: "wss://sfu.test",
    jwt: "jwt",
    seed: new Uint8Array(32).fill(1),
    localLeaf: 2,
    media: { audio: true, video: true },
    ...over,
  };
}

/** Let every pending promise continuation run. No timer is advanced. */
async function settle(): Promise<void> {
  for (let i = 0; i < 50; i += 1) await Promise.resolve();
}

function capture(i: number): PendingCapture {
  const c = captures[i];
  if (c === undefined) throw new Error(`no capture ${i} (have ${captures.length})`);
  return c;
}

/** The outcome of a promise, read without awaiting it. */
function watch(p: Promise<unknown>): { settled(): "pending" | "resolved" | "rejected"; error(): unknown } {
  let state: "pending" | "resolved" | "rejected" = "pending";
  let error: unknown;
  p.then(
    () => {
      state = "resolved";
    },
    (err: unknown) => {
      state = "rejected";
      error = err;
    },
  );
  return { settled: () => state, error: () => error };
}

function liveTracks(): FakeTrack[] {
  return allTracks.filter((t) => t.readyState === "live");
}

function openPcs(): FakePc[] {
  return pcs.filter((pc) => !pc.closed);
}

beforeEach(() => {
  allTracks = [];
  captures = [];
  pcs = [];
  sockets = [];
  socketsOpenThemselves = true;
  browser.offerNeverSettles = false;
  browser.replaceGate = null;
  hooks.offers.length = 0;
  vi.useFakeTimers();
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.spyOn(console, "info").mockImplementation(() => {});
  vi.stubGlobal("navigator", { mediaDevices: { getUserMedia: fakeGetUserMedia } });
  vi.stubGlobal("RTCPeerConnection", FakePc);
  vi.stubGlobal("WebSocket", FakeWebSocket);
  vi.stubGlobal("MediaStream", FakeStream);
  vi.stubGlobal("RTCRtpScriptTransform", class {});
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

/** A session whose `connect()` is parked on its first capture. */
async function parkedOnCapture(over: Partial<SfuCallConfig> = {}): Promise<{
  session: SfuCallSession;
  connect: ReturnType<typeof watch>;
}> {
  const session = new SfuCallSession(config(over));
  const connect = watch(session.connect());
  await settle();
  expect(captures).toHaveLength(1);
  expect(connect.settled()).toBe("pending");
  return { session, connect };
}

/** Nothing of the call is left running on this device. */
function expectNothingLeft(): void {
  expect(liveTracks()).toEqual([]);
  expect(openPcs()).toEqual([]);
  expect(vi.getTimerCount()).toBe(0);
  expect(hooks.offers).toEqual([]);
}

// ---- A1: the capture ---------------------------------------------------------

describe("a group call that ends while the microphone and camera are being opened", () => {
  it("left by the person: connect() settles at once, and the capture that arrives later is released", async () => {
    const { session, connect } = await parkedOnCapture();

    session.hangUp();
    await settle();
    // Told at once, while the permission prompt is still open.
    expect(connect.settled()).toBe("rejected");
    expect(connect.error()).toBeInstanceOf(SfuCallOverError);
    expect((connect.error() as SfuCallOverError).state).toBe("closed");

    // The prompt is answered afterwards.
    const stream = capture(0).grant();
    await settle();
    expect(stream.getTracks()).toHaveLength(2);
    expect(stream.getTracks().every((t) => t.readyState === "ended")).toBe(true);
    // No peer connection was built at all, not one built and then closed.
    expect(pcs).toEqual([]);
    expectNothingLeft();
    expect(session.getState()).toBe("closed");
  });

  it("refused by the SFU as already over: nothing is captured into a dead session", async () => {
    const { session, connect } = await parkedOnCapture();

    // The SFU's answer in place of the Welcome.
    sockets[0]?.onmessage?.({ data: JSON.stringify({ kind: "error", code: "call-ended", message: "" }) });
    await settle();
    expect(session.getState()).toBe("failed");
    expect(session.callEndedByServer()).toBe(true);
    expect(connect.settled()).toBe("rejected");
    expect((connect.error() as SfuCallOverError).state).toBe("failed");

    capture(0).grant();
    await settle();
    expect(pcs).toEqual([]);
    expectNothingLeft();
  });

  it("the signaling socket dropped: the same", async () => {
    const { session, connect } = await parkedOnCapture();

    sockets[0]?.onclose?.({ code: 1006, reason: "" });
    await settle();
    expect(session.getState()).toBe("failed");
    expect(connect.settled()).toBe("rejected");

    capture(0).grant();
    await settle();
    expect(pcs).toEqual([]);
    expectNothingLeft();
  });

  it("the camera failed and the call ended before the microphone-only attempt: no second prompt", async () => {
    const { session } = await parkedOnCapture();

    session.hangUp();
    capture(0).refuse();
    await settle();
    // The fallback (`capture-fallback.ts`) would ask for the microphone alone.
    expect(captures).toHaveLength(1);
    expectNothingLeft();
  });

  it("left while the relay credential was being fetched: the stream is stopped and no peer connection is built", async () => {
    const session = new SfuCallSession(config());
    let grantTurn: (server: RTCIceServer | null) => void = () => {};
    session.setTurnProvider(
      () =>
        new Promise<RTCIceServer | null>((resolve) => {
          grantTurn = resolve;
        }),
    );
    const connect = watch(session.connect());
    await settle();
    const stream = capture(0).grant();
    await settle();
    // Parked on the credential, with the devices open.
    expect(liveTracks()).toHaveLength(2);
    expect(pcs).toEqual([]);

    session.hangUp();
    await settle();
    expect(connect.settled()).toBe("rejected");
    grantTurn({ urls: "turn:relay.test" });
    await settle();

    expect(stream.getTracks().every((t) => t.readyState === "ended")).toBe(true);
    expect(pcs).toEqual([]);
    expectNothingLeft();
  });

  it("a deferred publish (the call-link path) hung up during its capture releases it too", async () => {
    const session = new SfuCallSession(config({ deferPublish: true }));
    await session.connect();
    const publish = watch(session.publish(new Uint8Array(32).fill(3), 4));
    await settle();
    expect(captures).toHaveLength(1);

    session.hangUp();
    await settle();
    // Nothing to report for a publish the person walked away from.
    expect(publish.settled()).toBe("resolved");

    capture(0).grant();
    await settle();
    expect(pcs).toEqual([]);
    expectNothingLeft();
  });

  it("a session that is over opens no device for a late publish", async () => {
    const session = new SfuCallSession(config({ deferPublish: true }));
    await session.connect();
    session.hangUp();

    await session.publish(new Uint8Array(32).fill(3), 4);
    await settle();
    expect(captures).toEqual([]);
    expectNothingLeft();
  });

  it("control: a call nobody left publishes, and its teardown leaves nothing", async () => {
    const { session, connect } = await parkedOnCapture();
    capture(0).grant();
    await settle();
    expect(connect.settled()).toBe("resolved");
    expect(openPcs()).toHaveLength(1);
    expect(hooks.offers).toEqual(["offer-1"]);
    expect(liveTracks().length).toBeGreaterThan(0);
    expect(vi.getTimerCount()).toBeGreaterThan(0);

    session.hangUp();
    await settle();
    expect(liveTracks()).toEqual([]);
    expect(openPcs()).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
  });
});

// ---- A2: connect() always settles -------------------------------------------

describe("a group call closed while it is still connecting", () => {
  it("the signaling client rejects a connect its own close() interrupted", async () => {
    socketsOpenThemselves = false;
    const client = new SfuSignalingClient("wss://sfu.test", "jwt");
    const connect = watch(client.connect());
    await settle();
    expect(sockets).toHaveLength(1);
    expect(connect.settled()).toBe("pending");

    client.close();
    await settle();
    expect(connect.settled()).toBe("rejected");
    expect(connect.error()).toBeInstanceOf(SfuSignalingClosedError);
    expect(sockets[0]?.closedByClient).toBe(true);
  });

  it("the signaling client closed before its module loaded opens no socket", async () => {
    const client = new SfuSignalingClient("wss://sfu.test", "jwt");
    const connect = watch(client.connect());
    // Closed in the same turn: `connect()` is parked on the module load.
    client.close();
    await settle();
    expect(connect.settled()).toBe("rejected");
    expect(connect.error()).toBeInstanceOf(SfuSignalingClosedError);
    expect(sockets).toEqual([]);
  });

  it("hung up while the socket was opening: connect() settles and nothing is left", async () => {
    socketsOpenThemselves = false;
    const session = new SfuCallSession(config());
    const connect = watch(session.connect());
    await settle();
    expect(connect.settled()).toBe("pending");

    session.hangUp();
    await settle();
    expect(connect.settled()).toBe("rejected");
    expect(session.getState()).toBe("closed");
    expect(captures).toEqual([]);
    expectNothingLeft();

    // The socket opening afterwards reaches nobody.
    sockets[0]?.open();
    await settle();
    expect(captures).toEqual([]);
    expectNothingLeft();
  });

  it("hung up in the turn the socket opened: no keepalive timer is armed", async () => {
    socketsOpenThemselves = false;
    const session = new SfuCallSession(config({ deferPublish: true }));
    const connect = watch(session.connect());
    await settle();
    sockets[0]?.open();
    session.hangUp();
    await settle();
    expect(connect.settled()).toBe("rejected");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("an offer the browser never settles does not hold connect() after the call is left", async () => {
    browser.offerNeverSettles = true;
    const { session, connect } = await parkedOnCapture();
    capture(0).grant();
    await settle();
    // Parked on createOffer, with a peer connection and the devices open.
    expect(openPcs()).toHaveLength(1);
    expect(connect.settled()).toBe("pending");

    session.hangUp();
    await settle();
    expect(connect.settled()).toBe("rejected");
    expect(connect.error()).toBeInstanceOf(SfuCallOverError);
    expectNothingLeft();
  });
});

// ---- A3 and the camera turned on mid-call -------------------------------------

describe("a device change that finishes after the group call was torn down", () => {
  async function connectedAudioVideo(): Promise<SfuCallSession> {
    const { session } = await parkedOnCapture();
    capture(0).grant();
    await settle();
    expect(session.getState()).toBe("connecting");
    return session;
  }

  it("switching the microphone: the new capture is stopped, and nothing throws", async () => {
    const session = await connectedAudioVideo();
    const switched = watch(session.switchAudioInput("mic-2"));
    await settle();
    expect(captures).toHaveLength(2);

    session.hangUp();
    const stream = capture(1).grant();
    await settle();

    expect(switched.settled()).toBe("resolved");
    expect(stream.getTracks()).toHaveLength(1);
    expect(stream.getTracks().every((t) => t.readyState === "ended")).toBe(true);
    expectNothingLeftAfterPublish();
  });

  it("switching the camera: the same", async () => {
    const session = await connectedAudioVideo();
    const switched = watch(session.switchVideoInput("cam-2"));
    await settle();

    session.hangUp();
    capture(1).grant();
    await settle();

    expect(switched.settled()).toBe("resolved");
    expectNothingLeftAfterPublish();
  });

  it("torn down while the sender was taking the new track: the new track is stopped", async () => {
    const session = await connectedAudioVideo();
    let release: () => void = () => {};
    browser.replaceGate = {
      wait: new Promise<void>((resolve) => {
        release = resolve;
      }),
      release: (): void => release(),
    };
    const switched = watch(session.switchAudioInput("mic-2"));
    await settle();
    capture(1).grant();
    await settle();
    // Parked on replaceTrack, the new microphone open.
    expect(liveTracks().some((t) => t.kind === "audio")).toBe(true);

    session.hangUp();
    browser.replaceGate.release();
    await settle();

    expect(switched.settled()).toBe("resolved");
    expectNothingLeftAfterPublish();
  });

  it("the camera turned on mid-call and the call left during its prompt: the camera is released", async () => {
    const { session } = await parkedOnCapture({ media: { audio: true, video: false } });
    capture(0).grant();
    await settle();

    const publishing = watch(session.publishVideo());
    await settle();
    expect(captures).toHaveLength(2);

    session.hangUp();
    const stream = capture(1).grant();
    await settle();

    expect(publishing.settled()).toBe("resolved");
    expect(stream.getTracks().every((t) => t.readyState === "ended")).toBe(true);
    expectNothingLeftAfterPublish();
    expect(session.getVideoPublishState()).toBe("off");
  });

  /** As {@link expectNothingLeft}, for a call whose offer did go out. */
  function expectNothingLeftAfterPublish(): void {
    expect(liveTracks()).toEqual([]);
    expect(openPcs()).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
    expect(hooks.offers).toEqual(["offer-1"]);
  }
});
