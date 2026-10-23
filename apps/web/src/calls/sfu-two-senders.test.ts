// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The group call publishes the camera as ordinary senders (ADR-0070 steps 1
 * and 5): the main one, a light one and a middle one, never simulcast; and it
 * tells the SFU which picture of each person this device shows.
 *
 * Headless: a fake `RTCPeerConnection`, fake tracks and a fake WebSocket. The
 * signaling client is the real one and so are the wasm helpers it calls, so
 * what is asserted about an offer or a `video-wants` frame is what reaches
 * `composeSfuSdpOfferWithMiddle(sdp, lightMids, middleMids)` and
 * `composeSfuVideoWants(peers, qualities)`, and whether the middle sender
 * sends is the shared rule's own answer (`middleSenderWanted`). The SFrame
 * attach is recorded, with the stream index the real module would seal
 * under.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

// ---- Fakes ----------------------------------------------------------------

interface FakeTrack {
  kind: "audio" | "video";
  label: string;
  enabled: boolean;
  readyState: "live" | "ended";
  id: string;
  cloneOf: FakeTrack | null;
  stop(): void;
  clone(): FakeTrack;
  addEventListener(): void;
}

interface FakeSender {
  track: FakeTrack | null;
  params: { encodings: Record<string, unknown>[]; degradationPreference?: string };
  setParametersCalls: unknown[];
  replaced: FakeTrack[];
  getParameters(): { encodings: Record<string, unknown>[]; degradationPreference?: string };
  setParameters(p: unknown): Promise<void>;
  replaceTrack(t: FakeTrack): Promise<void>;
  getStats(): Promise<Map<string, Record<string, unknown>>>;
}

interface FakeTransceiver {
  sender: FakeSender;
  receiver: { track: { kind: string } };
  mid: string | null;
  direction: string;
  /** Which copy of the camera a send-only transceiver carries, read from the
   *  size it was asked for. */
  copy: "light" | "middle" | null;
  init: RTCRtpTransceiverInit | null;
  via: "addTrack" | "addTransceiver";
  stopped: boolean;
  stop(): void;
}

const hooks = vi.hoisted(() => ({
  /** Every `composeSfuSdpOfferWithMiddle(sdp, lightMids, middleMids)` call. */
  offers: [] as Array<{ sdp: string; lightMids: string[]; middleMids: string[] }>,
  /** Every `composeSfuVideoWants(peers, qualities)` call. */
  wants: [] as Array<{ peers: string[]; qualities: string[] }>,
  /** The roster of the Welcome the fake SFU sends as the socket opens;
   *  `null` for none. */
  welcomeRoster: null as string[] | null,
  /** SFrame attaches, in order, with the index the real module seals under. */
  attaches: [] as Array<{ sender: unknown; streamIndex: number; trackKind: string }>,
  /** The receivers a decrypting transform was attached to. */
  receiverAttaches: [] as unknown[],
  /** Make the attach of a given stream index throw. */
  attachFailsFor: null as number | null,
  /** What happened, in order: attaches, offers and wants. */
  events: [] as string[],
}));

vi.mock("../wasm", async () => {
  // The real helpers and the real rule; `loadWasm` itself is replaced so no
  // module is fetched.
  const real = await import("../../wasm/lakko_ffi_wasm");
  return {
  loadWasm: async () => ({
    composeSfuSdpOfferWithMiddle: (sdp: string, lightMids: string[], middleMids: string[]): string => {
      hooks.offers.push({ sdp, lightMids: [...lightMids], middleMids: [...middleMids] });
      hooks.events.push("offer-sent");
      return real.composeSfuSdpOfferWithMiddle(sdp, lightMids, middleMids);
    },
    composeSfuVideoWants: (peers: string[], qualities: string[]): string => {
      hooks.wants.push({ peers: [...peers], qualities: [...qualities] });
      hooks.events.push("wants-sent");
      return real.composeSfuVideoWants(peers, qualities);
    },
    middleSenderWanted: (people: number): boolean => real.middleSenderWanted(people),
    parseSfuVideoGiven: (json: string): string[] => real.parseSfuVideoGiven(json),
    parseSfuVideoNeeded: (json: string): Uint8Array => real.parseSfuVideoNeeded(json),
    parseSfuPublishBudget: (json: string): Uint32Array => real.parseSfuPublishBudget(json),
    composeSfuSdpAnswer: (sdp: string): string => JSON.stringify({ kind: "sdp-answer", sdp }),
    composeSfuIceCandidate: (): string => "{}",
    composeSfuPing: (): string => "{}",
    composeSfuSignalRelayToCall: (): string => "{}",
    composeMediaState: (): Uint8Array => new Uint8Array([0]),
    validateSfuServerFrame: (raw: string): string => (JSON.parse(raw) as { kind: string }).kind,
  }),
  };
});

vi.mock("./sframe-transform", async (importOriginal) => {
  const real = await importOriginal<typeof import("./sframe-transform")>();
  return {
    ...real,
    isSFrameTransformSupported: (): boolean => true,
    attachSenderSFrame: (
      sender: { track: { kind: string } | null },
      opts: { streamIndex?: number },
    ): string => {
      const streamIndex = opts.streamIndex ?? real.streamIndexOfKind(sender.track?.kind ?? "audio");
      if (hooks.attachFailsFor === streamIndex) throw new Error("transform refused");
      hooks.attaches.push({ sender, streamIndex, trackKind: sender.track?.kind ?? "-" });
      hooks.events.push(`attach-${streamIndex}`);
      return `transform-${hooks.attaches.length}`;
    },
    attachReceiverSFrame: (receiver: unknown): string => {
      hooks.receiverAttaches.push(receiver);
      return "receiver";
    },
    rekeySFrame: (): void => {},
  };
});

import { SfuCallSession, type SfuCallConfig } from "./sfu-call-session";
import { shownLayerLedger } from "./layer-display";
import { videoFrameSourceOf } from "./video-frame-watch";

/** Every clone made of a captured track, oldest first. */
const clones: FakeTrack[] = [];

let trackSeq = 0;
function fakeTrack(kind: "audio" | "video", label: string, cloneOf: FakeTrack | null = null): FakeTrack {
  trackSeq += 1;
  const t: FakeTrack = {
    kind,
    label,
    enabled: true,
    readyState: "live",
    id: `t${trackSeq}`,
    cloneOf,
    stop(): void {
      t.readyState = "ended";
    },
    clone(): FakeTrack {
      const c = fakeTrack(kind, label, t);
      c.enabled = t.enabled;
      clones.push(c);
      return c;
    },
    addEventListener(): void {},
  };
  return t;
}

class FakeStream {
  constructor(readonly tracks: FakeTrack[] = []) {}
  id = "local";
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

/** How the fake browser treats the camera's copies (the send-only
 *  transceivers: the light sender and the middle sender). */
const browser = {
  /** `addTransceiver` throws for a send-only transceiver given encodings. */
  refusesLightEncodings: false,
  /** `addTransceiver` throws for every send-only transceiver. */
  refusesLightTransceiver: false,
  /** `setParameters` rejects on a send-only transceiver's sender. */
  refusesLightSetParameters: false,
  /** `setParameters` rejects only when it changes an encoding's `active`. */
  refusesActiveChange: false,
};

/** The copy a send-only transceiver's size names. */
function copyOfScale(scale: unknown): "light" | "middle" | null {
  return scale === 4 ? "light" : scale === 2 ? "middle" : null;
}

const pcs: FakePc[] = [];

class FakePc {
  transceivers: FakeTransceiver[] = [];
  onnegotiationneeded: (() => void) | null = null;
  ontrack: unknown = null;
  onicecandidate: unknown = null;
  oniceconnectionstatechange: unknown = null;
  iceConnectionState = "new";
  signalingState = "stable";
  localDescription: { type: string; sdp: string } | null = null;
  removed: FakeSender[] = [];
  closed = false;
  private offerSeq = 0;
  /** The copy whose encodings `addTransceiver` last refused: the plain
   *  transceiver that follows is that copy's. */
  private refusedCopy: "light" | "middle" | null = null;
  stats = new Map<string, Record<string, unknown>>();

  constructor() {
    pcs.push(this);
  }

  private make(
    track: FakeTrack | null,
    kind: string,
    direction: string,
    init: RTCRtpTransceiverInit | null,
    via: "addTrack" | "addTransceiver",
  ): FakeTransceiver {
    const sendOnly = direction === "sendonly";
    const copy = !sendOnly
      ? null
      : (copyOfScale(init?.sendEncodings?.[0]?.scaleResolutionDownBy) ?? this.refusedCopy);
    this.refusedCopy = null;
    const sender: FakeSender = {
      track,
      params: { encodings: init?.sendEncodings?.map((e) => ({ ...e })) ?? [{}] },
      setParametersCalls: [],
      replaced: [],
      getParameters: () => ({
        ...sender.params,
        encodings: sender.params.encodings.map((e) => ({ ...e })),
      }),
      async setParameters(p: unknown): Promise<void> {
        sender.setParametersCalls.push(p);
        if (sendOnly && browser.refusesLightSetParameters) throw new Error("InvalidModificationError");
        const next = p as FakeSender["params"];
        if (
          browser.refusesActiveChange &&
          (next.encodings[0]?.["active"] ?? true) !== (sender.params.encodings[0]?.["active"] ?? true)
        ) {
          throw new Error("InvalidStateError");
        }
        sender.params = next;
      },
      async replaceTrack(t: FakeTrack): Promise<void> {
        sender.replaced.push(t);
        sender.track = t;
      },
      getStats: async () => {
        const id = copy === "middle" ? "OD" : "OL";
        return new Map([[id, { id, type: "outbound-rtp", kind: "video" }]]);
      },
    };
    const t: FakeTransceiver = {
      sender,
      receiver: { track: { kind } },
      mid: null,
      direction,
      copy,
      init,
      via,
      stopped: false,
      stop(): void {
        t.stopped = true;
      },
    };
    this.transceivers.push(t);
    return t;
  }

  addTrack(track: FakeTrack): FakeSender {
    return this.make(track, track.kind, "sendrecv", null, "addTrack").sender;
  }

  addTransceiver(trackOrKind: FakeTrack | string, init: RTCRtpTransceiverInit = {}): FakeTransceiver {
    if (typeof trackOrKind === "string") {
      return this.make(null, trackOrKind, init.direction ?? "sendrecv", init, "addTransceiver");
    }
    if (init.direction === "sendonly") {
      if (browser.refusesLightTransceiver) throw new Error("OperationError");
      if (browser.refusesLightEncodings && init.sendEncodings !== undefined) {
        this.refusedCopy = copyOfScale(init.sendEncodings[0]?.scaleResolutionDownBy);
        throw new TypeError("sendEncodings refused");
      }
    }
    return this.make(trackOrKind, trackOrKind.kind, init.direction ?? "sendrecv", init, "addTransceiver");
  }

  removeTrack(sender: FakeSender): void {
    this.removed.push(sender);
    sender.track = null;
  }

  getSenders(): FakeSender[] {
    return this.transceivers.filter((t) => !t.stopped).map((t) => t.sender);
  }

  getTransceivers(): FakeTransceiver[] {
    return this.transceivers.filter((t) => !t.stopped);
  }

  async createOffer(): Promise<{ type: string; sdp: string }> {
    hooks.events.push("createOffer");
    this.offerSeq += 1;
    return { type: "offer", sdp: `offer-${this.offerSeq}` };
  }

  async setLocalDescription(desc?: { type: string; sdp: string }): Promise<void> {
    if (desc === undefined) {
      hooks.events.push("createOffer");
      this.offerSeq += 1;
    }
    // A local description gives every transceiver in it a mid.
    this.transceivers.forEach((t, i) => {
      if (!t.stopped && t.mid === null) t.mid = String(i);
    });
    this.localDescription = desc ?? { type: "offer", sdp: `offer-${this.offerSeq}` };
    this.signalingState = "have-local-offer";
  }

  async setRemoteDescription(): Promise<void> {
    this.signalingState = "stable";
  }

  async getStats(): Promise<Map<string, Record<string, unknown>>> {
    return this.stats;
  }

  close(): void {
    this.closed = true;
  }

  /** The light senders still in place. */
  lights(): FakeTransceiver[] {
    return this.getTransceivers().filter((t) => t.copy === "light");
  }

  /** The middle senders still in place. */
  middles(): FakeTransceiver[] {
    return this.getTransceivers().filter((t) => t.copy === "middle");
  }

  /** Every transceiver ever added for a copy, removed ones included. */
  everAdded(copy: "light" | "middle"): FakeTransceiver[] {
    return this.transceivers.filter((t) => t.copy === copy);
  }

  /** The camera's main transceiver. */
  main(): FakeTransceiver | undefined {
    return this.getTransceivers().find(
      (t) => t.direction !== "sendonly" && t.sender.track?.kind === "video",
    );
  }
}

const sockets: FakeWebSocket[] = [];

class FakeWebSocket {
  static OPEN = 1;
  readyState = 1;
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onerror: unknown = null;
  onclose: unknown = null;
  sent: string[] = [];
  constructor() {
    sockets.push(this);
    queueMicrotask(() => {
      this.onopen?.();
      if (hooks.welcomeRoster !== null) welcome(hooks.welcomeRoster);
    });
  }
  send(data: string): void {
    this.sent.push(data);
  }
  close(): void {}
}

/** The tracks `getUserMedia` handed out, newest last. */
const captured: FakeTrack[] = [];

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

function flush(): Promise<void> {
  return new Promise((r) => setTimeout(r, 0));
}

/** The SFU's Welcome: this device is `self`, the others are `others`. */
function welcome(others: readonly string[]): void {
  sockets[0]?.onmessage?.({
    data: JSON.stringify({
      kind: "welcome",
      peer_id: "self",
      room: "room",
      roster: others.map((peer_id) => ({ peer_id })),
      server_ts: 0,
    }),
  });
}

function peerJoined(peerId: string): void {
  sockets[0]?.onmessage?.({
    data: JSON.stringify({ kind: "peer-joined", peer_id: peerId }),
  });
}

function peerLeft(peerId: string): void {
  sockets[0]?.onmessage?.({ data: JSON.stringify({ kind: "peer-left", peer_id: peerId }) });
}

function copyWarnings(spy: { mock: { calls: unknown[][] } }, copy: "light" | "middle"): unknown[][] {
  return spy.mock.calls.filter((c: unknown[]) => String(c[0]).includes(`without a ${copy} sender`));
}

/** The SFU answers the offer that is out. */
async function answer(): Promise<void> {
  sockets[0]?.onmessage?.({ data: JSON.stringify({ kind: "sdp-answer", sdp: "answer" }) });
  await flush();
}

/** The browser asks for a renegotiation, as it does after a sender is added. */
async function negotiate(pc: FakePc): Promise<void> {
  pc.onnegotiationneeded?.();
  await flush();
}

function lightWarnings(spy: { mock: { calls: unknown[][] } }): unknown[][] {
  return copyWarnings(spy, "light");
}

async function connected(over: Partial<SfuCallConfig> = {}): Promise<{ session: SfuCallSession; pc: FakePc }> {
  const session = new SfuCallSession(config(over));
  await session.connect();
  const pc = pcs[0];
  if (pc === undefined) throw new Error("no peer connection");
  return { session, pc };
}

let warn: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  hooks.offers = [];
  hooks.wants = [];
  hooks.welcomeRoster = null;
  hooks.attaches = [];
  hooks.receiverAttaches = [];
  hooks.attachFailsFor = null;
  hooks.events = [];
  pcs.length = 0;
  sockets.length = 0;
  captured.length = 0;
  clones.length = 0;
  browser.refusesLightEncodings = false;
  browser.refusesLightTransceiver = false;
  browser.refusesLightSetParameters = false;
  browser.refusesActiveChange = false;
  warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.stubGlobal("RTCPeerConnection", FakePc);
  vi.stubGlobal("WebSocket", FakeWebSocket);
  vi.stubGlobal("MediaStream", FakeStream);
  vi.stubGlobal("navigator", {
    mediaDevices: {
      getUserMedia: async (c: { audio?: unknown; video?: unknown }) => {
        const tracks: FakeTrack[] = [];
        if (c.audio !== undefined && c.audio !== false) tracks.push(fakeTrack("audio", "mic"));
        if (c.video !== undefined && c.video !== false) {
          const exact = (c.video as { deviceId?: { exact?: string } }).deviceId?.exact;
          tracks.push(fakeTrack("video", exact ?? "default-camera"));
        }
        captured.push(...tracks);
        return new FakeStream(tracks);
      },
    },
  });
});

afterEach(() => {
  warn.mockRestore();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

// ---- The first publish ----------------------------------------------------

describe("the first publish of a video call", () => {
  it("creates the main sender with one encoding and no rid, and a light sender beside it", async () => {
    const { pc } = await connected();
    const main = pc.main();
    expect(main?.via).toBe("addTransceiver");
    expect(main?.init?.direction).toBe("sendrecv");
    expect(main?.init?.sendEncodings).toEqual([{ scalabilityMode: "L1T3" }]);
    expect(main?.init?.sendEncodings?.[0]).not.toHaveProperty("rid");

    const lights = pc.lights();
    expect(lights).toHaveLength(1);
    const light = lights[0];
    expect(light?.init).toEqual({
      direction: "sendonly",
      sendEncodings: [{ scaleResolutionDownBy: 4, maxBitrate: 150_000 }],
    });
    // A clone of the camera, not the camera track itself.
    const camera = main?.sender.track;
    expect(light?.sender.track).not.toBe(camera);
    expect(light?.sender.track?.cloneOf).toBe(camera);
    // No transceiver anywhere carries a rid or two encodings.
    for (const t of pc.transceivers) {
      for (const e of t.init?.sendEncodings ?? []) expect(e).not.toHaveProperty("rid");
      expect((t.init?.sendEncodings ?? []).length).toBeLessThanOrEqual(1);
    }
  });

  it("seals the microphone under 0, the main camera under 1 and the light sender under 3, all before the offer", async () => {
    const { pc } = await connected();
    expect(hooks.attaches.map((a) => a.streamIndex).sort()).toEqual([0, 1, 3, 4]);
    expect(hooks.attaches.find((a) => a.streamIndex === 1)?.sender).toBe(pc.main()?.sender);
    expect(hooks.attaches.find((a) => a.streamIndex === 3)?.sender).toBe(pc.lights()[0]?.sender);
    const firstOffer = hooks.events.indexOf("createOffer");
    expect(firstOffer).toBeGreaterThan(0);
    expect(hooks.events.slice(0, firstOffer).filter((e) => e.startsWith("attach-")).sort()).toEqual([
      "attach-0",
      "attach-1",
      "attach-3",
      "attach-4",
    ]);
  });

  it("names the light transceiver's mid in the offer it sends", async () => {
    const { pc } = await connected();
    const mid = pc.lights()[0]?.mid;
    expect(mid).toBeTypeOf("string");
    expect(hooks.offers).toEqual([
      { sdp: "offer-1", lightMids: [mid], middleMids: [pc.middles()[0]?.mid] },
    ]);
    expect(pc.main()?.mid).not.toBe(mid);
  });

  it("sends an empty list without a camera, and adds no light sender", async () => {
    const { pc } = await connected({ media: { audio: true, video: false } });
    expect(pc.lights()).toHaveLength(0);
    expect(pc.middles()).toHaveLength(0);
    expect(hooks.offers).toEqual([{ sdp: "offer-1", lightMids: [], middleMids: [] }]);
    expect(hooks.attaches.map((a) => a.streamIndex)).toEqual([0]);
  });
});

// ---- The fallbacks --------------------------------------------------------

describe("a browser that is awkward about the light sender", () => {
  it("first fallback: the transceiver is added plainly and the same values are set with setParameters", async () => {
    browser.refusesLightEncodings = true;
    const { pc } = await connected();
    const lights = pc.lights();
    expect(lights).toHaveLength(1);
    expect(lights[0]?.init).toEqual({ direction: "sendonly" });
    expect(lights[0]?.sender.setParametersCalls).toEqual([
      {
        encodings: [{ scaleResolutionDownBy: 4, maxBitrate: 150_000 }],
        degradationPreference: "maintain-framerate",
      },
    ]);
    // Sealed before the offer here too, and named in it.
    const firstOffer = hooks.events.indexOf("createOffer");
    expect(hooks.events.indexOf("attach-3")).toBeGreaterThanOrEqual(0);
    expect(hooks.events.indexOf("attach-3")).toBeLessThan(firstOffer);
    expect(hooks.offers[0]?.lightMids).toEqual([lights[0]?.mid]);
    expect(lightWarnings(warn)).toHaveLength(0);
  });

  it("second fallback: with setParameters refused too, the call is published without a light sender, said once", async () => {
    browser.refusesLightEncodings = true;
    browser.refusesLightSetParameters = true;
    const { session, pc } = await connected();
    expect(pc.lights()).toHaveLength(0);
    // The removed sender's clone is stopped: nothing is sent a second time at full size.
    const dropped = pc.everAdded("light")[0];
    expect(dropped?.stopped).toBe(true);
    expect(pc.removed).toContain(dropped?.sender);
    // The middle sender met the same refusals and went the same way.
    expect(pc.middles()).toHaveLength(0);
    expect(clones).toHaveLength(2);
    expect(clones.every((c) => c.readyState === "ended")).toBe(true);
    expect(captured.filter((t) => t.kind === "video")).toHaveLength(1);
    expect(hooks.offers).toEqual([{ sdp: "offer-1", lightMids: [], middleMids: [] }]);
    expect(pc.main()?.sender.track?.readyState).toBe("live");
    expect(session.getState()).toBe("connecting");
    expect(lightWarnings(warn)).toHaveLength(1);
  });

  it("publishes without a light sender when no second transceiver is taken at all, said once", async () => {
    browser.refusesLightTransceiver = true;
    const { session, pc } = await connected();
    expect(pc.transceivers.filter((t) => t.direction === "sendonly")).toHaveLength(0);
    expect(hooks.offers).toEqual([{ sdp: "offer-1", lightMids: [], middleMids: [] }]);
    expect(hooks.attaches.map((a) => a.streamIndex).sort()).toEqual([0, 1]);
    expect(lightWarnings(warn)).toHaveLength(1);
    expect(copyWarnings(warn, "middle")).toHaveLength(1);

    // Turning the camera off and on again does not say it a second time.
    await answer();
    session.setCameraEnabled(false);
    await session.publishVideo();
    expect(lightWarnings(warn)).toHaveLength(1);
    expect(copyWarnings(warn, "middle")).toHaveLength(1);
  });

  it("a failed SFrame attach removes the light sender and the call goes on (invariant #4)", async () => {
    hooks.attachFailsFor = 3;
    const { session, pc } = await connected();
    expect(pc.lights()).toHaveLength(0);
    const dropped = pc.everAdded("light")[0];
    expect(dropped).toBeDefined();
    expect(dropped?.stopped).toBe(true);
    expect(pc.removed).toContain(dropped?.sender);
    // Its clone is stopped, so the unsealed sender has nothing to send.
    expect(dropped?.init).not.toBeNull();
    expect(clones[0]?.readyState).toBe("ended");
    // The middle sender is another sender with another key: it stays.
    expect(pc.middles()).toHaveLength(1);
    expect(clones[1]?.readyState).toBe("live");
    expect(hooks.attaches.map((a) => a.streamIndex).sort()).toEqual([0, 1, 4]);
    expect(hooks.offers).toEqual([
      { sdp: "offer-1", lightMids: [], middleMids: [pc.middles()[0]?.mid] },
    ]);
    expect(session.getState()).toBe("connecting");
    expect(pc.closed).toBe(false);
    expect(lightWarnings(warn)).toHaveLength(1);
  });
});

// ---- Camera on mid-call ---------------------------------------------------

describe("turning the camera on mid-call", () => {
  it("adds the main sender on a transceiver of its own, one encoding and no rid, and the light sender, and names its mid in the renegotiation", async () => {
    const { session, pc } = await connected({ media: { audio: true, video: false } });
    expect(hooks.offers).toEqual([{ sdp: "offer-1", lightMids: [], middleMids: [] }]);
    await answer();

    await session.publishVideo();
    const main = pc.main();
    // Never addTrack: mid-call that would take the transceiver the SFU opened
    // for another person's camera, and the SFU would not take this camera as
    // this person's main picture.
    expect(main?.via).toBe("addTransceiver");
    expect(main?.init?.direction).toBe("sendrecv");
    expect(main?.init?.sendEncodings).toEqual([{ scalabilityMode: "L1T3" }]);
    expect(main?.init?.sendEncodings?.[0]).not.toHaveProperty("rid");
    const lights = pc.lights();
    expect(lights).toHaveLength(1);
    expect(lights[0]?.init).toEqual({
      direction: "sendonly",
      sendEncodings: [{ scaleResolutionDownBy: 4, maxBitrate: 150_000 }],
    });
    expect(lights[0]?.sender.track?.cloneOf).toBe(main?.sender.track);
    expect(hooks.attaches.map((a) => a.streamIndex)).toEqual([0, 1, 3, 4]);
    expect(hooks.attaches[1]?.sender).toBe(main?.sender);
    expect(hooks.attaches[2]?.sender).toBe(lights[0]?.sender);
    expect(hooks.attaches[3]?.sender).toBe(pc.middles()[0]?.sender);

    // Every sender is sealed before the renegotiation offer is built.
    const before = hooks.events.length;
    await negotiate(pc);
    expect(hooks.events.slice(before)).toEqual(["createOffer", "offer-sent"]);
    expect(hooks.offers).toHaveLength(2);
    expect(hooks.offers[1]).toEqual({
      sdp: "offer-2",
      lightMids: [lights[0]?.mid],
      middleMids: [pc.middles()[0]?.mid],
    });
  });

  it("keeps naming the light mid in every later offer of this client", async () => {
    const { pc } = await connected();
    const mid = pc.lights()[0]?.mid;
    await answer();
    await negotiate(pc);
    await answer();
    await negotiate(pc);
    expect(hooks.offers.map((o) => o.lightMids)).toEqual([[mid], [mid], [mid]]);
  });
});

// ---- The light sender follows the camera ----------------------------------

describe("the light sender follows the camera", () => {
  it("is disabled and enabled with it, and stays one", async () => {
    const { session, pc } = await connected();
    await answer();
    const main = pc.main();
    const light = pc.lights()[0];
    const clone = light?.sender.track;

    session.setCameraEnabled(false);
    expect(main?.sender.track?.enabled).toBe(false);
    expect(clone?.enabled).toBe(false);

    await session.publishVideo();
    expect(main?.sender.track?.enabled).toBe(true);
    expect(clone?.enabled).toBe(true);

    session.setCameraEnabled(false);
    await session.publishVideo();
    expect(pc.lights()).toHaveLength(1);
    expect(pc.lights()[0]).toBe(light);
    expect(pc.everAdded("light")).toHaveLength(1);
    expect(light?.sender.track).toBe(clone);
    // No capture, no new transform.
    expect(captured.filter((t) => t.kind === "video")).toHaveLength(1);
    expect(hooks.attaches.filter((a) => a.streamIndex === 3)).toHaveLength(1);
  });

  it("takes a clone of the new camera when the device is switched, keeping the mute state", async () => {
    const { session, pc } = await connected();
    await answer();
    const light = pc.lights()[0];
    const oldClone = light?.sender.track;
    session.setCameraEnabled(false);

    await session.switchVideoInput("back-camera");
    const newCamera = pc.main()?.sender.track;
    expect(newCamera?.label).toBe("back-camera");
    expect(pc.lights()).toHaveLength(1);
    expect(pc.lights()[0]).toBe(light);
    expect(pc.everAdded("light")).toHaveLength(1);
    expect(light?.sender.track).not.toBe(oldClone);
    expect(light?.sender.track?.cloneOf).toBe(newCamera);
    expect(light?.sender.track?.enabled).toBe(false);
    expect(light?.sender.replaced).toHaveLength(1);
    expect(oldClone?.readyState).toBe("ended");
    // Replaced in place: the same transform, no second one.
    expect(hooks.attaches.filter((a) => a.streamIndex === 3)).toHaveLength(1);
  });

  it("takes a clone of the re-published camera after the camera track ended", async () => {
    const { session, pc } = await connected();
    await answer();
    const main = pc.main();
    const light = pc.lights()[0];
    const oldCamera = main?.sender.track;
    const oldClone = light?.sender.track;
    // The device was unplugged: the camera and its clone both end.
    oldCamera?.stop();
    oldClone?.stop();

    await session.publishVideo();
    const newCamera = main?.sender.track;
    expect(newCamera).not.toBe(oldCamera);
    expect(newCamera?.readyState).toBe("live");
    expect(main?.sender.replaced).toEqual([newCamera]);
    expect(pc.lights()).toHaveLength(1);
    expect(pc.lights()[0]).toBe(light);
    expect(pc.everAdded("light")).toHaveLength(1);
    expect(light?.sender.track?.cloneOf).toBe(newCamera);
    expect(light?.sender.track?.readyState).toBe("live");
    expect(hooks.attaches.filter((a) => a.streamIndex === 3)).toHaveLength(1);
    expect(hooks.attaches.filter((a) => a.streamIndex === 1)).toHaveLength(1);
  });

  it("is stopped on hang-up", async () => {
    const { session, pc } = await connected();
    const clone = pc.lights()[0]?.sender.track;
    expect(clone?.readyState).toBe("live");
    session.hangUp();
    expect(clone?.readyState).toBe("ended");
    expect(pc.main()?.sender.track?.readyState).toBe("ended");
    expect(pc.closed).toBe(true);
  });
});

// ---- Diagnostics ----------------------------------------------------------

describe("the media counters", () => {
  it("log one send line for each camera sender, and a sender line that sums them", async () => {
    vi.useFakeTimers();
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    try {
      const session = new SfuCallSession(config({ nowMs: () => 0 }));
      const connecting = session.connect();
      await vi.advanceTimersByTimeAsync(0);
      await connecting;
      const pc = pcs[0];
      if (pc === undefined) throw new Error("no peer connection");
      pc.stats = new Map<string, Record<string, unknown>>([
        ["OD", { id: "OD", type: "outbound-rtp", kind: "video", frameWidth: 640, frameHeight: 360, framesPerSecond: 30, framesEncoded: 60, packetsSent: 130 }],
        ["OL", { id: "OL", type: "outbound-rtp", kind: "video", frameWidth: 320, frameHeight: 180, framesPerSecond: 30, framesEncoded: 60, packetsSent: 70 }],
        ["OM", { id: "OM", type: "outbound-rtp", kind: "video", scalabilityMode: "L1T3", frameWidth: 1280, frameHeight: 720, framesPerSecond: 30, framesEncoded: 60, packetsSent: 500 }],
      ]);
      await vi.advanceTimersByTimeAsync(2_000);
      const lines = info.mock.calls.map((c) => String(c[0]));
      expect(lines.filter((l) => l.includes(" send video "))).toEqual([
        "call media interval send video mode=L1T3 size=1280x720 fps=30 limited_by=- sender=main needed=- active=yes ceiling=-",
        "call media interval send video mode=- size=640x360 fps=30 limited_by=- sender=middle needed=- active=no ceiling=500",
        "call media interval send video mode=- size=320x180 fps=30 limited_by=- sender=light needed=- active=yes ceiling=150",
      ]);
      const senderLine = lines.find((l) => l.includes(" sender encoded="));
      expect(senderLine).toContain("sender encoded=180 pkts=700");
      expect(lines.some((l) => l.includes("rid="))).toBe(false);
      session.hangUp();
    } finally {
      info.mockRestore();
    }
  });
});

// ---- The middle sender (ADR-0070 step 5) ----------------------------------

/** The `active` of a sender's one encoding, as the browser holds it now. */
function encodingActive(t: FakeTransceiver | undefined): unknown {
  return t?.sender.params.encodings[0]?.["active"];
}

/** The values `active` was changed to by `setParameters` on `t`, in order.
 *  A call that left it as it was (the frame-rate preference) is not one. */
function activeChanges(t: FakeTransceiver | undefined): boolean[] {
  const changes: boolean[] = [];
  let current = (t?.init?.sendEncodings?.[0]?.active ?? true) === true;
  for (const p of t?.sender.setParametersCalls ?? []) {
    const next = ((p as FakeSender["params"]).encodings[0]?.["active"] ?? true) === true;
    if (next !== current) changes.push(next);
    current = next;
  }
  return changes;
}

function offersBuilt(): number {
  return hooks.events.filter((e) => e === "createOffer").length;
}

describe("the middle camera sender", () => {
  it("is a third ordinary sender: half the size, at most 500 kbit/s, no rid, a clone of the camera", async () => {
    const { pc } = await connected();
    const middles = pc.middles();
    expect(middles).toHaveLength(1);
    const middle = middles[0];
    expect(middle?.via).toBe("addTransceiver");
    // Alone in the call: created, and not sending.
    expect(middle?.init).toEqual({
      direction: "sendonly",
      sendEncodings: [{ scaleResolutionDownBy: 2, maxBitrate: 500_000, active: false }],
    });
    expect(middle?.init?.sendEncodings?.[0]).not.toHaveProperty("rid");
    const camera = pc.main()?.sender.track;
    expect(middle?.sender.track).not.toBe(camera);
    expect(middle?.sender.track?.cloneOf).toBe(camera);
    // Not the light sender's clone either: three tracks, three senders.
    expect(middle?.sender.track).not.toBe(pc.lights()[0]?.sender.track);
    await flush();
    // It keeps its frame rate like the other two.
    expect(middle?.sender.params.degradationPreference).toBe("maintain-framerate");
  });

  it("is sealed under stream index 4, its own, before any offer", async () => {
    const { pc } = await connected();
    const attach = hooks.attaches.find((a) => a.streamIndex === 4);
    expect(attach?.sender).toBe(pc.middles()[0]?.sender);
    expect(hooks.attaches.filter((a) => a.streamIndex === 4)).toHaveLength(1);
    // No two senders of this device share an index.
    const indices = hooks.attaches.map((a) => a.streamIndex);
    expect(new Set(indices).size).toBe(indices.length);
    expect(hooks.events.indexOf("attach-4")).toBeLessThan(hooks.events.indexOf("createOffer"));
  });

  it("is named in middle_mids, apart from the light mid, in every offer this client sends", async () => {
    const { pc } = await connected();
    const light = pc.lights()[0]?.mid;
    const middle = pc.middles()[0]?.mid;
    expect(middle).toBeTypeOf("string");
    expect(middle).not.toBe(light);
    expect(middle).not.toBe(pc.main()?.mid);
    await answer();
    await negotiate(pc);
    await answer();
    await negotiate(pc);
    expect(hooks.offers).toHaveLength(3);
    for (const offer of hooks.offers) {
      expect(offer.lightMids).toEqual([light]);
      expect(offer.middleMids).toEqual([middle]);
    }
    // What went on the wire is the frame the SFU reads.
    const sent = sockets[0]?.sent.map((raw) => JSON.parse(raw) as Record<string, unknown>) ?? [];
    const offerFrames = sent.filter((f) => f["kind"] === "sdp-offer");
    expect(offerFrames).toHaveLength(3);
    for (const frame of offerFrames) {
      expect(frame["light_mids"]).toEqual([light]);
      expect(frame["middle_mids"]).toEqual([middle]);
    }
  });

  it("is created sending when three people are already in the call", async () => {
    hooks.welcomeRoster = ["a", "b"];
    const { pc } = await connected();
    expect(pc.middles()[0]?.init?.sendEncodings).toEqual([
      { scaleResolutionDownBy: 2, maxBitrate: 500_000, active: true },
    ]);
    // The light sender never carries the flag: it always sends.
    expect(pc.lights()[0]?.init?.sendEncodings).toEqual([
      { scaleResolutionDownBy: 4, maxBitrate: 150_000 },
    ]);
  });

  it("sends only from three people on, switched with setParameters and no renegotiation", async () => {
    const { pc } = await connected();
    await answer();
    const middle = pc.middles()[0];
    const before = offersBuilt();
    expect(encodingActive(middle)).toBe(false);

    // Two people: still not sent.
    welcome(["a"]);
    await flush();
    expect(encodingActive(middle)).toBe(false);
    expect(activeChanges(middle)).toEqual([]);

    // Three: sent.
    peerJoined("b");
    await flush();
    expect(encodingActive(middle)).toBe(true);
    // Four: nothing to change.
    peerJoined("c");
    await flush();
    expect(activeChanges(middle)).toEqual([true]);

    // Back to three, then to two: stopped at two.
    peerLeft("c");
    await flush();
    expect(encodingActive(middle)).toBe(true);
    peerLeft("b");
    await flush();
    expect(encodingActive(middle)).toBe(false);
    expect(activeChanges(middle)).toEqual([true, false]);

    // And on again when a third person comes back.
    peerJoined("d");
    await flush();
    expect(encodingActive(middle)).toBe(true);

    // The size and the ceiling are untouched, the media line is the same,
    // and no offer was built for any of it.
    expect(middle?.sender.params.encodings[0]).toMatchObject({
      scaleResolutionDownBy: 2,
      maxBitrate: 500_000,
    });
    expect(pc.middles()).toEqual([middle]);
    expect(offersBuilt()).toBe(before);
    expect(hooks.offers).toHaveLength(1);
    // The light sender is never switched.
    expect(activeChanges(pc.lights()[0])).toEqual([]);
  });

  it("does not count this device twice when the SFU's roster names it", async () => {
    const { pc } = await connected();
    await answer();
    welcome(["self", "a"]);
    await flush();
    expect(encodingActive(pc.middles()[0])).toBe(false);
  });

  it("asks again when the browser refused to start it, and says so once", async () => {
    vi.useFakeTimers();
    try {
      const session = new SfuCallSession(config({ nowMs: () => Date.now() }));
      const connecting = session.connect();
      await vi.advanceTimersByTimeAsync(0);
      await connecting;
      const pc = pcs[0];
      if (pc === undefined) throw new Error("no peer connection");
      const middle = pc.middles()[0];
      browser.refusesActiveChange = true;
      welcome(["a", "b"]);
      await vi.advanceTimersByTimeAsync(0);
      expect(encodingActive(middle)).toBe(false);
      const refusals = (): unknown[][] =>
        warn.mock.calls.filter((c: unknown[]) => String(c[0]).includes("middle camera sender could not be started"));
      expect(refusals()).toHaveLength(1);

      // The next look (the counters' tick) asks again, and it is taken.
      browser.refusesActiveChange = false;
      await vi.advanceTimersByTimeAsync(2_000);
      expect(encodingActive(middle)).toBe(true);
      expect(refusals()).toHaveLength(1);
      // The call was never at risk.
      expect(session.getState()).toBe("connecting");
      session.hangUp();
    } finally {
      vi.useRealTimers();
    }
  });

  it("first fallback: added plainly, then given the same values and the same flag with setParameters", async () => {
    browser.refusesLightEncodings = true;
    hooks.welcomeRoster = ["a", "b"];
    const { pc } = await connected();
    const middles = pc.middles();
    expect(middles).toHaveLength(1);
    expect(middles[0]?.init).toEqual({ direction: "sendonly" });
    expect(middles[0]?.sender.setParametersCalls).toEqual([
      {
        encodings: [{ scaleResolutionDownBy: 2, maxBitrate: 500_000, active: true }],
        degradationPreference: "maintain-framerate",
      },
    ]);
    expect(hooks.events.indexOf("attach-4")).toBeLessThan(hooks.events.indexOf("createOffer"));
    expect(hooks.offers[0]?.middleMids).toEqual([middles[0]?.mid]);
    expect(copyWarnings(warn, "middle")).toHaveLength(0);
  });

  it("second fallback: with setParameters refused too, it is dropped with one warning and the call goes on", async () => {
    browser.refusesLightEncodings = true;
    browser.refusesLightSetParameters = true;
    const { session, pc } = await connected();
    expect(pc.middles()).toHaveLength(0);
    const dropped = pc.everAdded("middle")[0];
    expect(dropped?.stopped).toBe(true);
    expect(pc.removed).toContain(dropped?.sender);
    // Its clone is stopped: the camera is not sent again at full size.
    expect(clones.every((c) => c.readyState === "ended")).toBe(true);
    expect(hooks.offers[0]?.middleMids).toEqual([]);
    expect(copyWarnings(warn, "middle")).toHaveLength(1);
    expect(session.getState()).toBe("connecting");

    // A change of the roster does not bring it back, nor warn again.
    await answer();
    welcome(["a", "b"]);
    await flush();
    expect(pc.everAdded("middle")).toHaveLength(1);
    expect(copyWarnings(warn, "middle")).toHaveLength(1);
  });

  it("a failed SFrame attach removes it, and it alone (invariant #4)", async () => {
    hooks.attachFailsFor = 4;
    const { session, pc } = await connected();
    expect(pc.middles()).toHaveLength(0);
    const dropped = pc.everAdded("middle")[0];
    expect(dropped?.stopped).toBe(true);
    expect(pc.removed).toContain(dropped?.sender);
    expect(dropped?.sender.track).toBeNull();
    // The light sender is in place and named; the middle one is in no offer.
    expect(pc.lights()).toHaveLength(1);
    expect(hooks.offers).toEqual([
      { sdp: "offer-1", lightMids: [pc.lights()[0]?.mid], middleMids: [] },
    ]);
    expect(hooks.attaches.map((a) => a.streamIndex).sort()).toEqual([0, 1, 3]);
    expect(copyWarnings(warn, "middle")).toHaveLength(1);
    expect(lightWarnings(warn)).toHaveLength(0);
    expect(session.getState()).toBe("connecting");
    expect(pc.closed).toBe(false);
  });

  it("follows the camera off and on, and there is never a second one", async () => {
    const { session, pc } = await connected();
    await answer();
    const middle = pc.middles()[0];
    const clone = middle?.sender.track;

    session.setCameraEnabled(false);
    expect(clone?.enabled).toBe(false);
    await session.publishVideo();
    expect(clone?.enabled).toBe(true);
    session.setCameraEnabled(false);
    await session.publishVideo();

    expect(pc.everAdded("middle")).toEqual([middle]);
    expect(middle?.sender.track).toBe(clone);
    expect(hooks.attaches.filter((a) => a.streamIndex === 4)).toHaveLength(1);
  });

  it("takes a clone of the new camera on a device switch, in place, keeping the mute state and the flag", async () => {
    hooks.welcomeRoster = ["a", "b"];
    const { session, pc } = await connected();
    await answer();
    const middle = pc.middles()[0];
    const oldClone = middle?.sender.track;
    session.setCameraEnabled(false);

    await session.switchVideoInput("back-camera");
    const newCamera = pc.main()?.sender.track;
    expect(pc.everAdded("middle")).toEqual([middle]);
    expect(middle?.sender.track?.cloneOf).toBe(newCamera);
    expect(middle?.sender.track?.enabled).toBe(false);
    expect(middle?.sender.replaced).toHaveLength(1);
    expect(oldClone?.readyState).toBe("ended");
    expect(encodingActive(middle)).toBe(true);
    // Same sender, same transform: no second seal, no renegotiation.
    expect(hooks.attaches.filter((a) => a.streamIndex === 4)).toHaveLength(1);
    expect(hooks.offers).toHaveLength(1);
  });

  it("takes a clone of the re-published camera after the camera track ended", async () => {
    const { session, pc } = await connected();
    await answer();
    const middle = pc.middles()[0];
    pc.main()?.sender.track?.stop();
    for (const c of clones) c.stop();

    await session.publishVideo();
    const newCamera = pc.main()?.sender.track;
    expect(pc.everAdded("middle")).toEqual([middle]);
    expect(middle?.sender.track?.cloneOf).toBe(newCamera);
    expect(middle?.sender.track?.readyState).toBe("live");
    expect(hooks.attaches.filter((a) => a.streamIndex === 4)).toHaveLength(1);
  });

  it("is added with the camera when the camera is turned on mid-call", async () => {
    hooks.welcomeRoster = ["a", "b"];
    const { session, pc } = await connected({ media: { audio: true, video: false } });
    expect(pc.middles()).toHaveLength(0);
    await answer();
    await session.publishVideo();
    const middle = pc.middles()[0];
    expect(middle?.init?.sendEncodings).toEqual([
      { scaleResolutionDownBy: 2, maxBitrate: 500_000, active: true },
    ]);
    await negotiate(pc);
    expect(hooks.offers[1]?.middleMids).toEqual([middle?.mid]);
  });

  it("is stopped on hang-up", async () => {
    const { session, pc } = await connected();
    const clone = pc.middles()[0]?.sender.track;
    expect(clone?.readyState).toBe("live");
    session.hangUp();
    expect(clone?.readyState).toBe("ended");
  });
});

// ---- What this device asks the SFU for ------------------------------------

/** A session connected under fake timers, with `others` in the call. */
async function connectedOnFakeClock(others: string[]): Promise<{ session: SfuCallSession; pc: FakePc }> {
  vi.useFakeTimers();
  hooks.welcomeRoster = others;
  const session = new SfuCallSession(config({ nowMs: () => Date.now() }));
  const connecting = session.connect();
  await vi.advanceTimersByTimeAsync(0);
  await connecting;
  const pc = pcs[0];
  if (pc === undefined) throw new Error("no peer connection");
  return { session, pc };
}

/** The `video-wants` frames that went on the wire, as the SFU reads them. */
function wantsOnTheWire(): Array<Record<string, string>> {
  return (sockets[0]?.sent ?? [])
    .map((raw) => JSON.parse(raw) as { kind?: string; wants?: Array<{ peer: string; quality: string }> })
    .filter((f) => f.kind === "video-wants")
    .map((f) => Object.fromEntries((f.wants ?? []).map((w) => [w.peer, w.quality])));
}

describe("the video wants", () => {
  it("are sent once the publish offer is out, never before it: light for everyone no tile shows", async () => {
    await connectedOnFakeClock(["a", "b"]);
    await vi.advanceTimersByTimeAsync(0);
    expect(hooks.wants).toEqual([{ peers: ["a", "b"], qualities: ["light", "light"] }]);
    expect(hooks.events.indexOf("wants-sent")).toBeGreaterThan(hooks.events.indexOf("offer-sent"));
    expect(wantsOnTheWire()).toEqual([{ a: "light", b: "light" }]);
  });

  it("are not sent with nobody else in the call", async () => {
    await connectedOnFakeClock([]);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(hooks.wants).toEqual([]);
  });

  it("carry the highest quality any mounted tile of a person wants", async () => {
    const { session } = await connectedOnFakeClock(["a", "b", "c"]);
    await vi.advanceTimersByTimeAsync(0);
    hooks.wants = [];

    const strip = {};
    const stage = {};
    const grid = {};
    session.reportTileVideoQuality(strip, "a", "light");
    session.reportTileVideoQuality(stage, "a", "main");
    session.reportTileVideoQuality(grid, "b", "middle");
    await vi.advanceTimersByTimeAsync(200);
    expect(hooks.wants).toEqual([{ peers: ["a", "b", "c"], qualities: ["main", "middle", "light"] }]);
    expect(session.videoQualityOf("a")).toBe("main");
    expect(session.videoQualityOf("c")).toBe("light");

    // The stage tile goes: what is left of `a` is its strip tile.
    session.reportTileVideoQuality(stage, "a", null);
    await vi.advanceTimersByTimeAsync(200);
    expect(hooks.wants.at(-1)).toEqual({ peers: ["a", "b", "c"], qualities: ["light", "middle", "light"] });

    // A tile changes its mind.
    session.reportTileVideoQuality(strip, "a", "middle");
    await vi.advanceTimersByTimeAsync(200);
    expect(hooks.wants.at(-1)).toEqual({ peers: ["a", "b", "c"], qualities: ["middle", "middle", "light"] });
    expect(hooks.wants).toHaveLength(3);
  });

  it("are coalesced: the reports of one moment are one frame, and frames are at least 200 ms apart", async () => {
    const { session } = await connectedOnFakeClock(["a", "b"]);
    await vi.advanceTimersByTimeAsync(0);
    expect(hooks.wants).toHaveLength(1);

    const ta = {};
    const tb = {};
    session.reportTileVideoQuality(ta, "a", "middle");
    session.reportTileVideoQuality(tb, "b", "middle");
    // Inside 200 ms of the last frame: held.
    await vi.advanceTimersByTimeAsync(199);
    expect(hooks.wants).toHaveLength(1);
    session.reportTileVideoQuality(ta, "a", "main");
    await vi.advanceTimersByTimeAsync(1);
    expect(hooks.wants).toHaveLength(2);
    expect(hooks.wants[1]).toEqual({ peers: ["a", "b"], qualities: ["main", "middle"] });

    // A run of changes after that is again one frame, 200 ms after the last.
    for (const q of ["light", "middle", "main", "middle"] as const) {
      session.reportTileVideoQuality(tb, "b", q);
      await vi.advanceTimersByTimeAsync(40);
    }
    expect(hooks.wants).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(40);
    // `b` ended where it began: the set is the one last sent, so nothing goes.
    expect(hooks.wants).toHaveLength(2);

    session.reportTileVideoQuality(tb, "b", "light");
    await vi.advanceTimersByTimeAsync(200);
    expect(hooks.wants).toHaveLength(3);
    expect(hooks.wants[2]).toEqual({ peers: ["a", "b"], qualities: ["main", "light"] });
  });

  it("are not sent again for a report that changes nothing", async () => {
    const { session } = await connectedOnFakeClock(["a"]);
    await vi.advanceTimersByTimeAsync(0);
    const tile = {};
    session.reportTileVideoQuality(tile, "a", "main");
    await vi.advanceTimersByTimeAsync(200);
    const sent = hooks.wants.length;
    session.reportTileVideoQuality(tile, "a", "main");
    session.reportTileVideoQuality({}, "a", "light");
    await vi.advanceTimersByTimeAsync(1_000);
    expect(hooks.wants).toHaveLength(sent);
  });

  it("are sent again, whole, after a person joins, even when nothing else changed", async () => {
    const { session } = await connectedOnFakeClock(["a"]);
    await vi.advanceTimersByTimeAsync(0);
    session.reportTileVideoQuality({}, "a", "main");
    await vi.advanceTimersByTimeAsync(200);
    const sent = hooks.wants.length;

    peerJoined("b");
    await vi.advanceTimersByTimeAsync(200);
    expect(hooks.wants).toHaveLength(sent + 1);
    expect(hooks.wants.at(-1)).toEqual({ peers: ["a", "b"], qualities: ["main", "light"] });
  });

  it("drop a person who left", async () => {
    const { session } = await connectedOnFakeClock(["a", "b"]);
    await vi.advanceTimersByTimeAsync(0);
    const ta = {};
    session.reportTileVideoQuality(ta, "a", "main");
    session.reportTileVideoQuality({}, "b", "middle");
    await vi.advanceTimersByTimeAsync(200);

    peerLeft("a");
    await vi.advanceTimersByTimeAsync(200);
    expect(hooks.wants.at(-1)).toEqual({ peers: ["b"], qualities: ["middle"] });
    // A tile of the person that is still mounted for a moment names no one.
    session.reportTileVideoQuality(ta, "a", "middle");
    await vi.advanceTimersByTimeAsync(200);
    expect(hooks.wants.at(-1)).toEqual({ peers: ["b"], qualities: ["middle"] });
  });

  it("are said again when media connects", async () => {
    const { pc } = await connectedOnFakeClock(["a"]);
    await vi.advanceTimersByTimeAsync(0);
    expect(hooks.wants).toHaveLength(1);
    pc.iceConnectionState = "connected";
    (pc.oniceconnectionstatechange as (() => void) | null)?.();
    await vi.advanceTimersByTimeAsync(200);
    expect(hooks.wants).toHaveLength(2);
    expect(hooks.wants[1]).toEqual(hooks.wants[0]);
  });

  it("stop with the call", async () => {
    const { session } = await connectedOnFakeClock(["a"]);
    await vi.advanceTimersByTimeAsync(0);
    const tile = {};
    session.reportTileVideoQuality(tile, "a", "main");
    session.hangUp();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(hooks.wants).toHaveLength(1);
    // A tile that unmounts after the call ended changes nothing.
    session.reportTileVideoQuality(tile, "a", null);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(hooks.wants).toHaveLength(1);
  });
});

// ---- What arrives ---------------------------------------------------------

describe("a remote middle track", () => {
  it("is decrypted and registered like any track, and named layer=middle in the counters", async () => {
    vi.useFakeTimers();
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    try {
      const remote: Array<{ streamId: string }> = [];
      const session = new SfuCallSession(config({ nowMs: () => Date.now() }), {
        onRemoteTrack: (rt) => remote.push({ streamId: rt.streamId }),
      });
      const connecting = session.connect();
      await vi.advanceTimersByTimeAsync(0);
      await connecting;
      const pc = pcs[0];
      if (pc === undefined) throw new Error("no peer connection");
      const arrived: Array<{ track: object; receiver: object }> = [];
      const arrive = (streamId: string, trackId: string): void => {
        const track = { kind: "video", id: trackId, addEventListener(): void {} };
        const receiver = { getStats: async () => new Map() };
        arrived.push({ track, receiver });
        (pc.ontrack as (ev: unknown) => void)({ track, receiver, streams: [{ id: streamId }] });
      };
      arrive("peerX", "t-main");
      arrive("peerX~mid", "t-mid");
      arrive("peerX~light", "t-light");
      // Every one of the three is handed on under its own stream id,
      expect(remote.map((r) => r.streamId)).toEqual(["peerX", "peerX~mid", "peerX~light"]);
      // has the decrypting transform on its receiver,
      expect(hooks.receiverAttaches).toEqual(arrived.map((a) => a.receiver));
      // and is registered for the frame watch.
      for (const a of arrived) expect(videoFrameSourceOf(a.track)).toBe(a.receiver);

      const inbound = (id: string, trackIdentifier: string): [string, Record<string, unknown>] => [
        id,
        { id, type: "inbound-rtp", kind: "video", trackIdentifier, packetsReceived: 10 },
      ];
      pc.stats = new Map([inbound("I1", "t-main"), inbound("I2", "t-mid"), inbound("I3", "t-light")]);
      await vi.advanceTimersByTimeAsync(2_000);
      const lines = info.mock.calls.map((c) => String(c[0])).filter((l) => l.includes(" video#"));
      expect(lines.map((l) => l.slice(l.lastIndexOf("layer=")))).toEqual([
        "layer=main",
        "layer=middle",
        "layer=light",
      ]);
      session.hangUp();
    } finally {
      info.mockRestore();
    }
  });

  it("puts want= on the shown line: what this device asks for of that person", async () => {
    vi.useFakeTimers();
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    try {
      hooks.welcomeRoster = ["a", "b"];
      const session = new SfuCallSession(config({ nowMs: () => Date.now() }));
      const connecting = session.connect();
      await vi.advanceTimersByTimeAsync(0);
      await connecting;
      session.reportTileVideoQuality({}, "a", "middle");
      const counted = { lightSwitches: 1, lightMs: 400, stallMaxMs: 180, stallTotalMs: 180 };
      shownLayerLedger.add("a", counted);
      shownLayerLedger.add("b", counted);
      await vi.advanceTimersByTimeAsync(2_000);
      const lines = info.mock.calls.map((c) => String(c[0])).filter((l) => l.includes(" shown "));
      expect(lines).toEqual([
        "call media interval shown peer=a want=middle given=- light_switches=1 light_ms=400 stall_max_ms=180 stall_total_ms=180",
        "call media interval shown peer=b want=light given=- light_switches=1 light_ms=400 stall_max_ms=180 stall_total_ms=180",
      ]);
      session.hangUp();
    } finally {
      info.mockRestore();
    }
  });
});

// ---- What the SFU gives (ADR-0070 step 6) ---------------------------------

/** The SFU says what it gives. */
function videoGiven(given: unknown): void {
  sockets[0]?.onmessage?.({ data: JSON.stringify({ kind: "video-given", given }) });
}

describe("the video the SFU gives", () => {
  let info: ReturnType<typeof vi.spyOn>;
  beforeEach(() => {
    info = vi.spyOn(console, "info").mockImplementation(() => {});
  });
  afterEach(() => {
    info.mockRestore();
  });
  const givenLines = (): string[] =>
    info.mock.calls.map((c: unknown[]) => String(c[0])).filter((l: string) => l.startsWith("call video given"));

  /** A session on the fake clock that records every set it hands the tiles. */
  async function withGiven(others: string[]): Promise<{
    session: SfuCallSession;
    seen: Array<Record<string, string>>;
  }> {
    vi.useFakeTimers();
    hooks.welcomeRoster = others;
    const seen: Array<Record<string, string>> = [];
    const session = new SfuCallSession(config({ nowMs: () => Date.now() }), {
      onVideoGiven: (given) => {
        seen.push(Object.fromEntries(given));
      },
    });
    const connecting = session.connect();
    await vi.advanceTimersByTimeAsync(0);
    await connecting;
    await vi.advanceTimersByTimeAsync(0);
    return { session, seen };
  }

  it("is nothing until the SFU says something: every tile shows what it wants", async () => {
    const { session, seen } = await withGiven(["a"]);
    expect(session.videoGivenOf("a")).toBeUndefined();
    expect(seen).toEqual([]);
    expect(givenLines()).toEqual([]);
  });

  it("reaches the tiles as the whole set, and one line names whose shown picture changed", async () => {
    const { session, seen } = await withGiven(["a", "b", "c"]);
    session.reportTileVideoQuality({}, "a", "main");
    session.reportTileVideoQuality({}, "b", "middle");
    await vi.advanceTimersByTimeAsync(200);

    videoGiven([
      { peer: "a", quality: "middle" },
      { peer: "b", quality: "middle" },
      { peer: "c", quality: "light" },
    ]);
    expect(seen).toEqual([{ a: "middle", b: "middle", c: "light" }]);
    expect(session.videoGivenOf("a")).toBe("middle");
    expect(givenLines()).toEqual(["call video given peer=a want=main given=middle"]);

    // A frame that changes no shown picture is taken and says nothing.
    videoGiven([
      { peer: "a", quality: "middle" },
      { peer: "b", quality: "main" },
      { peer: "c", quality: "main" },
    ]);
    expect(seen).toHaveLength(2);
    expect(givenLines()).toHaveLength(1);

    // The picture is given back.
    videoGiven([
      { peer: "a", quality: "main" },
      { peer: "b", quality: "light" },
    ]);
    expect(seen.at(-1)).toEqual({ a: "main", b: "light" });
    expect(session.videoGivenOf("c")).toBeUndefined();
    expect(givenLines().at(-1)).toBe(
      "call video given peer=a want=main given=main | peer=b want=middle given=light",
    );
  });

  it("does not change what this device asks for, and sends nothing for it", async () => {
    const { session } = await withGiven(["a", "b"]);
    session.reportTileVideoQuality({}, "a", "main");
    session.reportTileVideoQuality({}, "b", "middle");
    await vi.advanceTimersByTimeAsync(200);
    const sent = hooks.wants.length;
    expect(hooks.wants.at(-1)).toEqual({ peers: ["a", "b"], qualities: ["main", "middle"] });

    videoGiven([
      { peer: "a", quality: "light" },
      { peer: "b", quality: "light" },
    ]);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(hooks.wants).toHaveLength(sent);
    expect(session.videoQualityOf("a")).toBe("main");
    expect(session.videoQualityOf("b")).toBe("middle");

    // A later frame of wants still carries the wish, not what was given.
    peerJoined("c");
    await vi.advanceTimersByTimeAsync(200);
    expect(hooks.wants.at(-1)).toEqual({ peers: ["a", "b", "c"], qualities: ["main", "middle", "light"] });
    expect(wantsOnTheWire().at(-1)).toEqual({ a: "main", b: "middle", c: "light" });
  });

  it("forgets a person who left", async () => {
    const { session, seen } = await withGiven(["a", "b"]);
    videoGiven([
      { peer: "a", quality: "light" },
      { peer: "b", quality: "middle" },
    ]);
    peerLeft("a");
    expect(session.videoGivenOf("a")).toBeUndefined();
    expect(session.videoGivenOf("b")).toBe("middle");
    expect(seen.at(-1)).toEqual({ b: "middle" });
    // Someone the set never named leaves: the tiles are not told again.
    const told = seen.length;
    peerJoined("c");
    peerLeft("c");
    expect(seen).toHaveLength(told);
  });

  it("ignores a malformed frame with one warning, and keeps the set it had", async () => {
    const { session, seen } = await withGiven(["a"]);
    videoGiven([{ peer: "a", quality: "middle" }]);
    const warned = (): unknown[][] =>
      warn.mock.calls.filter((c: unknown[]) => String(c[0]).includes("video-given"));
    expect(warned()).toHaveLength(0);

    videoGiven([{ peer: "a", quality: "full" }]);
    expect(warned()).toHaveLength(1);
    videoGiven("main");
    expect(warned()).toHaveLength(2);
    videoGiven([{ peer: "", quality: "main" }]);
    expect(warned()).toHaveLength(3);

    expect(seen).toEqual([{ a: "middle" }]);
    expect(session.videoGivenOf("a")).toBe("middle");
  });

  it("is in the shown line beside want=, and is emptied when the call ends", async () => {
    const { session, seen } = await withGiven(["a", "b"]);
    session.reportTileVideoQuality({}, "a", "main");
    videoGiven([{ peer: "a", quality: "middle" }]);
    const counted = { lightSwitches: 0, lightMs: 0, stallMaxMs: 0, stallTotalMs: 0 };
    shownLayerLedger.add("a", counted);
    shownLayerLedger.add("b", counted);
    await vi.advanceTimersByTimeAsync(2_000);
    const lines = info.mock.calls
      .map((c: unknown[]) => String(c[0]))
      .filter((l: string) => l.startsWith("call media interval shown"));
    expect(lines).toEqual([
      "call media interval shown peer=a want=main given=middle light_switches=0 light_ms=0 stall_max_ms=0 stall_total_ms=0",
      "call media interval shown peer=b want=light given=- light_switches=0 light_ms=0 stall_max_ms=0 stall_total_ms=0",
    ]);
    session.hangUp();
    expect(session.videoGivenOf("a")).toBeUndefined();
    expect(seen.at(-1)).toEqual({});
  });
});

// ---- Senders nobody watches (`video-needed`) -------------------------------

/** The SFU says which of this device's camera senders somebody is sent. */
function videoNeeded(main: boolean, middle: boolean): void {
  sockets[0]?.onmessage?.({ data: JSON.stringify({ kind: "video-needed", main, middle }) });
}

describe("senders nobody watches", () => {
  /** A call of three (so the middle sender sends), published and answered. */
  async function inCallOfThree(): Promise<{ session: SfuCallSession; pc: FakePc }> {
    hooks.welcomeRoster = ["a", "b"];
    const call = await connected();
    await answer();
    return call;
  }

  it("leaves every sender sending until the SFU says otherwise", async () => {
    const { pc } = await inCallOfThree();
    expect(encodingActive(pc.main()) ?? true).toBe(true);
    expect(encodingActive(pc.middles()[0])).toBe(true);
    expect(encodingActive(pc.lights()[0]) ?? true).toBe(true);
    // The first frame says true for everything: nothing to change.
    videoNeeded(true, true);
    await flush();
    expect(activeChanges(pc.main())).toEqual([]);
    expect(activeChanges(pc.lights()[0])).toEqual([]);
  });

  it("stops the main sender on false and starts it at once on true, with setParameters alone", async () => {
    const { pc } = await inCallOfThree();
    const main = pc.main();
    const transceivers = [...pc.getTransceivers()];
    const offers = hooks.offers.length;
    const seals = hooks.attaches.length;

    videoNeeded(false, true);
    await flush();
    expect(encodingActive(main)).toBe(false);
    expect(encodingActive(pc.middles()[0])).toBe(true);

    videoNeeded(true, true);
    await flush();
    expect(encodingActive(main)).toBe(true);
    expect(activeChanges(main)).toEqual([false, true]);

    // Same transceivers, same tracks, no offer, no new seal.
    expect(pc.getTransceivers()).toEqual(transceivers);
    expect(pc.removed).toEqual([]);
    expect(main?.sender.replaced).toEqual([]);
    expect(main?.sender.track?.enabled).toBe(true);
    expect(main?.sender.params.encodings[0]?.["scalabilityMode"]).toBe("L1T3");
    expect(hooks.offers).toHaveLength(offers);
    expect(hooks.attaches).toHaveLength(seals);
  });

  it("stops and starts the middle sender by the SFU's word, and never the light one", async () => {
    const { pc } = await inCallOfThree();
    videoNeeded(false, false);
    await flush();
    expect(encodingActive(pc.main())).toBe(false);
    expect(encodingActive(pc.middles()[0])).toBe(false);
    expect(encodingActive(pc.lights()[0]) ?? true).toBe(true);
    videoNeeded(false, true);
    await flush();
    expect(encodingActive(pc.middles()[0])).toBe(true);
    expect(encodingActive(pc.main())).toBe(false);
    expect(activeChanges(pc.lights()[0])).toEqual([]);
  });

  it("ends at the last word under a rapid true, false, true", async () => {
    const { pc } = await inCallOfThree();
    videoNeeded(false, true);
    videoNeeded(true, true);
    videoNeeded(false, true);
    videoNeeded(true, true);
    await flush();
    expect(encodingActive(pc.main()) ?? true).toBe(true);
    videoNeeded(true, true);
    videoNeeded(false, true);
    await flush();
    expect(encodingActive(pc.main())).toBe(false);
  });

  it("keeps the people count over the SFU's word for the middle sender", async () => {
    const { pc } = await connected();
    await answer();
    welcome(["a"]);
    await flush();
    // Two people: not sent, whatever the SFU says.
    videoNeeded(true, true);
    await flush();
    expect(encodingActive(pc.middles()[0])).toBe(false);
    // Three people, and the SFU's last word for it is false: still not sent.
    videoNeeded(true, false);
    peerJoined("b");
    await flush();
    expect(encodingActive(pc.middles()[0])).toBe(false);
    // Needed: sent.
    videoNeeded(true, true);
    await flush();
    expect(encodingActive(pc.middles()[0])).toBe(true);
    // Back to two people: stopped though it is needed.
    peerLeft("b");
    await flush();
    expect(encodingActive(pc.middles()[0])).toBe(false);
  });

  it("leaves the person's camera toggle as it was: the tracks, not the encodings", async () => {
    const { session, pc } = await inCallOfThree();
    session.setCameraEnabled(false);
    await flush();
    expect(pc.main()?.sender.track?.enabled).toBe(false);
    expect(pc.lights()[0]?.sender.track?.enabled).toBe(false);
    expect(activeChanges(pc.main())).toEqual([]);
    expect(activeChanges(pc.middles()[0])).toEqual([]);
    expect(activeChanges(pc.lights()[0])).toEqual([]);
  });

  it("finds the main encoding still off when the camera is turned on after the SFU said false", async () => {
    const { session, pc } = await inCallOfThree();
    session.setCameraEnabled(false);
    videoNeeded(false, true);
    await flush();
    expect(encodingActive(pc.main())).toBe(false);

    await session.publishVideo();
    await flush();
    expect(pc.main()?.sender.track?.enabled).toBe(true);
    expect(encodingActive(pc.main())).toBe(false);
    expect(encodingActive(pc.lights()[0]) ?? true).toBe(true);
    expect(pc.lights()[0]?.sender.track?.enabled).toBe(true);

    // And it starts the moment somebody wants it.
    videoNeeded(true, true);
    await flush();
    expect(encodingActive(pc.main())).toBe(true);
  });

  it("puts the flag back after a device switch", async () => {
    const { session, pc } = await inCallOfThree();
    videoNeeded(false, false);
    await flush();
    const main = pc.main();
    const middle = pc.middles()[0];
    // A browser that reset the parameters when the track was replaced.
    if (main !== undefined) main.sender.params = { encodings: [{}] };
    if (middle !== undefined) middle.sender.params = { encodings: [{}] };
    await session.switchVideoInput("back-camera");
    await flush();
    expect(pc.main()).toBe(main);
    expect(encodingActive(main)).toBe(false);
    expect(encodingActive(middle)).toBe(false);
    expect(encodingActive(pc.lights()[0]) ?? true).toBe(true);
  });

  it("puts the flag back after a renegotiation that reset it", async () => {
    const { pc } = await inCallOfThree();
    videoNeeded(false, true);
    await flush();
    const main = pc.main();
    if (main !== undefined) main.sender.params = { encodings: [{}] };
    await negotiate(pc);
    await answer();
    expect(encodingActive(main)).toBe(false);
  });

  it("keeps the flag over a re-publish after the camera track ended", async () => {
    const { session, pc } = await inCallOfThree();
    videoNeeded(false, true);
    await flush();
    const main = pc.main();
    const sender = main?.sender;
    sender?.track?.stop();
    await session.publishVideo();
    await flush();
    // The same sender took the new camera in place.
    expect(sender?.replaced).toHaveLength(1);
    expect(sender?.track?.readyState).toBe("live");
    expect(sender?.params.encodings[0]?.["active"]).toBe(false);
    expect(encodingActive(pc.lights()[0]) ?? true).toBe(true);
    videoNeeded(true, true);
    await flush();
    expect(sender?.params.encodings[0]?.["active"]).toBe(true);
  });

  it("gives a camera turned on mid-call the SFU's last word", async () => {
    const { session, pc } = await connected({ media: { audio: true, video: false } });
    await answer();
    welcome(["a", "b"]);
    await flush();
    // (An SFU tells only a publisher that declared a light sender; a word
    // held from before the camera is applied to the senders when they come.)
    videoNeeded(false, false);
    await session.publishVideo();
    await flush();
    expect(encodingActive(pc.main())).toBe(false);
    expect(encodingActive(pc.middles()[0])).toBe(false);
    expect(encodingActive(pc.lights()[0]) ?? true).toBe(true);
  });

  it("leaves a sender sending when the browser refuses to stop it, and says so once", async () => {
    const { pc } = await inCallOfThree();
    browser.refusesActiveChange = true;
    const refusals = (): unknown[][] =>
      warn.mock.calls.filter((c: unknown[]) => String(c[0]).includes("main camera sender could not be stopped"));
    videoNeeded(false, true);
    await flush();
    expect(encodingActive(pc.main()) ?? true).toBe(true);
    expect(refusals()).toHaveLength(1);
    videoNeeded(false, true);
    videoNeeded(false, false);
    await flush();
    expect(encodingActive(pc.main()) ?? true).toBe(true);
    expect(refusals()).toHaveLength(1);
  });

  it("ignores a malformed frame with one warning and keeps the last good word", async () => {
    const { pc } = await inCallOfThree();
    videoNeeded(false, true);
    await flush();
    sockets[0]?.onmessage?.({ data: JSON.stringify({ kind: "video-needed", main: true }) });
    await flush();
    expect(encodingActive(pc.main())).toBe(false);
    expect(
      warn.mock.calls.filter((c: unknown[]) => String(c[0]).includes("malformed video-needed frame")),
    ).toHaveLength(1);
  });

  it("touches no sender once the call has ended", async () => {
    const { session, pc } = await inCallOfThree();
    videoNeeded(false, true);
    await flush();
    const main = pc.main();
    const calls = main?.sender.setParametersCalls.length;
    const socket = sockets[0];
    session.hangUp();
    // A frame that was already on its way.
    expect(() =>
      socket?.onmessage?.({ data: JSON.stringify({ kind: "video-needed", main: true, middle: true }) }),
    ).not.toThrow();
    await flush();
    expect(main?.sender.setParametersCalls).toHaveLength(calls ?? -1);
  });

  it("says one line when the word changes what is active, and names it in the send lines", async () => {
    vi.useFakeTimers();
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    try {
      hooks.welcomeRoster = ["a", "b"];
      const session = new SfuCallSession(config({ nowMs: () => 0 }));
      const connecting = session.connect();
      await vi.advanceTimersByTimeAsync(0);
      await connecting;
      const pc = pcs[0];
      if (pc === undefined) throw new Error("no peer connection");
      const needLines = (): string[] =>
        info.mock.calls.map((c) => String(c[0])).filter((l) => l.startsWith("call video needed"));

      videoNeeded(true, true);
      expect(needLines()).toEqual([]);
      videoNeeded(false, true);
      videoNeeded(false, true);
      expect(needLines()).toEqual([
        "call video needed main=no middle=yes active main=no middle=yes light=yes",
      ]);

      // The browser reports the light and the middle sender only: the main
      // one, switched off, still has its line.
      pc.stats = new Map<string, Record<string, unknown>>([
        ["OD", { id: "OD", type: "outbound-rtp", kind: "video", frameWidth: 640, frameHeight: 360, framesPerSecond: 30 }],
        ["OL", { id: "OL", type: "outbound-rtp", kind: "video", frameWidth: 320, frameHeight: 180, framesPerSecond: 30 }],
      ]);
      await vi.advanceTimersByTimeAsync(2_000);
      const lines = info.mock.calls.map((c) => String(c[0])).filter((l) => l.includes(" send video "));
      expect(lines).toEqual([
        "call media interval send video mode=- size=640x360 fps=30 limited_by=- sender=middle needed=yes active=yes ceiling=500",
        "call media interval send video mode=- size=320x180 fps=30 limited_by=- sender=light needed=- active=yes ceiling=150",
        "call media interval send video mode=- size=- fps=- limited_by=- sender=main needed=no active=no ceiling=-",
      ]);
      session.hangUp();
    } finally {
      info.mockRestore();
      vi.useRealTimers();
    }
  });
});

// ---- The publish budget (`publish-budget`) ---------------------------------

/** The SFU says how much this device's main and middle sender may send. */
function publishBudget(mainKbps: number, middleKbps: number): void {
  sockets[0]?.onmessage?.({
    data: JSON.stringify({ kind: "publish-budget", main_kbps: mainKbps, middle_kbps: middleKbps }),
  });
}

/** The ceiling a sender's one encoding holds now, bits a second. */
function encodingCeiling(t: FakeTransceiver | undefined): unknown {
  return t?.sender.params.encodings[0]?.["maxBitrate"];
}

describe("the publish budget", () => {
  async function inCallOfThree(): Promise<{ session: SfuCallSession; pc: FakePc }> {
    hooks.welcomeRoster = ["a", "b"];
    const call = await connected();
    await answer();
    return call;
  }

  it("leaves every sender the ceiling it was created with until the SFU says something", async () => {
    const { pc } = await inCallOfThree();
    expect(encodingCeiling(pc.main())).toBeUndefined();
    expect(encodingCeiling(pc.middles()[0])).toBe(500_000);
    expect(encodingCeiling(pc.lights()[0])).toBe(150_000);
  });

  it("sets the main and the middle sender's ceiling with setParameters alone, and never the light one's", async () => {
    const { pc } = await inCallOfThree();
    const main = pc.main();
    const transceivers = [...pc.getTransceivers()];
    const offers = hooks.offers.length;
    const seals = hooks.attaches.length;
    const lightCalls = pc.lights()[0]?.sender.setParametersCalls.length;

    publishBudget(1200, 483);
    await flush();
    expect(encodingCeiling(main)).toBe(1_200_000);
    expect(encodingCeiling(pc.middles()[0])).toBe(483_000);
    expect(encodingCeiling(pc.lights()[0])).toBe(150_000);
    expect(pc.lights()[0]?.sender.setParametersCalls).toHaveLength(lightCalls ?? -1);

    // Nothing else of the encodings moved, and nothing was renegotiated.
    expect(main?.sender.params.encodings[0]?.["scalabilityMode"]).toBe("L1T3");
    expect(pc.middles()[0]?.sender.params.encodings[0]?.["scaleResolutionDownBy"]).toBe(2);
    expect(encodingActive(pc.middles()[0])).toBe(true);
    expect(pc.getTransceivers()).toEqual(transceivers);
    expect(hooks.offers).toHaveLength(offers);
    expect(hooks.attaches).toHaveLength(seals);
  });

  it("ends at the last word under rapid frames", async () => {
    const { pc } = await inCallOfThree();
    publishBudget(2000, 500);
    publishBudget(900, 400);
    publishBudget(1500, 500);
    await flush();
    expect(encodingCeiling(pc.main())).toBe(1_500_000);
    expect(encodingCeiling(pc.middles()[0])).toBe(500_000);
  });

  it("applies beside the SFU's word on which senders are needed, neither undoing the other", async () => {
    const { pc } = await inCallOfThree();
    videoNeeded(false, true);
    publishBudget(700, 500);
    videoNeeded(true, false);
    publishBudget(1553, 500);
    await flush();
    expect(encodingActive(pc.main()) ?? true).toBe(true);
    expect(encodingActive(pc.middles()[0])).toBe(false);
    expect(encodingCeiling(pc.main())).toBe(1_553_000);
    // A sender that is switched off holds its ceiling too.
    expect(encodingCeiling(pc.middles()[0])).toBe(500_000);
    expect(pc.main()?.sender.params.encodings[0]?.["scalabilityMode"]).toBe("L1T3");
  });

  it("puts the ceiling back after a renegotiation or a device switch that reset it", async () => {
    const { session, pc } = await inCallOfThree();
    publishBudget(800, 300);
    await flush();
    const main = pc.main();
    const middle = pc.middles()[0];
    if (main !== undefined) main.sender.params = { encodings: [{}] };
    await negotiate(pc);
    await answer();
    expect(encodingCeiling(main)).toBe(800_000);
    if (main !== undefined) main.sender.params = { encodings: [{}] };
    if (middle !== undefined) middle.sender.params = { encodings: [{}] };
    await session.switchVideoInput("back-camera");
    await flush();
    expect(encodingCeiling(main)).toBe(800_000);
    expect(encodingCeiling(middle)).toBe(300_000);
  });

  it("gives a camera turned on mid-call the ceiling in force, not the one its senders are created with", async () => {
    const { session, pc } = await connected({ media: { audio: true, video: false } });
    await answer();
    welcome(["a", "b"]);
    await flush();
    publishBudget(600, 250);
    await session.publishVideo();
    await flush();
    expect(encodingCeiling(pc.main())).toBe(600_000);
    expect(encodingCeiling(pc.middles()[0])).toBe(250_000);
    expect(encodingCeiling(pc.lights()[0])).toBe(150_000);
    // The mid-call sender still got its temporal layers.
    expect(pc.main()?.sender.params.encodings[0]?.["scalabilityMode"]).toBe("L1T3");
  });

  it("ignores a malformed frame with one warning, and keeps the ceilings it had", async () => {
    const { pc } = await inCallOfThree();
    publishBudget(900, 400);
    await flush();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      publishBudget(0, 400);
      sockets[0]?.onmessage?.({ data: '{"kind":"publish-budget","main_kbps":900}' });
      await flush();
      expect(warn.mock.calls.filter((c) => String(c[0]).includes("malformed publish-budget"))).toHaveLength(2);
      expect(encodingCeiling(pc.main())).toBe(900_000);
      expect(encodingCeiling(pc.middles()[0])).toBe(400_000);
    } finally {
      warn.mockRestore();
    }
  });

  it("touches no sender once the call has ended, and the next call starts from nothing", async () => {
    const first = await inCallOfThree();
    publishBudget(900, 400);
    await flush();
    const main = first.pc.main();
    const calls = main?.sender.setParametersCalls.length;
    const socket = sockets[0];
    first.session.hangUp();
    // A frame that was already on its way.
    expect(() =>
      socket?.onmessage?.({
        data: JSON.stringify({ kind: "publish-budget", main_kbps: 300, middle_kbps: 200 }),
      }),
    ).not.toThrow();
    await flush();
    expect(main?.sender.setParametersCalls).toHaveLength(calls ?? -1);
    // A later call's senders are not written before that call's first frame.
    pcs.length = 0;
    sockets.length = 0;
    const next = await inCallOfThree();
    expect(encodingCeiling(next.pc.main())).toBeUndefined();
    expect(encodingCeiling(next.pc.middles()[0])).toBe(500_000);
  });

  it("says one line on each change and names the ceiling in the send lines", async () => {
    vi.useFakeTimers();
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    try {
      hooks.welcomeRoster = ["a", "b"];
      const session = new SfuCallSession(config({ nowMs: () => 0 }));
      const connecting = session.connect();
      await vi.advanceTimersByTimeAsync(0);
      await connecting;
      const pc = pcs[0];
      if (pc === undefined) throw new Error("no peer connection");
      const budgetLines = (): string[] =>
        info.mock.calls.map((c) => String(c[0])).filter((l) => l.startsWith("call publish budget"));

      publishBudget(1922, 500);
      publishBudget(1922, 500);
      publishBudget(200, 483);
      expect(budgetLines()).toEqual([
        "call publish budget main=1922 middle=500",
        "call publish budget main=200 middle=483",
      ]);

      pc.stats = new Map<string, Record<string, unknown>>([
        ["OM", { id: "OM", type: "outbound-rtp", kind: "video", frameWidth: 1280, frameHeight: 720, framesPerSecond: 30 }],
      ]);
      await vi.advanceTimersByTimeAsync(2_000);
      const lines = info.mock.calls.map((c) => String(c[0])).filter((l) => l.includes(" send video "));
      expect(lines).toEqual([
        "call media interval send video mode=- size=1280x720 fps=30 limited_by=- sender=main needed=- active=yes ceiling=200",
        "call media interval send video mode=- size=- fps=- limited_by=- sender=middle needed=- active=yes ceiling=483",
        "call media interval send video mode=- size=- fps=- limited_by=- sender=light needed=- active=yes ceiling=150",
      ]);
      session.hangUp();
    } finally {
      info.mockRestore();
      vi.useRealTimers();
    }
  });
});
