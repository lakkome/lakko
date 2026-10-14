// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `SfuCallSession` on each of the three kinds of browser
 * (`sframe-transport.ts`): which configuration its peer connection is built
 * with, what it says at the start, that every receiver is covered on the
 * insertable-streams path, and that it refuses before any capture when the
 * browser can reach the SFrame worker by neither interface (invariant #4).
 *
 * Headless: a fake `RTCPeerConnection`, a fake capture, a fake signaling
 * socket; the attach helpers are spies. No browser is run.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const hooks = vi.hoisted(() => ({
  pcs: [] as Array<{
    config: Record<string, unknown>;
    ontrack: ((ev: unknown) => void) | null;
    receivers: object[];
    senders: object[];
  }>,
  senderAttaches: [] as object[],
  receiverAttaches: [] as object[],
  covered: [] as Uint8Array[],
  captures: 0,
  signaling: null as { callbacks: { onSdpAnswer?: (f: { sdp: string }) => void } } | null,
}));

vi.mock("./sfu-signaling-client", () => ({
  SFU_SUBPROTOCOL: "lakko-sfu-v1",
  SfuSignalingClient: class {
    constructor(_url: string, _jwt: string, callbacks: { onSdpAnswer?: (f: { sdp: string }) => void }) {
      hooks.signaling = { callbacks };
    }
    async connect(): Promise<void> {}
    close(): void {}
    get isOpen(): boolean {
      return true;
    }
    sendSignalRelayToCall(): void {}
    sendSignalRelayToPeer(): void {}
    sendSdpOffer(): void {}
    sendSdpAnswer(): void {}
    sendIceCandidate(): void {}
    sendPing(): void {}
    sendVideoWants(): void {}
  },
}));

vi.mock("./sframe-transform", async (importOriginal) => {
  const real = await importOriginal<typeof import("./sframe-transform")>();
  return {
    ...real,
    attachSenderSFrame: (sender: object): string => {
      hooks.senderAttaches.push(sender);
      return `sender-${hooks.senderAttaches.length}`;
    },
    attachReceiverSFrame: (receiver: object): string => {
      hooks.receiverAttaches.push(receiver);
      return `receiver-${hooks.receiverAttaches.length}`;
    },
    coverEncodedStreams: (
      pc: { getReceivers: () => object[] },
      seed: Uint8Array,
    ): { attachedReceiverIds: string[]; unsealedSenders: object[] } => {
      hooks.covered.push(seed);
      const fresh = pc.getReceivers().filter((r) => !hooks.receiverAttaches.includes(r));
      hooks.receiverAttaches.push(...fresh);
      return { attachedReceiverIds: fresh.map((_, i) => `covered-${i}`), unsealedSenders: [] };
    },
    syncSenderCodecs: (): void => {},
    rekeySFrame: (): void => {},
  };
});

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    composeMediaState: (): Uint8Array => new Uint8Array([0]),
  }),
}));

import { SfuCallSession } from "./sfu-call-session";

class FakePeerConnection {
  ontrack: ((ev: unknown) => void) | null = null;
  onicecandidate: unknown = null;
  onnegotiationneeded: unknown = null;
  oniceconnectionstatechange: unknown = null;
  onconnectionstatechange: unknown = null;
  iceConnectionState = "new";
  connectionState = "new";
  signalingState = "stable";
  readonly receivers: object[] = [];
  readonly senders: object[] = [];
  readonly config: Record<string, unknown>;
  constructor(config: Record<string, unknown>) {
    this.config = config;
    hooks.pcs.push(this);
  }
  addTrack(track: object): object {
    const sender = { track };
    this.senders.push(sender);
    return sender;
  }
  addTransceiver(): object {
    return { sender: {}, receiver: {} };
  }
  async createOffer(): Promise<{ type: string; sdp: string }> {
    return { type: "offer", sdp: "v=0" };
  }
  async setLocalDescription(): Promise<void> {}
  async setRemoteDescription(): Promise<void> {}
  getSenders(): object[] {
    return this.senders;
  }
  getReceivers(): object[] {
    return this.receivers;
  }
  getTransceivers(): unknown[] {
    return [];
  }
  async getStats(): Promise<Map<string, unknown>> {
    return new Map();
  }
  close(): void {}
}

function fakeStream(id: string): object {
  const track = { kind: "audio", enabled: true, stop: (): void => {}, addEventListener: (): void => {} };
  return {
    id,
    getTracks: () => [track],
    getAudioTracks: () => [track],
    getVideoTracks: () => [],
  };
}

class WithStreams {
  createEncodedStreams(): unknown {
    return {};
  }
}
class ScriptTransform {}

const SEED = new Uint8Array(32).fill(1);

function session(): SfuCallSession {
  return new SfuCallSession({
    sfuBaseUrl: "wss://sfu.test",
    jwt: "jwt",
    seed: SEED,
    localLeaf: 0,
    media: { audio: true, video: false },
  });
}

/** Everything logged with `console.info`, first argument only. */
const said: unknown[] = [];
let info: { mockRestore: () => void };

beforeEach(() => {
  hooks.pcs = [];
  hooks.senderAttaches = [];
  hooks.receiverAttaches = [];
  hooks.covered = [];
  hooks.captures = 0;
  hooks.signaling = null;
  vi.stubGlobal("RTCPeerConnection", FakePeerConnection);
  vi.stubGlobal("navigator", {
    mediaDevices: {
      getUserMedia: async () => {
        hooks.captures += 1;
        return fakeStream("local");
      },
    },
  });
  said.length = 0;
  info = vi.spyOn(console, "info").mockImplementation((first: unknown) => {
    said.push(first);
  });
});

afterEach(() => {
  info.mockRestore();
  vi.unstubAllGlobals();
});

describe("SfuCallSession on a browser with createEncodedStreams only", () => {
  beforeEach(() => {
    vi.stubGlobal("RTCRtpSender", WithStreams);
    vi.stubGlobal("RTCRtpReceiver", WithStreams);
  });

  it("builds its peer connection for insertable streams, and says which path it took", async () => {
    await session().connect();
    expect(hooks.pcs).toHaveLength(1);
    expect(hooks.pcs[0]?.config.encodedInsertableStreams).toBe(true);
    expect(hooks.pcs[0]?.config.bundlePolicy).toBe("max-bundle");
    expect(hooks.pcs[0]?.config.rtcpMuxPolicy).toBe("require");
    expect(said).toContain("sframe transport: encoded-streams");
    // The microphone's sender was sealed before the offer.
    expect(hooks.senderAttaches).toEqual(hooks.pcs[0]?.senders);
  });

  it("covers a receiver no track event announced, once the SFU has answered", async () => {
    await session().connect();
    const pc = hooks.pcs[0]!;
    const silent = { name: "receiver nobody announced" };
    pc.receivers.push(silent);
    expect(hooks.covered).toHaveLength(0);
    hooks.signaling?.callbacks.onSdpAnswer?.({ sdp: "v=0" });
    await vi.waitFor(() => expect(hooks.covered.length).toBeGreaterThan(0));
    expect(hooks.receiverAttaches).toContain(silent);
    expect(Array.from(hooks.covered[0] ?? [])).toEqual(Array.from(SEED));
  });
});

describe("SfuCallSession on a browser with RTCRtpScriptTransform", () => {
  beforeEach(() => {
    vi.stubGlobal("RTCRtpScriptTransform", ScriptTransform);
    // Chromium has both; the script transform is still the one used.
    vi.stubGlobal("RTCRtpSender", WithStreams);
    vi.stubGlobal("RTCRtpReceiver", WithStreams);
  });

  it("does not ask for insertable streams, and never runs the cover", async () => {
    await session().connect();
    expect(hooks.pcs).toHaveLength(1);
    expect("encodedInsertableStreams" in (hooks.pcs[0]?.config ?? {})).toBe(false);
    expect(said).toContain("sframe transport: script-transform");
    hooks.pcs[0]?.receivers.push({});
    hooks.signaling?.callbacks.onSdpAnswer?.({ sdp: "v=0" });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(hooks.covered).toHaveLength(0);
  });
});

describe("SfuCallSession on a browser with neither", () => {
  it("refuses before any capture and before any peer connection (invariant #4)", async () => {
    await expect(session().connect()).rejects.toThrow(/invariant #4/);
    expect(hooks.captures).toBe(0);
    expect(hooks.pcs).toHaveLength(0);
    expect(hooks.senderAttaches).toHaveLength(0);
    expect(said).not.toContain("sframe transport: none");
  });
});
