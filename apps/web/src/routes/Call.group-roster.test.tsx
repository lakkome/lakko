// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The group call screen follows the roster, and can always be torn down.
 *
 * The defect this file is written against was seen on a live three-person
 * call: one person's browser closed, the store dropped them, and the screen
 * did not. Their tile stayed, the header kept counting three, the remaining
 * tile kept asking the SFU for the `middle` picture, and later the same
 * screen could not be dismissed when the call ended (for everyone, or by a
 * plain `End`): the session stayed up with the camera and microphone on.
 *
 * One cause: a tile is drawn under a `<Show>` keyed on the person being in
 * the roster, and its teardown read the person back through that `<Show>`'s
 * accessor. Solid refuses that read once the condition is false ("Stale read
 * from <Show>"), so the roster write threw half way through the update,
 * every pending effect of the screen was dropped for good, and the next
 * disposal of the tree threw at the same place.
 *
 * Nothing here is mocked but the browser facts jsdom does not have (layout
 * and a media stream): the component, the store and the shared rules are the
 * real ones, so the `main` asked of the last remaining person is the answer
 * of `lakko_ui_rules::tile_video_quality`, not of a stand-in.
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route } from "@solidjs/router";
import { createSignal, Show } from "solid-js";
import { render } from "solid-js/web";

import { callActions, callState, type Participant } from "../state/call-state";
import { all, initWasm, one, settle } from "../test/ui";
import { VideoWantsBook, type VideoQuality } from "../calls/video-wants";
import { groupCallTileSources, type TileSourceHandle } from "../calls/group-call-tile-sources";

type CallComponent = typeof import("./Call")["default"];
let Call: CallComponent;

/** The width every tile measures, in CSS pixels: wide enough that the rule
 *  answers `middle` for a grid tile of a three-person call. */
const TILE_WIDTH = 400;

beforeAll(async () => {
  // jsdom has no layout: give the call area a size and every tile a width.
  class FakeResizeObserver {
    constructor(private readonly cb: ResizeObserverCallback) {}
    observe(): void {
      this.cb(
        [{ contentRect: { width: 1200, height: 700 } } as ResizeObserverEntry],
        this as unknown as ResizeObserver,
      );
    }
    unobserve(): void {}
    disconnect(): void {}
  }
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    () => ({ width: TILE_WIDTH, height: 300, left: 0, top: 0, right: TILE_WIDTH, bottom: 300, x: 0, y: 0 }) as DOMRect,
  );
  // jsdom implements no media playback.
  vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(async () => {});
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
  await initWasm();
  Call = (await import("./Call")).default;
  await settle();
});

beforeEach(() => {
  callActions.reset();
});

function person(id: string, over: Partial<Participant> = {}): Participant {
  return {
    id,
    displayName: id,
    isLocal: false,
    micOn: true,
    camOn: true,
    quality: "good",
    speaking: false,
    ...over,
  };
}

/** A stream as far as the screen reads one. */
function fakeStream(): MediaStream {
  return {
    getTracks: () => [],
    getVideoTracks: () => [],
    getAudioTracks: () => [],
  } as unknown as MediaStream;
}

interface Mounted {
  /** What the tiles asked of the SFU, by peer id. */
  readonly book: VideoWantsBook;
  readonly exits: () => number;
  dispose(): void;
}

let mounted: Mounted | null = null;
afterEach(() => {
  try {
    mounted?.dispose();
  } catch {
    // A test that already failed on a throwing teardown says so itself.
  }
  mounted = null;
  document.body.replaceChildren();
});

/** A connected three-person group call on the embedded screen: this device,
 *  `b` and `c`. */
function mountGroupCall(): Mounted {
  const streams = new Map<string, MediaStream>([
    ["peer-b", fakeStream()],
    ["peer-c", fakeStream()],
  ]);
  callActions.setParticipants([
    person("local", { isLocal: true, camOn: false }),
    person("peer-b"),
    person("peer-c"),
  ]);
  callActions.setPhase({ kind: "connected" });
  const book = new VideoWantsBook();
  book.setPeers(["b", "c"]);
  const onExit = vi.fn();
  const host = document.createElement("div");
  document.body.appendChild(host);
  const dispose = render(
    () => (
      <MemoryRouter>
        <Route
          path="*"
          component={() => (
            <Call
              embedded={true}
              group={true}
              onExit={onExit}
              remoteStreamFor={(id) => streams.get(id)}
              reportTileVideoQuality={(tile, participantId, quality) => {
                book.setTile(tile, participantId.replace(/^peer-/, ""), quality);
              }}
            />
          )}
        />
      </MemoryRouter>
    ),
    host,
  );
  mounted = { book, exits: () => onExit.mock.calls.length, dispose };
  return mounted;
}

const TILE = ".lk-call-tile";

function tileNames(): string[] {
  return all(".lk-call-tile-name").map((el) => el.textContent ?? "");
}

function wants(book: VideoWantsBook): Record<string, VideoQuality> {
  return Object.fromEntries(book.wants());
}

describe("a person who left the call", () => {
  it("draws three tiles, counts three and asks for the middle picture while three are in it", async () => {
    const { book } = mountGroupCall();
    await settle();
    expect(tileNames().sort()).toEqual(["local", "peer-b", "peer-c"]);
    expect(one(".lk-call-status").textContent).toContain("3");
    expect(wants(book)).toEqual({ b: "middle", c: "middle" });
  });

  it("has no tile and is not counted once they are gone", async () => {
    mountGroupCall();
    await settle();
    expect(() => callActions.participantLeft("peer-c")).not.toThrow();
    await settle();
    expect(callState.participants().map((p) => p.id)).toEqual(["local", "peer-b"]);
    expect(tileNames().sort()).toEqual(["local", "peer-b"]);
    expect(all(TILE)).toHaveLength(2);
    expect(one(".lk-call-status").textContent).toContain("2");
    expect(one(".lk-call-status").textContent).not.toContain("3");
  });

  it("leaves the remaining person asked for the main picture", async () => {
    const { book } = mountGroupCall();
    await settle();
    callActions.participantLeft("peer-c");
    book.removePeer("c");
    await settle();
    // Two people in the call: the rule answers `main` whatever the width.
    expect(wants(book)).toEqual({ b: "main" });
  });

  it("gets one tile when they come back, not two", async () => {
    mountGroupCall();
    await settle();
    callActions.participantLeft("peer-c");
    callActions.participantJoined(person("peer-c"));
    // A join announced twice (a resent frame) is still one person.
    callActions.participantJoined(person("peer-c"));
    await settle();
    expect(tileNames().sort()).toEqual(["local", "peer-b", "peer-c"]);
    expect(one(".lk-call-status").textContent).toContain("3");
  });

  it("is dropped from the roster drawer too", async () => {
    mountGroupCall();
    await settle();
    callActions.setParticipantListOpen(true);
    await settle();
    callActions.participantLeft("peer-c");
    await settle();
    expect(callState.participants()).toHaveLength(2);
    expect(all(TILE)).toHaveLength(2);
  });
});

describe("the screen can always be taken down", () => {
  it("is disposed without a throw after a person left", async () => {
    const m = mountGroupCall();
    await settle();
    callActions.participantLeft("peer-c");
    await settle();
    expect(() => m.dispose()).not.toThrow();
    mounted = null;
    expect(all(TILE)).toHaveLength(0);
  });

  it("survives the store being emptied under it, as a hang-up does", async () => {
    mountGroupCall();
    await settle();
    callActions.participantLeft("peer-c");
    await settle();
    // `hangUp` of either call adapter resets the store while the screen may
    // still be mounted.
    expect(() => callActions.reset()).not.toThrow();
    await settle();
    expect(all(TILE)).toHaveLength(0);
  });

  it("shows the ended state and then asks to be closed, after a person left", async () => {
    const m = mountGroupCall();
    await settle();
    callActions.participantLeft("peer-c");
    await settle();
    vi.useFakeTimers();
    try {
      expect(() => callActions.endCall()).not.toThrow();
      expect(all(".lk-call-ended")).toHaveLength(1);
      expect(m.exits()).toBe(0);
      vi.advanceTimersByTime(2_000);
      expect(m.exits()).toBe(1);
    } finally {
      vi.useRealTimers();
    }
  });
});

// ---------------------------------------------------------------------------
// The call ending: the screen under the host's own wiring
// ---------------------------------------------------------------------------
//
// The second half of what was seen live. The starter ended the call for
// everyone; the other laptop showed "... ended the call for everyone" and
// stayed on the call screen with its camera and microphone on, and the
// starter's own screen showed `Call ended` and stayed too.
//
// The host leaves a group call in two steps (`CallHostContext.tsx`
// `releaseGroupSlot`): it clears the call's handle, which takes the screen
// down, and then hangs the session up. The screen threw while it was taken
// down, so the second step never ran. This mounts the screen the way the host
// does (under "is there a handle", fed by `groupCallTileSources`) and leaves
// the same way; `hangUp` standing in for the session's is the one stand-in.

interface HostedCall {
  readonly hangUps: () => number;
  readonly reports: Array<[string, VideoQuality | null]>;
  /** What `leaveGroupCall` does to the screen and the session. */
  leave(): void;
  dispose(): void;
}

function mountHostedCall(): HostedCall {
  const main = new Map<string, MediaStream>([
    ["b", fakeStream()],
    ["c", fakeStream()],
  ]);
  const light = new Map<string, MediaStream>([
    ["b", fakeStream()],
    ["c", fakeStream()],
  ]);
  const [mainStreams, setMainStreams] = createSignal<ReadonlyMap<string, MediaStream>>(main);
  const [lightStreams, setLightStreams] = createSignal<ReadonlyMap<string, MediaStream>>(light);
  const reports: Array<[string, VideoQuality | null]> = [];
  let hangUps = 0;
  const live: TileSourceHandle & { dropStreamsOf(peerId: string): void } = {
    remoteStreams: mainStreams,
    remoteLightStreams: lightStreams,
    remoteMiddleStreams: () => new Map(),
    remoteVideoGiven: () => new Map(),
    localStream: () => null,
    reportTileVideoQuality: (_tile, peerId, quality) => {
      reports.push([peerId, quality]);
    },
    dropStreamsOf: (peerId) => {
      setLightStreams((prev) => new Map([...prev].filter(([id]) => id !== peerId)));
      setMainStreams((prev) => new Map([...prev].filter(([id]) => id !== peerId)));
    },
  };
  const [handle, setHandle] = createSignal<typeof live | null>(live);
  hosted = live;
  callActions.setParticipants([
    person("local", { isLocal: true, camOn: false }),
    person("peer-b"),
    person("peer-c"),
  ]);
  callActions.setPhase({ kind: "connected" });
  const host = document.createElement("div");
  document.body.appendChild(host);
  const tiles = groupCallTileSources(handle);
  const dispose = render(
    () => (
      <MemoryRouter>
        <Route
          path="*"
          component={() => (
            <Show when={handle() !== null}>
              <Call
                embedded={true}
                group={true}
                remoteStreamFor={tiles.remoteStreamFor}
                remoteLightStreamFor={tiles.remoteLightStreamFor}
                remoteMiddleStreamFor={tiles.remoteMiddleStreamFor}
                remoteVideoGivenFor={tiles.remoteVideoGivenFor}
                reportTileVideoQuality={tiles.reportTileVideoQuality}
                localStream={tiles.localStream}
              />
            </Show>
          )}
        />
      </MemoryRouter>
    ),
    host,
  );
  const hosted_: HostedCall = {
    hangUps: () => hangUps,
    reports,
    leave: () => {
      // `releaseGroupSlot`: the handle first, the session second, and the
      // second whatever the first did.
      try {
        setHandle(null);
      } finally {
        hangUps += 1;
        // Either adapter's `hangUp` empties the shared store.
        callActions.reset();
      }
    },
    dispose,
  };
  mounted = { book: new VideoWantsBook(), exits: () => 0, dispose };
  return hosted_;
}

let hosted: { dropStreamsOf(peerId: string): void } | null = null;

const SCREEN = ".lk-call";

describe("leaving a group call takes the screen down and closes the session", () => {
  it("when nobody left before", async () => {
    const call = mountHostedCall();
    await settle();
    expect(all(TILE)).toHaveLength(3);
    expect(() => call.leave()).not.toThrow();
    expect(call.hangUps()).toBe(1);
    await settle();
    expect(all(TILE)).toHaveLength(0);
    expect(all(SCREEN)).toHaveLength(0);
  });

  it("when a person left earlier in the call", async () => {
    const call = mountHostedCall();
    await settle();
    // As the SFU adapter does on `peer-left`: their streams, then the roster.
    hosted?.dropStreamsOf("c");
    expect(() => callActions.participantLeft("peer-c")).not.toThrow();
    await settle();
    expect(all(TILE)).toHaveLength(2);
    expect(() => call.leave()).not.toThrow();
    expect(call.hangUps()).toBe(1);
    await settle();
    expect(all(TILE)).toHaveLength(0);
    expect(all(SCREEN)).toHaveLength(0);
  });

  it("when the call was ended for everyone (the ended phase comes first)", async () => {
    const call = mountHostedCall();
    await settle();
    hosted?.dropStreamsOf("c");
    callActions.participantLeft("peer-c");
    await settle();
    // Both sides of section 4.1.2 go through the same two steps: the store
    // moves to `ended`, and the host's ended-phase effect leaves.
    expect(() => callActions.endCall()).not.toThrow();
    expect(() => call.leave()).not.toThrow();
    expect(call.hangUps()).toBe(1);
    await settle();
    expect(all(SCREEN)).toHaveLength(0);
  });

  it("tells the call that no tile shows anyone any more", async () => {
    const call = mountHostedCall();
    await settle();
    call.reports.length = 0;
    call.leave();
    // The withdrawals are sent to a call that is already gone, and dropped.
    expect(call.reports).toEqual([]);
    expect(call.hangUps()).toBe(1);
  });

  it("when a person's streams end before the roster hears they left", async () => {
    mountHostedCall();
    await settle();
    expect(() => hosted?.dropStreamsOf("c")).not.toThrow();
    await settle();
    // Still in the roster, so still a tile; it draws no picture.
    expect(all(TILE)).toHaveLength(3);
    expect(() => callActions.participantLeft("peer-c")).not.toThrow();
    await settle();
    expect(all(TILE)).toHaveLength(2);
    expect(one(".lk-call-status").textContent).toContain("2");
  });
});
