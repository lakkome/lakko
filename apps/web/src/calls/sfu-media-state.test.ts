// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * camera-off-cleanup (web) — the SFU media-state signal wiring.
 *
 * A remote participant turning its camera OFF is a mute (`track.enabled=false`),
 * which fires NO track event, so the roster tile would keep painting a frozen /
 * black frame with `camOn` stuck true. The fix broadcasts an app-level camera
 * on/off signal over the opaque SFU `signal-relay` payload; the receiver flips
 * that participant's `camOn` deterministically.
 *
 * Since ADR-0078 section 2 the signal is sealed under the call seed's relay
 * key (the SFU relays ciphertext only). These tests pin, headlessly (node
 * env, no WebRTC / DOM):
 *   * the RECEIVE + classify path in {@link SfuCallSession}: a SEALED
 *     `MediaState` relay is consumed → `onRemoteMediaState` fires; anything
 *     the relay key does not open falls through to `onSignalRelay` unchanged
 *     (the ADR-0032 orchestrator), an UNSEALED media state included - it is
 *     never trusted;
 *   * a sealed peer announcement is verified by the announcer and fires
 *     `onPeerIdentified`;
 *   * the {@link createSfuCallUi} mapping: a `MediaState(off/on)` relay flips
 *     the matching store participant's `camOn`;
 *   * the SEND triggers: a camera toggle broadcasts, and a peer-join
 *     re-announces the local camera state.
 *
 * The signaling socket + wasm + SFrame transform are mocked; `deferPublish`
 * keeps `connect()` off the `getUserMedia` / `RTCPeerConnection` path.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// ---- Mocks --------------------------------------------------------------

// Captured signaling callbacks + recorded broadcasts, shared with the test
// body via `vi.hoisted` (evaluated before the mock factories).
const hooks = vi.hoisted(() => ({
  captured: null as {
    onWelcome?: (f: unknown) => void;
    onPeerJoined?: (f: unknown) => void;
    onPeerLeft?: (f: unknown) => void;
    onSignalRelay?: (f: {
      kind: "signal-relay";
      from: string;
      payload_b64: string;
    }) => void;
    onSdpAnswer?: (f: unknown) => void;
    onSdpOffer?: (f: unknown) => void;
    onServerError?: (f: unknown) => void;
    onClose?: () => void;
    onSocketError?: () => void;
  } | null,
  broadcasts: [] as Uint8Array[],
}));

// Fake signaling client: capture the callbacks the session wires in, record
// call-broadcasts, resolve connect() immediately.
vi.mock("./sfu-signaling-client", () => ({
  SFU_SUBPROTOCOL: "lakko-sfu-v1",
  SfuSignalingClient: class {
    constructor(_base: string, _jwt: string, callbacks: unknown) {
      hooks.captured = callbacks as typeof hooks.captured;
    }
    async connect(): Promise<void> {}
    close(): void {}
    get isOpen(): boolean {
      return true;
    }
    sendSignalRelayToCall(bytes: Uint8Array): void {
      hooks.broadcasts.push(bytes);
    }
    sendSignalRelayToPeer(): void {}
    sendSdpOffer(): void {}
    sendSdpAnswer(): void {}
    sendIceCandidate(): void {}
    sendPing(): void {}
  },
}));

// SFrame transform: report supported (invariant #4 gate) + no-op attach.
vi.mock("./sframe-transform", () => ({
  isSFrameTransformSupported: (): boolean => true,
  attachSenderSFrame: (): string => "s",
  attachReceiverSFrame: (): string => "r",
  rekeySFrame: (): void => {},
}));

// wasm surface the exercised paths touch. `sfuDecodePayload` is identity-ish
// (decode the b64 field as UTF-8 text) so the test can hand-craft envelopes;
// `dispatchIncomingEnvelope` classifies that text; `composeMediaState` tags the
// broadcast bytes so the send tests can read the intended flag back.
vi.mock("../wasm", () => {
  // Mirrors the real enum's tags (`WasmIncomingEnvelopeKind`), including the
  // kinds this suite never dispatches, so a relayed payload the mock classified
  // as one of them cannot silently collide with a tag it does use.
  const IncomingEnvelopeKind = Object.freeze({
    CallWelcome: 0,
    CallKeyPackage: 1,
    IntroMessage: 2,
    MediaState: 3,
    PushRoutingSecret: 4,
    CommitterBeacon: 5,
    Profile: 6,
  });
  return {
    loadWasm: async () => ({
      IncomingEnvelopeKind,
      sfuDecodePayload: (b64: string): Uint8Array =>
        new TextEncoder().encode(b64),
      dispatchIncomingEnvelope: (
        payload: Uint8Array,
      ): { kind: number; cameraOn: boolean | undefined; free: () => void } => {
        const s = new TextDecoder().decode(payload);
        if (s === "media:on") {
          return { kind: IncomingEnvelopeKind.MediaState, cameraOn: true, free: () => {} };
        }
        if (s === "media:off") {
          return { kind: IncomingEnvelopeKind.MediaState, cameraOn: false, free: () => {} };
        }
        if (s === "media:malformed") {
          throw new Error("matched media-state domain but malformed body");
        }
        if (s === "profile") {
          // ADR-0048 — a profile envelope classifies as its own kind, with no
          // camera flag.
          return { kind: IncomingEnvelopeKind.Profile, cameraOn: undefined, free: () => {} };
        }
        // Anything else classifies as a plain intro (fall-through).
        return {
          kind: IncomingEnvelopeKind.IntroMessage,
          cameraOn: undefined,
          free: () => {},
        };
      },
      composeMediaState: (on: boolean): Uint8Array =>
        new TextEncoder().encode(on ? "compose:on" : "compose:off"),
      // ADR-0078 section 2: a fake relay key. "Sealing" prefixes `sealed:`;
      // opening accepts only that prefix, so an unsealed payload never opens.
      CallRelayKey: {
        fromCallSeed: (): unknown => ({
          seal: (pt: Uint8Array): Uint8Array =>
            new TextEncoder().encode(`sealed:${new TextDecoder().decode(pt)}`),
          open: (sealed: Uint8Array): Uint8Array => {
            const t = new TextDecoder().decode(sealed);
            if (!t.startsWith("sealed:")) throw new Error("notOpened");
            return new TextEncoder().encode(t.slice("sealed:".length));
          },
          free: (): void => {},
        }),
      },
      classifyCallRelayPlaintext: (pt: Uint8Array) => {
        const t = new TextDecoder().decode(pt);
        const free = (): void => {};
        if (t === "media:on" || t === "compose:on") {
          return { kind: "media-state", cameraOn: true, announcement: new Uint8Array(), free };
        }
        if (t === "media:off" || t === "compose:off") {
          return { kind: "media-state", cameraOn: false, announcement: new Uint8Array(), free };
        }
        if (t.startsWith("ann:")) {
          return { kind: "peer-announcement", cameraOn: false, announcement: pt, free };
        }
        return { kind: "unknown", cameraOn: false, announcement: new Uint8Array(), free };
      },
    }),
  };
});

import { SfuCallSession, type SfuCallConfig } from "./sfu-call-session";
import { createSfuCallUi } from "./sfu-call-ui";
import { callActions, callState } from "../state/call-state";

// ---- Helpers ------------------------------------------------------------

/** Drain microtasks (the relay handler + announce are fire-and-forget async). */
function flush(): Promise<void> {
  return new Promise((r) => setTimeout(r, 0));
}

function baseConfig(overrides: Partial<SfuCallConfig> = {}): SfuCallConfig {
  return {
    sfuBaseUrl: "wss://sfu.test",
    jwt: "jwt",
    seed: new Uint8Array(32),
    localLeaf: 0,
    media: { audio: true, video: false },
    deferPublish: true,
    ...overrides,
  };
}

function relay(from: string, payload_b64: string): void {
  hooks.captured?.onSignalRelay?.({ kind: "signal-relay", from, payload_b64 });
}

const decode = (b: Uint8Array): string => new TextDecoder().decode(b);

beforeEach(() => {
  hooks.captured = null;
  hooks.broadcasts = [];
  callActions.reset();
});

afterEach(() => {
  vi.clearAllMocks();
});

// ---- Receive + classify (session level) --------------------------------

describe("SfuCallSession — relay classification", () => {
  it("consumes a sealed MediaState relay → onRemoteMediaState (not onSignalRelay)", async () => {
    const media: Array<[string, boolean]> = [];
    const opaque: Array<[string, Uint8Array]> = [];
    const session = new SfuCallSession(baseConfig(), {
      onRemoteMediaState: (peerId, on) => media.push([peerId, on]),
      onSignalRelay: (from, payload) => opaque.push([from, payload]),
    });
    await session.connect();

    relay("peerX", "sealed:media:off");
    await flush();
    expect(media).toEqual([["peerX", false]]);
    expect(opaque).toHaveLength(0); // consumed, never double-dispatched

    relay("peerX", "sealed:media:on");
    await flush();
    expect(media).toEqual([
      ["peerX", false],
      ["peerX", true],
    ]);
    expect(opaque).toHaveLength(0);
  });

  it("never trusts an UNSEALED media state: it falls through, no camera flips", async () => {
    const media: Array<[string, boolean]> = [];
    const opaque: Array<[string, string]> = [];
    const session = new SfuCallSession(baseConfig(), {
      onRemoteMediaState: (peerId, on) => media.push([peerId, on]),
      onSignalRelay: (from, payload) => opaque.push([from, decode(payload)]),
    });
    await session.connect();

    relay("peerX", "media:off");
    await flush();
    expect(media).toHaveLength(0);
    expect(opaque).toEqual([["peerX", "media:off"]]);
  });

  it("a sealed announcement that verifies names the peer; one that does not, nobody", async () => {
    const identified: Array<[string, string]> = [];
    const session = new SfuCallSession(
      baseConfig({
        peerAnnouncer: {
          compose: () => new TextEncoder().encode("ann:self"),
          verify: (from, ann) =>
            decode(ann) === `ann:${from}`
              ? { personKeyB64: `person-of-${from}`, memberKeyHex: `leaf-of-${from}` }
              : null,
        },
      }),
      { onPeerIdentified: (peerId, id) => identified.push([peerId, id.personKeyB64]) },
    );
    await session.connect();

    relay("peerA", "sealed:ann:peerA");
    relay("peerB", "sealed:ann:someone-else");
    await flush();
    expect(identified).toEqual([["peerA", "person-of-peerA"]]);
  });

  it("falls through a non-media-state payload to onSignalRelay", async () => {
    const media: Array<[string, boolean]> = [];
    const opaque: Array<[string, string]> = [];
    const session = new SfuCallSession(baseConfig(), {
      onRemoteMediaState: (peerId, on) => media.push([peerId, on]),
      onSignalRelay: (from, payload) => opaque.push([from, decode(payload)]),
    });
    await session.connect();

    relay("peerY", "call-welcome-bytes");
    await flush();
    expect(media).toHaveLength(0);
    expect(opaque).toEqual([["peerY", "call-welcome-bytes"]]);
  });

  it("never consumes a Profile relay as a media-state signal (ADR-0048)", async () => {
    // A Profile is not a media-state signal: it must not flip a camera tile,
    // and the session passes it on untouched for the orchestrator to drop.
    const media: Array<[string, boolean]> = [];
    const opaque: Array<[string, string]> = [];
    const session = new SfuCallSession(baseConfig(), {
      onRemoteMediaState: (peerId, on) => media.push([peerId, on]),
      onSignalRelay: (from, payload) => opaque.push([from, decode(payload)]),
    });
    await session.connect();

    relay("peerP", "profile");
    await flush();
    expect(media).toHaveLength(0);
    expect(opaque).toEqual([["peerP", "profile"]]);
  });

  it("drops a sealed payload it cannot classify, without passing it on", async () => {
    const media: Array<[string, boolean]> = [];
    const opaque: Array<[string, string]> = [];
    const session = new SfuCallSession(baseConfig(), {
      onRemoteMediaState: (peerId, on) => media.push([peerId, on]),
      onSignalRelay: (from, payload) => opaque.push([from, decode(payload)]),
    });
    await session.connect();

    relay("peerZ", "sealed:media:malformed");
    await flush();
    expect(media).toHaveLength(0);
    expect(opaque).toHaveLength(0);
  });
});

// ---- createSfuCallUi mapping (store level) -----------------------------

describe("createSfuCallUi — media-state flips the roster tile", () => {
  function camOn(peerId: string): boolean | undefined {
    return callState
      .participants()
      .find((p) => p.id === `peer-${peerId}`)?.camOn;
  }

  it("MediaState(on) then (off) flips the matching participant's camOn", async () => {
    const handle = createSfuCallUi(baseConfig());
    await handle.connect();

    // Build the roster: peerX is a remote (tile camOn defaults false).
    hooks.captured?.onWelcome?.({
      kind: "welcome",
      peer_id: "self",
      room: "r",
      roster: [{ peer_id: "peerX" }],
      server_ts: 0,
    });
    expect(camOn("peerX")).toBe(false);

    relay("peerX", "sealed:media:on");
    await flush();
    expect(camOn("peerX")).toBe(true);

    relay("peerX", "sealed:media:off");
    await flush();
    expect(camOn("peerX")).toBe(false);

    handle.hangUp();
  });

  it("a MediaState relay for an unknown peer is a harmless no-op", async () => {
    const handle = createSfuCallUi(baseConfig());
    await handle.connect();
    hooks.captured?.onWelcome?.({
      kind: "welcome",
      peer_id: "self",
      room: "r",
      roster: [{ peer_id: "peerX" }],
      server_ts: 0,
    });

    relay("ghost", "sealed:media:on");
    await flush();
    // peerX untouched; no participant materialised for "ghost".
    expect(camOn("peerX")).toBe(false);
    expect(camOn("ghost")).toBeUndefined();

    handle.hangUp();
  });
});

// ---- Send triggers ------------------------------------------------------

describe("SfuCallSession — send triggers", () => {
  it("broadcasts on a camera toggle (OFF then back ON)", async () => {
    const session = new SfuCallSession(baseConfig({ media: { audio: true, video: true } }));
    await session.connect();
    hooks.broadcasts = []; // ignore any connect-path broadcast

    session.setCameraEnabled(false);
    await flush();
    expect(hooks.broadcasts.map(decode)).toEqual(["sealed:compose:off"]);

    session.setCameraEnabled(true);
    await flush();
    expect(hooks.broadcasts.map(decode)).toEqual(["sealed:compose:off", "sealed:compose:on"]);
  });

  it("does not re-broadcast when the camera state is unchanged", async () => {
    const session = new SfuCallSession(baseConfig({ media: { audio: true, video: false } }));
    await session.connect();
    hooks.broadcasts = [];

    // Already off (media.video=false) — a redundant setCameraEnabled(false) is
    // a no-op, no broadcast.
    session.setCameraEnabled(false);
    await flush();
    expect(hooks.broadcasts).toHaveLength(0);
  });

  it("re-announces the local camera state when a new peer joins", async () => {
    const session = new SfuCallSession(baseConfig({ media: { audio: true, video: true } }));
    await session.connect();
    hooks.broadcasts = [];

    hooks.captured?.onPeerJoined?.({
      kind: "peer-joined",
      peer_id: "newcomer",
    });
    await flush();
    // Local camera is on → re-announce "on" so the newcomer is shown on.
    expect(hooks.broadcasts.map(decode)).toEqual(["sealed:compose:on"]);
  });
});
