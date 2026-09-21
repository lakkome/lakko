// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The answer to a peer's mid-call offer must arm the generic frame descriptor.
 *
 * Both web sessions armed the descriptor on every m-line they OFFER on and on
 * none of the m-lines the PEER opens. That is the only kind of m-line a remote
 * camera arrives on: the phone turning its camera on mid-call sends an offer,
 * and the browser answers it — creating the inbound video transceiver inside
 * `setRemoteDescription` with the descriptor stopped, as every fresh
 * transceiver is.
 *
 * Why that is fatal rather than cosmetic, measured in a Chrome loopback
 * (2026-09-21, three runs over one canvas source): plain video assembles
 * (8 frames sent, 8 received, 8 decoded); the SAME video with an opaque
 * payload and NO descriptor assembles nothing at all (115 packets received,
 * `framesReceived` 0, 41 PLIs); the same opaque payload WITH the descriptor
 * negotiated assembles and decodes every frame (8/8). So a browser can read
 * the descriptor perfectly well — it simply has to be asked, per call and per
 * m-line, and asking is the page's job.
 *
 * Live symptom this closes: phone -> web video delivered 1748 packets and zero
 * frames on a 1×1 call, and a browser showed a blank tile for another peer's
 * camera in a link call, while both phones saw each other fine.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../wasm", () => {
  const ascii = (s: string): Uint8Array => new TextEncoder().encode(s);
  return {
    loadWasm: async () => ({
      composeCallAnswerSignaling: (): Uint8Array => ascii("answer"),
      composeCallOfferSignaling: (): Uint8Array => ascii("offer"),
      composeCallIceCandidateSignaling: (): Uint8Array => ascii("ice"),
      composeCallEndSignaling: (): Uint8Array => ascii("end"),
    }),
  };
});

import { CallSession } from "./call-session";
import type { CallSessionCallbacks } from "./call-session";
import { GENERIC_FRAME_DESCRIPTOR_URI } from "./frame-descriptor";
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

/** A transceiver that models the one thing under test: the Header Extension
 *  Control API, with the descriptor `stopped` as a real fresh transceiver
 *  has it. */
class FakeTransceiver {
  readonly receiver: { track: { kind: string } | null };
  private exts = [
    { uri: GENERIC_FRAME_DESCRIPTOR_URI, direction: "stopped" },
    { uri: "urn:ietf:params:rtp-hdrext:sdes:mid", direction: "sendrecv" },
  ];
  constructor(kind: "audio" | "video") {
    this.receiver = { track: { kind } };
  }
  get descriptorDirection(): string {
    return this.exts.find((e) => e.uri === GENERIC_FRAME_DESCRIPTOR_URI)?.direction ?? "absent";
  }
  getHeaderExtensionsToNegotiate(): Array<{ uri: string; direction: string }> {
    return this.exts.map((e) => ({ ...e }));
  }
  setHeaderExtensionsToNegotiate(list: Array<{ uri: string; direction: string }>): void {
    this.exts = list.map((e) => ({ ...e }));
  }
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
  closed = false;
  transceivers: FakeTransceiver[] = [];
  /** The descriptor's direction on the inbound video m-line at the moment the
   *  ANSWER was produced — the only moment that decides what goes on the
   *  wire. `absent` means there was no inbound video transceiver yet. */
  descriptorAtAnswer: string | null = null;

  constructor(_config?: unknown) {}

  addTrack(track: FakeTrack, _stream: FakeStream): { track: FakeTrack | null } {
    this.transceivers.push(new FakeTransceiver(track.kind));
    return { track };
  }
  getSenders(): Array<{ track: FakeTrack | null }> {
    return [];
  }
  getTransceivers(): FakeTransceiver[] {
    return this.transceivers;
  }
  async createOffer(): Promise<{ type: string; sdp: string }> {
    return { type: "offer", sdp: "v=0 fake-offer" };
  }
  async createAnswer(): Promise<{ type: string; sdp: string }> {
    return { type: "answer", sdp: "v=0 fake-answer" };
  }
  async setLocalDescription(desc?: { type: string; sdp: string }): Promise<void> {
    const type =
      desc?.type ?? (this.signalingState === "have-remote-offer" ? "answer" : "offer");
    if (type === "answer") {
      const video = this.transceivers.filter((t) => t.receiver.track?.kind === "video");
      const last = video[video.length - 1];
      this.descriptorAtAnswer = last?.descriptorDirection ?? "absent";
    }
    this.localDescription = desc ?? { type, sdp: "v=0 fake-local" };
    this.signalingState = type === "offer" ? "have-local-offer" : "stable";
  }
  /** A remote offer that opens a video m-line CREATES the transceiver — the
   *  browser does this inside `setRemoteDescription`, which is why arming
   *  before this point cannot reach it. */
  async setRemoteDescription(desc: { type: string; sdp: string }): Promise<void> {
    this.remoteDescription = desc;
    if (desc.type === "offer" && desc.sdp.includes("m=video")) {
      this.transceivers.push(new FakeTransceiver("video"));
    }
    this.signalingState = desc.type === "offer" ? "have-remote-offer" : "stable";
  }
  async addIceCandidate(): Promise<void> {}
  async getStats(): Promise<Map<string, unknown>> {
    return new Map();
  }
  close(): void {
    this.closed = true;
  }
}

let pcs: FakePeerConnection[] = [];

beforeEach(() => {
  pcs = [];
  vi.stubGlobal("navigator", {
    mediaDevices: {
      getUserMedia: async (c: MediaStreamConstraints): Promise<FakeStream> => {
        const tracks: FakeTrack[] = [];
        if (c.audio) tracks.push(new FakeTrack("audio"));
        if (c.video) tracks.push(new FakeTrack("video"));
        return new FakeStream(tracks);
      },
    },
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
  vi.unstubAllGlobals();
});

function makeSession(): CallSession {
  const callbacks: CallSessionCallbacks = {
    onRemoteStream: () => {},
    onLocalStream: () => {},
    onOutboundSignaling: () => {},
    onStateChange: () => {},
    onVideoPublishState: () => {},
  };
  const session = new CallSession(new Uint8Array(16), callbacks, {}, {
    supported: () => true,
    attachSender: () => {},
    attachReceiver: () => {},
  });
  session.setCallKey(new Uint8Array(32), 3);
  return session;
}

async function settle(): Promise<void> {
  for (let i = 0; i < 20; i += 1) await Promise.resolve();
}

function audioOffer(): ParsedCallSignaling {
  return {
    kind: "offer",
    callId: new Uint8Array(16),
    sdp: "v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
    mediaAudio: true,
    mediaVideo: false,
  } as unknown as ParsedCallSignaling;
}

/** The peer turning its camera on mid-call. */
function videoRenegotiationOffer(): ParsedCallSignaling {
  return {
    kind: "offer",
    callId: new Uint8Array(16),
    sdp: "v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n",
    mediaAudio: true,
    mediaVideo: true,
  } as unknown as ParsedCallSignaling;
}

describe("1×1: answering a peer's mid-call offer", () => {
  it("arms the frame descriptor on the video m-line the PEER opened", async () => {
    const session = makeSession();
    await session.applyIncomingSignaling(audioOffer());
    await session.accept();
    await settle();

    const pc = pcs[0];
    expect(pc).toBeDefined();
    if (pc === undefined) throw new Error("no peer connection");

    // The peer adds video. The browser answers; the inbound video transceiver
    // is born inside that exchange.
    await session.applyIncomingSignaling(videoRenegotiationOffer());
    await settle();

    expect(pc.descriptorAtAnswer).toBe("sendrecv");
  });

  it("leaves the descriptor armed on the transceiver afterwards", async () => {
    const session = makeSession();
    await session.applyIncomingSignaling(audioOffer());
    await session.accept();
    await settle();
    await session.applyIncomingSignaling(videoRenegotiationOffer());
    await settle();

    const pc = pcs[0];
    if (pc === undefined) throw new Error("no peer connection");
    const video = pc.getTransceivers().filter((t) => t.receiver.track?.kind === "video");
    expect(video.length).toBeGreaterThan(0);
    for (const t of video) expect(t.descriptorDirection).toBe("sendrecv");
  });
});
