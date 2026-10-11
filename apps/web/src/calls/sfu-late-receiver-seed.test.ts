// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A remote track that arrives AFTER a mid-call rekey must be decrypted under
 * the CURRENT call key, not the key the session published with.
 *
 * Found live on a `/c/` link call: the owner publishes at the founding epoch,
 * admitting the joiner advances the MLS epoch and `rekey`s the session, and the
 * joiner's tracks only then reach the owner through `ontrack`. The receiver
 * transform was armed with the seed captured when the peer connection was
 * built, so every inbound frame failed the AEAD open and was dropped: the owner
 * received the joiner's RTP (`packetsReceived` climbing) but decoded no audio
 * and no video (`framesReceived` 0, PLIs piling up), while the safety numbers
 * on both screens matched.
 *
 * `rekey` already records the new seed as `activeSeed` precisely so a
 * late-arriving receiver arms at the new epoch; this pins that the `ontrack`
 * path reads it.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const hooks = vi.hoisted(() => ({
  receiverSeeds: [] as Uint8Array[],
  pcs: [] as Array<{ ontrack: ((ev: unknown) => void) | null }>,
}));

vi.mock("./sfu-signaling-client", () => ({
  SFU_SUBPROTOCOL: "lakko-sfu-v1",
  SfuSignalingClient: class {
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
  },
}));

vi.mock("./sframe-transform", () => ({
  isSFrameTransformSupported: (): boolean => true,
  attachSenderSFrame: (): string => "sender",
  attachReceiverSFrame: (
    _receiver: unknown,
    opts: { readonly seed: Uint8Array },
  ): string => {
    hooks.receiverSeeds.push(opts.seed);
    return `receiver-${hooks.receiverSeeds.length}`;
  },
  rekeySFrame: (): void => {},
}));

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
  iceConnectionState = "new";
  constructor() {
    hooks.pcs.push(this);
  }
  addTrack(): object {
    return {};
  }
  async createOffer(): Promise<{ type: string; sdp: string }> {
    return { type: "offer", sdp: "v=0" };
  }
  async setLocalDescription(): Promise<void> {}
  async setRemoteDescription(): Promise<void> {}
  getSenders(): unknown[] {
    return [];
  }
  getTransceivers(): unknown[] {
    return [];
  }
  close(): void {}
}

function fakeStream(id: string): object {
  const track = { kind: "audio", enabled: true, stop: (): void => {} };
  return {
    id,
    getTracks: () => [track],
    getAudioTracks: () => [track],
    getVideoTracks: () => [],
  };
}

beforeEach(() => {
  hooks.receiverSeeds = [];
  hooks.pcs = [];
  vi.stubGlobal("RTCPeerConnection", FakePeerConnection);
  vi.stubGlobal("navigator", {
    mediaDevices: { getUserMedia: async () => fakeStream("local") },
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("SfuCallSession — receiver keying after a rekey", () => {
  it("arms a track that arrives after rekey with the new seed", async () => {
    const foundingSeed = new Uint8Array(32).fill(1);
    const admittedSeed = new Uint8Array(32).fill(2);
    const session = new SfuCallSession({
      sfuBaseUrl: "wss://sfu.test",
      jwt: "jwt",
      seed: foundingSeed,
      localLeaf: 0,
      media: { audio: true, video: false },
    });
    await session.connect();
    const pc = hooks.pcs[0];
    expect(pc?.ontrack).toBeTypeOf("function");

    // Admitting a joiner advanced the epoch.
    session.rekey(admittedSeed);

    // The joiner's track reaches us only now.
    pc?.ontrack?.({
      streams: [fakeStream("joiner")],
      track: { kind: "audio" },
      receiver: {},
    });

    expect(hooks.receiverSeeds).toHaveLength(1);
    expect(Array.from(hooks.receiverSeeds[0] ?? [])).toEqual(
      Array.from(admittedSeed),
    );
  });

  it("arms a track that arrives before any rekey with the publish seed", async () => {
    const foundingSeed = new Uint8Array(32).fill(7);
    const session = new SfuCallSession({
      sfuBaseUrl: "wss://sfu.test",
      jwt: "jwt",
      seed: foundingSeed,
      localLeaf: 0,
      media: { audio: true, video: false },
    });
    await session.connect();
    hooks.pcs[0]?.ontrack?.({
      streams: [fakeStream("early")],
      track: { kind: "audio" },
      receiver: {},
    });
    expect(Array.from(hooks.receiverSeeds[0] ?? [])).toEqual(
      Array.from(foundingSeed),
    );
  });
});

/**
 * The call screen reads a person's sound level from the receiver that
 * decodes their audio (`audio-level.ts`), on this device.
 */
describe("SfuCallSession: the audio receiver of a peer", () => {
  async function connected(): Promise<SfuCallSession> {
    const session = new SfuCallSession({
      sfuBaseUrl: "wss://sfu.test",
      jwt: "jwt",
      seed: new Uint8Array(32).fill(3),
      localLeaf: 0,
      media: { audio: true, video: false },
    });
    await session.connect();
    return session;
  }

  it("is known by peer id from the moment the track is announced, and only for audio", async () => {
    const session = await connected();
    const audio = { track: { readyState: "live" } };
    const seen: unknown[] = [];
    // The receiver must already answer inside the track callback.
    (session as unknown as { callbacks: { onRemoteTrack?: (rt: { streamId: string }) => void } }).callbacks.onRemoteTrack =
      (rt): void => {
        seen.push(session.audioReceiverFor(rt.streamId));
      };
    hooks.pcs[0]?.ontrack?.({ streams: [fakeStream("b")], track: { kind: "audio" }, receiver: audio });
    hooks.pcs[0]?.ontrack?.({
      streams: [fakeStream("c")],
      track: { kind: "video", addEventListener: () => undefined },
      receiver: {},
    });
    expect(seen[0]).toBe(audio);
    expect(session.audioReceiverFor("b")).toBe(audio);
    expect(session.audioReceiverFor("c")).toBeUndefined();
    expect(session.audioReceiverFor("nobody")).toBeUndefined();
  });

  it("is known for a peer whose audio arrives in a later track event than their video", async () => {
    const session = await connected();
    const stream = fakeStream("b");
    const announced: { kind: string; receiver: unknown }[] = [];
    (session as unknown as { callbacks: { onRemoteTrack?: (rt: { streamId: string; track: { kind: string } }) => void } }).callbacks.onRemoteTrack =
      (rt): void => {
        announced.push({ kind: rt.track.kind, receiver: session.audioReceiverFor(rt.streamId) });
      };
    hooks.pcs[0]?.ontrack?.({
      streams: [stream],
      track: { kind: "video", addEventListener: () => undefined },
      receiver: {},
    });
    const audio = { track: { readyState: "live" } };
    hooks.pcs[0]?.ontrack?.({ streams: [stream], track: { kind: "audio" }, receiver: audio });
    // Each track is announced, so whoever maps streams is told twice and
    // finds the receiver the second time.
    expect(announced).toEqual([
      { kind: "video", receiver: undefined },
      { kind: "audio", receiver: audio },
    ]);
  });

  it("is gone once the track has ended and after hang-up", async () => {
    const session = await connected();
    const b = { track: { readyState: "live" } };
    const c = { track: { readyState: "live" } };
    hooks.pcs[0]?.ontrack?.({ streams: [fakeStream("b")], track: { kind: "audio" }, receiver: b });
    hooks.pcs[0]?.ontrack?.({ streams: [fakeStream("c")], track: { kind: "audio" }, receiver: c });
    b.track.readyState = "ended";
    expect(session.audioReceiverFor("b")).toBeUndefined();
    expect(session.audioReceiverFor("c")).toBe(c);
    session.hangUp();
    expect(session.audioReceiverFor("c")).toBeUndefined();
  });
});
