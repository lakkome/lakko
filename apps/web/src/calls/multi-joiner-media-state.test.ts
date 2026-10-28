// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0032 multi-joiner call-link orchestrator — remote camera-state wiring.
 *
 * Brings {@link MultiJoinerCall} to parity with the in-chat `sfu-call-ui.ts`
 * for the camera on/off signal on a `/c/<slug>` link-call. Two behaviours are
 * pinned headlessly (node env, no WebRTC / DOM):
 *
 *   * `onRemoteMediaState(peerId, cameraOn)` — a peer's camera-toggle mute
 *     (which fires NO track event) flips the matching `peer-<peerId>` roster
 *     tile's `camOn` deterministically, so the tile stops painting a frozen
 *     frame;
 *   * `onRemoteTrack` `ended` — a genuine track-gone (unpublish / leave)
 *     clears `camOn` on the video track AND prunes the `remoteStreams` entry
 *     only when no live track remains (a camera-off keeps its audio track +
 *     entry), mirroring `sfu-call-ui.ts` `addStream` exactly.
 *
 * The `SfuCallSession` (send side + N-way video), wasm, JWT mint, and
 * group-id derivation are mocked; the orchestrator, presence, election, and
 * the shared `callActions` store are the real code under test. The mocked
 * session captures the consumer callbacks the orchestrator wires in, so the
 * two behaviours are exercised on a REAL `MultiJoinerCall` instance.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// ---- Mocks --------------------------------------------------------------

// The consumer callbacks the orchestrator passes to `new SfuCallSession(...)`,
// captured by the fake session and driven from the test body.
const hooks = vi.hoisted(() => ({
  captured: null as {
    onWelcome?: (
      selfPeerId: string,
      roster: ReadonlyArray<{
        peer_id: string;
      }>,
    ) => void;
    onPeerLeft?: (peerId: string) => void;
    onRemoteTrack?: (rt: {
      streamId: string;
      track: unknown;
      stream: unknown;
    }) => void;
    onRemoteMediaState?: (peerId: string, cameraOn: boolean) => void;
    onLocalStream?: (stream: unknown) => void;
    onVideoPublishState?: (state: "off" | "starting" | "live" | "failed") => void;
  } | null,
}));

// Fake SFU session: capture the consumer callbacks, resolve connect(), no-op
// every send. Only `type SfuRemoteTrack` is imported by the orchestrator (a
// compile-time-only import), so the value mock is sufficient.
vi.mock("./sfu-call-session", () => ({
  SfuCallSession: class {
    constructor(_config: unknown, callbacks: unknown) {
      hooks.captured = callbacks as typeof hooks.captured;
    }
    async connect(): Promise<void> {}
    async publish(): Promise<void> {}
    hangUp(): void {}
    rekey(): void {}
    setMicrophoneEnabled(): void {}
    setCameraEnabled(): void {}
    sendSignalRelayToCall(): void {}
    sendSignalRelayToPeer(): void {}
    setTurnProvider(): void {}
  },
}));

// wasm surface the `join` construction path touches (peer-tag derivation +
// KeyPackage wrap). The camera-state callbacks under test touch no wasm.
vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    wrapCallKeyPackage: (): Uint8Array => new Uint8Array([1, 2, 3]),
  }),
}));

// JWT mint without HTTP.
vi.mock("./call-jwt-client", () => ({
  CallJwtClient: class {
    async mintForCall(): Promise<{ jwt: string; turn: null }> {
      return { jwt: "test-jwt", turn: null };
    }
  },
}));

// `call_id` / `account_id` → hex, without pulling the real device-sync module.
vi.mock("../devices/sync/state", () => ({
  groupIdHex: (bytes: Uint8Array): string =>
    Array.from(bytes)
      .map((b) => b.toString(16).padStart(2, "0"))
      .join(""),
}));

import { MultiJoinerCall, type MultiJoinerCallOptions } from "./multi-joiner-call";
import { callActions, callState } from "../state/call-state";

// ---- Helpers ------------------------------------------------------------

function makeOpts(): MultiJoinerCallOptions {
  const identity = {
    signaturePublicKey: new Uint8Array(32).fill(7),
    createKeyPackage: (): unknown => ({}),
  };
  return {
    identity,
    callId: new Uint8Array(16).fill(1),
    ownerAccountId: new Uint8Array(16).fill(2),
    ownerSignatureKey: new Uint8Array(32).fill(3),
    sfuBaseUrl: "wss://sfu.test",
    media: { audio: true, video: false },
  } as unknown as MultiJoinerCallOptions;
}

/** A remote video track whose `ended` listener the test can fire, plus a
 *  controllable stream track list for the prune guard. */
function fakeRemote(
  streamId: string,
  kind: "video" | "audio",
  streamTracks: Array<{ readyState: "live" | "ended" }>,
): {
  rt: { streamId: string; track: unknown; stream: unknown };
  track: { readyState: "live" | "ended" };
  fireEnded: () => void;
} {
  const endedListeners: Array<() => void> = [];
  const track = {
    kind,
    readyState: "live" as "live" | "ended",
    addEventListener(type: string, cb: () => void): void {
      if (type === "ended") endedListeners.push(cb);
    },
  };
  const stream = {
    id: streamId,
    getTracks: (): Array<{ readyState: "live" | "ended" }> => streamTracks,
  };
  return {
    rt: { streamId, track, stream },
    track,
    fireEnded: (): void => {
      for (const cb of endedListeners) cb();
    },
  };
}

/** Camera flag of the `peer-<peerId>` roster tile (undefined if absent). */
function camOn(peerId: string): boolean | undefined {
  return callState.participants().find((p) => p.id === `peer-${peerId}`)?.camOn;
}

/** Build a one-remote roster (peerX) so the tiles exist to be flipped. */
function seedRosterWithPeerX(): void {
  hooks.captured?.onWelcome?.("self", [
    { peer_id: "peerX" },
  ]);
}

let call: MultiJoinerCall | null = null;

beforeEach(() => {
  hooks.captured = null;
  callActions.reset();
});

afterEach(() => {
  // hangUp clears the joiner admission timer + resets the store.
  call?.hangUp();
  call = null;
  vi.clearAllMocks();
});

// ---- onRemoteMediaState (camera-toggle mute) ----------------------------

describe("MultiJoinerCall — onRemoteMediaState flips the roster tile", () => {
  it("MediaState(on) then (off) flips the matching participant's camOn", async () => {
    call = await MultiJoinerCall.join(makeOpts());
    seedRosterWithPeerX();
    expect(camOn("peerX")).toBe(false);

    hooks.captured?.onRemoteMediaState?.("peerX", true);
    expect(camOn("peerX")).toBe(true);

    hooks.captured?.onRemoteMediaState?.("peerX", false);
    expect(camOn("peerX")).toBe(false);
  });

  it("a MediaState for an unknown peer is a harmless no-op", async () => {
    call = await MultiJoinerCall.join(makeOpts());
    seedRosterWithPeerX();

    hooks.captured?.onRemoteMediaState?.("ghost", true);
    expect(camOn("peerX")).toBe(false); // untouched
    expect(camOn("ghost")).toBeUndefined(); // no tile materialised
  });
});

// ---- onRemoteTrack `ended` (genuine unpublish / leave) ------------------

describe("MultiJoinerCall — onRemoteTrack ended clears camOn + prunes", () => {
  it("flips camOn on arrival, then clears + prunes when the track ends", async () => {
    call = await MultiJoinerCall.join(makeOpts());
    seedRosterWithPeerX();

    // The only track on the stream is this video track.
    const { rt, track, fireEnded } = fakeRemote("peerX", "video", []);
    // getTracks reflects the same track object so the prune guard sees it end.
    (rt.stream as { getTracks: () => unknown[] }).getTracks = () => [track];

    hooks.captured?.onRemoteTrack?.(rt);
    expect(camOn("peerX")).toBe(true);
    expect(call.remoteStreams().has("peerX")).toBe(true);

    // Genuine track-gone: no live track remains → clear camOn + prune entry.
    track.readyState = "ended";
    fireEnded();
    expect(camOn("peerX")).toBe(false);
    expect(call.remoteStreams().has("peerX")).toBe(false);
  });

  it("keeps the stream entry when another live track remains (camera-off)", async () => {
    call = await MultiJoinerCall.join(makeOpts());
    seedRosterWithPeerX();

    // A live audio track shares the stream; only the video track ends.
    const liveAudio = { readyState: "live" as "live" | "ended" };
    const { rt, track, fireEnded } = fakeRemote("peerX", "video", []);
    (rt.stream as { getTracks: () => unknown[] }).getTracks = () => [
      track,
      liveAudio,
    ];

    hooks.captured?.onRemoteTrack?.(rt);
    expect(camOn("peerX")).toBe(true);

    // Video ends but audio stays live → clear camOn, KEEP the entry.
    track.readyState = "ended";
    fireEnded();
    expect(camOn("peerX")).toBe(false);
    expect(call.remoteStreams().has("peerX")).toBe(true);
  });
});

// ---- Join-race guard (track-arrival vs media-state reorder) -------------

describe("MultiJoinerCall — camera-state join-race guard", () => {
  it("media-state(off) THEN track-arrival keeps camOn=false", async () => {
    // THE RACE: peerX is already camera-muted; its media-state(off) lands
    // FIRST, then its still-present (disabled) video track arrives at this
    // newcomer. The track arrival must NOT flip peerX back on.
    call = await MultiJoinerCall.join(makeOpts());
    seedRosterWithPeerX();

    hooks.captured?.onRemoteMediaState?.("peerX", false);
    expect(camOn("peerX")).toBe(false);

    const { rt } = fakeRemote("peerX", "video", []);
    hooks.captured?.onRemoteTrack?.(rt);
    expect(camOn("peerX")).toBe(false);
  });

  it("track-arrival THEN media-state(off) ends camOn=false", async () => {
    // The other order: the track arrives first (no sticky state → defaults on),
    // then the authoritative media-state(off) corrects it to false.
    call = await MultiJoinerCall.join(makeOpts());
    seedRosterWithPeerX();

    const { rt } = fakeRemote("peerX", "video", []);
    hooks.captured?.onRemoteTrack?.(rt);
    expect(camOn("peerX")).toBe(true);

    hooks.captured?.onRemoteMediaState?.("peerX", false);
    expect(camOn("peerX")).toBe(false);
  });

  it("track-arrival with no prior media-state defaults camOn=true", async () => {
    // The happy path: a genuinely camera-on peer whose track arrives with no
    // prior media-state → camOn=true.
    call = await MultiJoinerCall.join(makeOpts());
    seedRosterWithPeerX();

    const { rt } = fakeRemote("peerX", "video", []);
    hooks.captured?.onRemoteTrack?.(rt);
    expect(camOn("peerX")).toBe(true);
  });

  it("peer-left clears sticky state so a re-join defaults camOn=true", async () => {
    // A muted peer leaves; its sticky entry is cleared. A subsequent re-join's
    // track arrival must NOT inherit the stale "off" — it defaults on again.
    call = await MultiJoinerCall.join(makeOpts());
    seedRosterWithPeerX();

    hooks.captured?.onRemoteMediaState?.("peerX", false);
    hooks.captured?.onPeerLeft?.("peerX");

    // peerX re-joins (roster re-adds the tile) and its track arrives.
    seedRosterWithPeerX();
    const { rt } = fakeRemote("peerX", "video", []);
    hooks.captured?.onRemoteTrack?.(rt);
    expect(camOn("peerX")).toBe(true);
  });
});

// ---- Local media surfaced to the app-level call host --------------------
//
// The call host renders the link call's self-preview and camera button from
// these accessors; before the call moved there, the session's local-stream and
// camera-lifecycle callbacks were not wired at all.

describe("MultiJoinerCall — local stream + camera lifecycle for the call host", () => {
  it("surfaces the local capture stream and the camera lifecycle", async () => {
    call = await MultiJoinerCall.join(makeOpts());
    expect(call.localStream()).toBeNull();
    expect(call.videoPublishState()).toBe("off");

    const stream = { id: "local" };
    hooks.captured?.onLocalStream?.(stream);
    hooks.captured?.onVideoPublishState?.("live");
    expect(call.localStream()).toBe(stream);
    expect(call.videoPublishState()).toBe("live");
  });

  it("hangUp clears them", async () => {
    call = await MultiJoinerCall.join(makeOpts());
    hooks.captured?.onLocalStream?.({ id: "local" });
    hooks.captured?.onVideoPublishState?.("live");
    call.hangUp();
    expect(call.localStream()).toBeNull();
    expect(call.videoPublishState()).toBe("off");
  });
});

// ---- The light companion track (`light-stream.ts`) -----------------------

describe("MultiJoinerCall: the light companion track", () => {
  it("is kept under its peer's id and never becomes a participant, a sink or a camera flag", async () => {
    call = await MultiJoinerCall.join(makeOpts());
    seedRosterWithPeerX();
    const before = callState.participants().map((p) => p.id);

    const light = fakeRemote("peerX~light", "video", []);
    (light.rt.stream as { getTracks: () => unknown[] }).getTracks = () => [light.track];
    hooks.captured?.onRemoteTrack?.(light.rt);

    expect(callState.participants().map((p) => p.id)).toEqual(before);
    expect(camOn("peerX")).toBe(false);
    expect(call.remoteStreams().size).toBe(0);
    expect(call.remoteLightStreams().get("peerX")).toBe(light.rt.stream);
    expect(call.remoteLightStreamFor("peer-peerX")).toBe(light.rt.stream);
    expect(call.remoteStreamFor("peer-peerX")).toBeUndefined();
  });

  it("is pruned when its track ends and when its peer leaves", async () => {
    call = await MultiJoinerCall.join(makeOpts());
    seedRosterWithPeerX();

    const first = fakeRemote("peerX~light", "video", []);
    hooks.captured?.onRemoteTrack?.(first.rt);
    first.track.readyState = "ended";
    first.fireEnded();
    expect(call.remoteLightStreams().has("peerX")).toBe(false);

    const second = fakeRemote("peerX~light", "video", []);
    hooks.captured?.onRemoteTrack?.(second.rt);
    expect(call.remoteLightStreams().has("peerX")).toBe(true);
    hooks.captured?.onPeerLeft?.("peerX");
    expect(call.remoteLightStreams().has("peerX")).toBe(false);
  });
});
