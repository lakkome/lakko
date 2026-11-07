// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * camera-state JOIN-RACE guard for the in-chat SFU adapter {@link createSfuCallUi}.
 *
 * The race: in a group call, a newcomer subscribing to an already
 * camera-muted peer A still receives A's present-but-DISABLED video track, so
 * `addStream` (the `onRemoteTrack` handler) fires and — before the guard — set
 * A's roster `camOn=true` (a frozen / black tile). A's `onRemoteMediaState(off)`
 * re-announce also arrives, but the two are NOT ordered: if the track-add lands
 * AFTER the media-state, A was wrongly shown camera-on. The fix is an
 * adapter-local sticky `lastKnownCameraOn` map that the track-arrival path
 * defers to (`trackPresent ? (lastKnown ?? true) : false`), so a known-muted
 * peer is never flipped back on by a track arrival, cleared on peer-left.
 *
 * `sfu-media-state.test.ts` exercises the SAME adapter but keeps the REAL
 * `SfuCallSession` (to test its relay classification), and a track ARRIVAL only
 * fires off a live `RTCPeerConnection.ontrack` — absent under `deferPublish` in
 * node. So, like `multi-joiner-media-state.test.ts`, this file mocks the full
 * `SfuCallSession` (capturing the adapter's consumer callbacks) and drives them
 * in every arrival order against a REAL `createSfuCallUi` + the REAL
 * `callActions` store, pinning the deterministic result. Mirrors the native
 * `CameraJoinRaceTests` / `CameraJoinRaceTest`.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// ---- Mocks --------------------------------------------------------------

// The consumer callbacks the adapter passes to `new SfuCallSession(...)`,
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
  } | null,
}));

// Fake SFU session: capture the consumer callbacks, resolve connect(), no-op
// every send. Only compile-time-only types are imported by the adapter, so a
// value mock of the class is sufficient.
vi.mock("./sfu-call-session", () => ({
  SfuCallSession: class {
    constructor(_config: unknown, callbacks: unknown) {
      hooks.captured = callbacks as typeof hooks.captured;
    }
    async connect(): Promise<void> {}
    hangUp(): void {}
    rekey(): void {}
    setMicrophoneEnabled(): void {}
    setCameraEnabled(): void {}
    setOwnCallName(): void {}
    async publishVideo(): Promise<void> {}
    async switchAudioInput(): Promise<void> {}
    async switchVideoInput(): Promise<void> {}
  },
}));

import { createSfuCallUi, type SfuCallUiHandle } from "./sfu-call-ui";
import { callActions, callState } from "../state/call-state";

// ---- Helpers ------------------------------------------------------------

function baseConfig(): {
  sfuBaseUrl: string;
  jwt: string;
  seed: Uint8Array;
  localLeaf: number;
  media: { audio: boolean; video: boolean };
  deferPublish: boolean;
} {
  return {
    sfuBaseUrl: "wss://sfu.test",
    jwt: "jwt",
    seed: new Uint8Array(32),
    localLeaf: 0,
    media: { audio: true, video: false },
    deferPublish: true,
  };
}

/** A remote video track whose `ended` listener the test can fire, plus a
 *  controllable stream track list for the prune guard. */
function fakeRemote(streamId: string): {
  rt: { streamId: string; track: unknown; stream: unknown };
} {
  const track = {
    kind: "video" as const,
    readyState: "live" as "live" | "ended",
    addEventListener(_type: string, _cb: () => void): void {},
  };
  const stream = {
    id: streamId,
    getTracks: (): Array<{ readyState: "live" | "ended" }> => [track],
  };
  return { rt: { streamId, track, stream } };
}

/** Camera flag of the `peer-<peerId>` roster tile (undefined if absent). */
function camOn(peerId: string): boolean | undefined {
  return callState
    .participants()
    .find((p) => p.id === `peer-${peerId}`)?.camOn;
}

/** Build a one-remote roster (peerX) so the tile exists to be flipped. */
function seedRosterWithPeerX(): void {
  hooks.captured?.onWelcome?.("self", [
    { peer_id: "peerX" },
  ]);
}

let handle: SfuCallUiHandle | null = null;

beforeEach(() => {
  hooks.captured = null;
  callActions.reset();
});

afterEach(() => {
  handle?.hangUp();
  handle = null;
  vi.clearAllMocks();
});

// ---- The race -----------------------------------------------------------

describe("createSfuCallUi — camera-state join-race guard", () => {
  it("media-state(off) THEN track-arrival keeps camOn=false", () => {
    // THE RACE: peerX is already camera-muted; its media-state(off) lands
    // FIRST, then its still-present (disabled) video track arrives. The track
    // arrival must NOT flip peerX back on.
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();
    expect(camOn("peerX")).toBe(false);

    hooks.captured?.onRemoteMediaState?.("peerX", false);
    expect(camOn("peerX")).toBe(false);

    hooks.captured?.onRemoteTrack?.(fakeRemote("peerX").rt);
    expect(camOn("peerX")).toBe(false);
  });

  it("track-arrival THEN media-state(off) ends camOn=false", () => {
    // The other order: the track arrives first (no sticky state → defaults on),
    // then the authoritative media-state(off) corrects it to false.
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();

    hooks.captured?.onRemoteTrack?.(fakeRemote("peerX").rt);
    expect(camOn("peerX")).toBe(true);

    hooks.captured?.onRemoteMediaState?.("peerX", false);
    expect(camOn("peerX")).toBe(false);
  });

  it("track-arrival with no prior media-state defaults camOn=true", () => {
    // The happy path: a genuinely camera-on peer whose track arrives with no
    // prior media-state → camOn=true.
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();

    hooks.captured?.onRemoteTrack?.(fakeRemote("peerX").rt);
    expect(camOn("peerX")).toBe(true);
  });

  it("media-state(on) THEN track-arrival stays camOn=true", () => {
    // A known camera-ON peer's track arrival stays on (sticky state is true).
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();

    hooks.captured?.onRemoteMediaState?.("peerX", true);
    hooks.captured?.onRemoteTrack?.(fakeRemote("peerX").rt);
    expect(camOn("peerX")).toBe(true);
  });

  it("peer-left clears sticky state so a re-join defaults camOn=true", () => {
    // A muted peer leaves; its sticky entry is cleared. A subsequent re-join's
    // track arrival must NOT inherit the stale "off" — it defaults on again.
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();

    hooks.captured?.onRemoteMediaState?.("peerX", false);
    hooks.captured?.onPeerLeft?.("peerX");

    // peerX re-joins (roster re-adds the tile) and its track arrives.
    seedRosterWithPeerX();
    hooks.captured?.onRemoteTrack?.(fakeRemote("peerX").rt);
    expect(camOn("peerX")).toBe(true);
  });
});
