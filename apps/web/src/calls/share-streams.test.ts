// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The stream a received share is shown from: the newest picture and the
 * sound that came with it, never an earlier share's track left beside it.
 */
import { describe, expect, it } from "vitest";

import { ShareStreams } from "./share-streams";

interface FakeTrack {
  readonly kind: "audio" | "video";
  readonly readyState: string;
  end(): void;
  addEventListener(type: string, fn: () => void): void;
}

function track(kind: "audio" | "video"): FakeTrack {
  const listeners: (() => void)[] = [];
  return {
    kind,
    readyState: "live",
    end: () => {
      for (const fn of listeners) fn();
    },
    addEventListener: (type, fn) => {
      if (type === "ended") listeners.push(fn);
    },
  };
}

function setup(): { streams: ShareStreams; shown: Map<string, MediaStream> } {
  const shown = new Map<string, MediaStream>();
  const streams = new ShareStreams(
    (peerId, stream, only) => {
      if (stream === null) {
        if (only === undefined || shown.get(peerId) === only) shown.delete(peerId);
      } else {
        shown.set(peerId, stream);
      }
    },
    (tracks) => ({ getTracks: () => tracks }) as unknown as MediaStream,
  );
  return { streams, shown };
}

const add = (s: ShareStreams, peer: string, t: FakeTrack): void => s.add(peer, t as unknown as MediaStreamTrack);
const tracksOf = (m: Map<string, MediaStream>, peer: string): unknown[] | undefined => m.get(peer)?.getTracks();

describe("ShareStreams", () => {
  it("shows a picture and the sound that came with it", () => {
    const { streams, shown } = setup();
    const v = track("video");
    const a = track("audio");
    add(streams, "p", v);
    add(streams, "p", a);
    expect(tracksOf(shown, "p")).toEqual([v, a]);
  });

  it("a new picture is a new share: the old picture and its sound go", () => {
    const { streams, shown } = setup();
    const v1 = track("video");
    add(streams, "p", v1);
    add(streams, "p", track("audio"));
    const v2 = track("video");
    add(streams, "p", v2);
    expect(tracksOf(shown, "p")).toEqual([v2]);
  });

  it("hands over a new stream object at every change, so the tile resets", () => {
    const { streams, shown } = setup();
    add(streams, "p", track("video"));
    const before = shown.get("p");
    add(streams, "p", track("video"));
    expect(shown.get("p")).not.toBe(before);
  });

  it("an old picture ending does not take the new share with it", () => {
    const { streams, shown } = setup();
    const v1 = track("video");
    add(streams, "p", v1);
    const v2 = track("video");
    add(streams, "p", v2);
    v1.end();
    expect(tracksOf(shown, "p")).toEqual([v2]);
  });

  it("the shown picture ending ends the share", () => {
    const { streams, shown } = setup();
    const v = track("video");
    add(streams, "p", v);
    v.end();
    expect(shown.has("p")).toBe(false);
  });

  it("the sound ending keeps the picture", () => {
    const { streams, shown } = setup();
    const v = track("video");
    const a = track("audio");
    add(streams, "p", v);
    add(streams, "p", a);
    a.end();
    expect(tracksOf(shown, "p")).toEqual([v]);
  });

  it("a sharer who leaves takes the share", () => {
    const { streams, shown } = setup();
    add(streams, "p", track("video"));
    streams.drop("p");
    expect(shown.has("p")).toBe(false);
    expect(streams.streamOf("p")).toBeUndefined();
  });
});
