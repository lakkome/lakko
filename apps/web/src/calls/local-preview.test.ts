// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Binding the own-camera element (`local-preview.ts`) without asking the
 * browser engine for anything it may not do. Stand-ins only: no DOM.
 */
import { describe, expect, it } from "vitest";

import {
  describeLocalCamera,
  describeLocalPreview,
  describeRemotePicture,
  describeTrackState,
  LocalPreviewBinder,
  type PreviewElement,
} from "./local-preview";

interface Track {
  readonly id: string;
  readyState: string;
  muted: boolean;
  enabled: boolean;
}
interface Stream {
  readonly of: Track;
}

const track = (id: string): Track => ({ id, readyState: "live", muted: false, enabled: true });

/** The call's one stream, whose camera track is swapped inside it. */
function callStream(...tracks: Track[]): { tracks: Track[]; getVideoTracks(): Track[] } {
  return { tracks, getVideoTracks: () => tracks };
}

interface FakeEl extends PreviewElement<Stream> {
  paused: boolean;
  isConnected: boolean;
  plays: number;
}

/** An element that starts playing only when `play()` is called, as an engine
 *  may; `refuse` makes `play()` reject. */
function element(opts: { connected?: boolean; refuse?: boolean } = {}): FakeEl {
  const el: FakeEl = {
    srcObject: null,
    paused: true,
    isConnected: opts.connected ?? true,
    plays: 0,
    play(): Promise<void> {
      el.plays += 1;
      if (opts.refuse === true) return Promise.reject(new Error("not allowed"));
      el.paused = false;
      return Promise.resolve();
    },
  };
  return el;
}

const binder = (): LocalPreviewBinder<Track, Stream> => new LocalPreviewBinder((of) => ({ of }));

describe("LocalPreviewBinder", () => {
  it("binds a stream of its own made from the camera track, and calls play", () => {
    const b = binder();
    const el = element();
    const cam = track("cam");
    expect(b.sync(el, callStream(cam))).toBe(true);
    expect(el.srcObject).toEqual({ of: cam });
    expect(el.plays).toBe(1);
    expect(el.paused).toBe(false);
    // Nothing changed: nothing is done again.
    expect(b.sync(el, callStream(cam))).toBe(false);
    expect(el.plays).toBe(1);
    expect(b.counts()).toEqual({ binds: 1, playCalls: 1, playRefusals: 0 });
  });

  it("does not bind an element that is not in the document yet, and binds it on the next look", () => {
    const b = binder();
    const el = element({ connected: false });
    const source = callStream(track("cam"));
    expect(b.sync(el, source)).toBe(false);
    expect(el.srcObject).toBeNull();
    expect(el.plays).toBe(0);
    el.isConnected = true;
    expect(b.sync(el, source)).toBe(true);
    expect(el.plays).toBe(1);
  });

  it("gives the element a new stream when the camera track inside the call's stream is another", () => {
    const b = binder();
    const el = element();
    const source = callStream(track("front"));
    b.sync(el, source);
    const first = el.srcObject;
    // The camera is flipped: same stream object, another track in it.
    const back = track("back");
    source.tracks.splice(0, 1, back);
    expect(b.sync(el, source)).toBe(true);
    expect(el.srcObject).not.toBe(first);
    expect(el.srcObject).toEqual({ of: back });
    expect(el.plays).toBe(2);
  });

  it("binds when the camera arrives in a stream that had none (camera turned on mid-call)", () => {
    const b = binder();
    const el = element();
    const source = callStream();
    expect(b.sync(el, source)).toBe(false);
    expect(el.plays).toBe(0);
    const cam = track("cam");
    source.tracks.push(cam);
    expect(b.sync(el, source)).toBe(true);
    expect(el.srcObject).toEqual({ of: cam });
  });

  it("unbinds when the camera goes away or there is no stream", () => {
    const b = binder();
    const el = element();
    b.sync(el, callStream(track("cam")));
    expect(b.sync(el, null)).toBe(true);
    expect(el.srcObject).toBeNull();
    expect(b.track()).toBeNull();
    expect(b.sync(el, undefined)).toBe(false);
  });

  it("calls play again on each look while the element is paused, and counts refusals", async () => {
    const b = binder();
    const el = element({ refuse: true });
    const source = callStream(track("cam"));
    b.sync(el, source);
    b.sync(el, source);
    b.sync(el, source);
    expect(el.plays).toBe(3);
    await Promise.resolve();
    await Promise.resolve();
    expect(b.counts()).toEqual({ binds: 1, playCalls: 3, playRefusals: 3 });
  });

  it("survives an element whose play throws or does not exist", () => {
    const b = binder();
    const throwing: PreviewElement<Stream> = {
      srcObject: null,
      paused: true,
      play: () => {
        throw new Error("no");
      },
    };
    expect(b.sync(throwing, callStream(track("cam")))).toBe(true);
    expect(b.counts().playRefusals).toBe(1);
    const bare: PreviewElement<Stream> = { srcObject: null };
    expect(binder().sync(bare, callStream(track("cam")))).toBe(true);
  });

  it("binds again from nothing after a reset", () => {
    const b = binder();
    const source = callStream(track("cam"));
    b.sync(element(), source);
    b.reset();
    const next = element();
    expect(b.sync(next, source)).toBe(true);
    expect(next.plays).toBe(1);
  });
});

describe("the diagnostic lines", () => {
  it("says a track's state, and - for what is absent", () => {
    expect(describeTrackState(track("a"))).toBe("ready=live muted=false enabled=true");
    expect(describeTrackState(null)).toBe("ready=- muted=- enabled=-");
    expect(describeTrackState({})).toBe("ready=- muted=- enabled=-");
  });

  it("says the element and its track in one line", () => {
    const el: PreviewElement<unknown> = { srcObject: null, readyState: 0, paused: true, videoWidth: 0 };
    const muted = { ...track("a"), muted: true };
    expect(describeLocalPreview("tile", el, muted, { binds: 1, playCalls: 4, playRefusals: 4 })).toBe(
      "call local preview tile: el_ready=0 paused=true width=0 binds=1 play_calls=4 play_refused=4" +
        " track ready=live muted=true enabled=true",
    );
  });

  it("says the camera and each copy that is sent", () => {
    const ended = { ...track("m"), readyState: "ended" };
    expect(
      describeLocalCamera(track("main"), [
        ["light", track("l")],
        ["middle", ended],
      ]),
    ).toBe(
      "call local camera: main ready=live muted=false enabled=true" +
        " | light ready=live muted=false enabled=true | middle ready=ended muted=false enabled=true",
    );
    expect(describeLocalCamera(null, [["light", undefined]])).toBe(
      "call local camera: main ready=- muted=- enabled=- | light ready=- muted=- enabled=-",
    );
  });
});

describe("describeRemotePicture", () => {
  it("says how the element plays and where it is drawn", () => {
    const el = { srcObject: null, paused: false, readyState: 4, videoWidth: 640 };
    const line = describeRemotePicture(
      "peer-ab heavy",
      el,
      track("t1"),
      { binds: 1, playCalls: 1, playRefusals: 0 },
      { width: 320.4, height: 179.6, opacity: "1", zIndex: "1", visibility: "visible", display: "block", shown: "true" },
    );
    expect(line).toBe(
      "call remote picture peer-ab heavy: el_ready=4 paused=false width=640 binds=1 play_calls=1 play_refused=0" +
        " track ready=live muted=false enabled=true box=320x180 opacity=1 z=1 visibility=visible display=block shown=true",
    );
  });
});
