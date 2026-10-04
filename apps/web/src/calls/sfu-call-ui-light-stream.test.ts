// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The light companion track in the in-chat SFU adapter {@link createSfuCallUi}.
 *
 * A simulcast publisher's light layer arrives as a second video track whose
 * MSID stream id is the peer id plus `~light` (`light-stream.ts`). It is a
 * second picture of a person already in the call: it must never become a
 * participant, a tile, an audio sink (an entry in `remoteStreams`) or a camera
 * flag, and it goes when its track ends and when its peer leaves.
 *
 * Same harness as `sfu-call-ui-join-race.test.ts`: the session is mocked, its
 * consumer callbacks are captured and driven against the real adapter and the
 * real `callActions` store.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const hooks = vi.hoisted(() => ({
  captured: null as {
    onWelcome?: (
      selfPeerId: string,
      roster: ReadonlyArray<{ peer_id: string; account_pseudonym: string; peer_tag?: string }>,
    ) => void;
    onPeerLeft?: (peerId: string) => void;
    onRemoteTrack?: (rt: { streamId: string; track: unknown; stream: unknown }) => void;
    onRemoteMediaState?: (peerId: string, cameraOn: boolean) => void;
  } | null,
}));

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
    async publishVideo(): Promise<void> {}
    async switchAudioInput(): Promise<void> {}
    async switchVideoInput(): Promise<void> {}
  },
}));

import { createSfuCallUi, type SfuCallUiHandle } from "./sfu-call-ui";
import { callActions, callState } from "../state/call-state";

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

/** A remote track whose `ended` listeners the test can fire. */
function fakeRemote(
  streamId: string,
  kind: "audio" | "video" = "video",
): {
  rt: { streamId: string; track: unknown; stream: unknown };
  stream: unknown;
  end: () => void;
} {
  const listeners: Array<() => void> = [];
  const track = {
    kind,
    readyState: "live" as "live" | "ended",
    addEventListener(type: string, cb: () => void): void {
      if (type === "ended") listeners.push(cb);
    },
  };
  const stream = { id: streamId, getTracks: (): unknown[] => [track] };
  return {
    rt: { streamId, track, stream },
    stream,
    end: (): void => {
      track.readyState = "ended";
      for (const cb of listeners) cb();
    },
  };
}

const ids = (): string[] => callState.participants().map((p) => p.id);
const camOn = (peerId: string): boolean | undefined =>
  callState.participants().find((p) => p.id === `peer-${peerId}`)?.camOn;

function seedRosterWithPeerX(): void {
  hooks.captured?.onWelcome?.("self", [{ peer_id: "peerX", account_pseudonym: "abcdef0123" }]);
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

describe("createSfuCallUi: the light companion track", () => {
  it("is kept beside the main stream, under its peer's id", () => {
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();
    const main = fakeRemote("peerX");
    const light = fakeRemote("peerX~light");
    hooks.captured?.onRemoteTrack?.(main.rt);
    hooks.captured?.onRemoteTrack?.(light.rt);

    expect(handle.remoteStreams().get("peerX")).toBe(main.stream);
    expect(handle.remoteLightStreams().get("peerX")).toBe(light.stream);
  });

  it("never creates a participant, a tile or an audio sink", () => {
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();
    const before = ids();
    expect(before).toEqual(["local", "peer-peerX"]);

    hooks.captured?.onRemoteTrack?.(fakeRemote("peerX~light").rt);

    // The roster is what it was: no `peer-peerX~light`.
    expect(ids()).toEqual(before);
    // `remoteStreams` is what the view mounts audio sinks and tiles from.
    expect(handle.remoteStreams().size).toBe(0);
    expect([...handle.remoteStreams().keys()]).not.toContain("peerX~light");
  });

  it("does not flip the peer's camera flag by itself", () => {
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();
    expect(camOn("peerX")).toBe(false);

    const light = fakeRemote("peerX~light");
    hooks.captured?.onRemoteTrack?.(light.rt);
    expect(camOn("peerX")).toBe(false);

    // Nor does it clear a camera the main track turned on, when it ends.
    hooks.captured?.onRemoteTrack?.(fakeRemote("peerX").rt);
    expect(camOn("peerX")).toBe(true);
    light.end();
    expect(camOn("peerX")).toBe(true);
  });

  it("does not undo a known camera-off state", () => {
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();
    hooks.captured?.onRemoteMediaState?.("peerX", false);
    hooks.captured?.onRemoteTrack?.(fakeRemote("peerX~light").rt);
    expect(camOn("peerX")).toBe(false);
  });

  it("is ignored when it is not video", () => {
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();
    hooks.captured?.onRemoteTrack?.(fakeRemote("peerX~light", "audio").rt);
    expect(handle.remoteLightStreams().size).toBe(0);
    expect(handle.remoteStreams().size).toBe(0);
  });

  it("is pruned when its track ends, leaving the main stream", () => {
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();
    const main = fakeRemote("peerX");
    const light = fakeRemote("peerX~light");
    hooks.captured?.onRemoteTrack?.(main.rt);
    hooks.captured?.onRemoteTrack?.(light.rt);

    light.end();
    expect(handle.remoteLightStreams().has("peerX")).toBe(false);
    expect(handle.remoteStreams().get("peerX")).toBe(main.stream);
  });

  it("is not pruned by the end of an older companion it replaced", () => {
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();
    const first = fakeRemote("peerX~light");
    const second = fakeRemote("peerX~light");
    hooks.captured?.onRemoteTrack?.(first.rt);
    hooks.captured?.onRemoteTrack?.(second.rt);
    first.end();
    expect(handle.remoteLightStreams().get("peerX")).toBe(second.stream);
  });

  it("is pruned with its peer", () => {
    handle = createSfuCallUi(baseConfig());
    hooks.captured?.onWelcome?.("self", [
      { peer_id: "peerX", account_pseudonym: "abcdef0123" },
      { peer_id: "peerY", account_pseudonym: "0123abcdef" },
    ]);
    const lightY = fakeRemote("peerY~light");
    hooks.captured?.onRemoteTrack?.(fakeRemote("peerX~light").rt);
    hooks.captured?.onRemoteTrack?.(lightY.rt);

    hooks.captured?.onPeerLeft?.("peerX");
    expect(handle.remoteLightStreams().has("peerX")).toBe(false);
    expect(handle.remoteLightStreams().get("peerY")).toBe(lightY.stream);
  });

  it("is dropped on hang-up", () => {
    handle = createSfuCallUi(baseConfig());
    seedRosterWithPeerX();
    hooks.captured?.onRemoteTrack?.(fakeRemote("peerX~light").rt);
    handle.hangUp();
    expect(handle.remoteLightStreams().size).toBe(0);
  });
});
