// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the call screen asks a group call about a person's pictures
 * (`calls/group-call-tile-sources.ts`), and above all that it can still ask
 * after the call is over: the screen is taken down after the host cleared
 * the call's handle, and its tiles report on the way out.
 */

import { describe, expect, it } from "vitest";
import { createRoot, createSignal } from "solid-js";

import {
  groupCallTileSources,
  peerIdOfRosterId,
  type TileSourceHandle,
} from "./group-call-tile-sources";
import type { VideoQuality } from "./video-wants";

function stream(name: string): MediaStream {
  return { name } as unknown as MediaStream;
}

function handle(): TileSourceHandle & { reports: Array<[object, string, VideoQuality | null]> } {
  const reports: Array<[object, string, VideoQuality | null]> = [];
  return {
    reports,
    remoteStreams: () => new Map([["b", stream("b-main")]]),
    remoteLightStreams: () => new Map([["b", stream("b-light")]]),
    remoteMiddleStreams: () => new Map([["b", stream("b-mid")]]),
    remoteVideoGiven: () => new Map<string, VideoQuality>([["b", "middle"]]),
    localStream: () => stream("own"),
    reportTileVideoQuality: (tile, peerId, quality) => {
      reports.push([tile, peerId, quality]);
    },
  };
}

describe("peerIdOfRosterId", () => {
  it("reads the peer id out of a roster id, once", () => {
    expect(peerIdOfRosterId("peer-b")).toBe("b");
    expect(peerIdOfRosterId("peer-peer-b")).toBe("peer-b");
    expect(peerIdOfRosterId("local")).toBe("local");
  });
});

describe("the tile sources of a live call", () => {
  it("answer by roster id from the handle", () => {
    const h = handle();
    const src = groupCallTileSources(() => h);
    expect(src.remoteStreamFor("peer-b")).toEqual(stream("b-main"));
    expect(src.remoteLightStreamFor("peer-b")).toEqual(stream("b-light"));
    expect(src.remoteMiddleStreamFor("peer-b")).toEqual(stream("b-mid"));
    expect(src.remoteVideoGivenFor("peer-b")).toBe("middle");
    expect(src.remoteStreamFor("peer-c")).toBeUndefined();
    expect(src.localStream()).toEqual(stream("own"));
    expect(src.remoteStreamList()).toEqual([stream("b-main")]);
    const tile = {};
    src.reportTileVideoQuality(tile, "peer-b", "main");
    expect(h.reports).toEqual([[tile, "b", "main"]]);
  });
});

describe("the tile sources of a call that is over", () => {
  it("answer nothing and take no report, without a throw", () => {
    const src = groupCallTileSources(() => null);
    expect(src.remoteStreamFor("peer-b")).toBeUndefined();
    expect(src.remoteLightStreamFor("peer-b")).toBeUndefined();
    expect(src.remoteMiddleStreamFor("peer-b")).toBeUndefined();
    expect(src.remoteVideoGivenFor("peer-b")).toBeUndefined();
    expect(src.localStream()).toBeUndefined();
    expect(src.remoteStreamList()).toEqual([]);
    expect(() => src.reportTileVideoQuality({}, "peer-b", null)).not.toThrow();
  });

  it("follow the handle as the host clears it", () => {
    createRoot((dispose) => {
      const h = handle();
      const [current, setCurrent] = createSignal<TileSourceHandle | null>(h);
      const src = groupCallTileSources(current);
      expect(src.remoteStreamFor("peer-b")).toBeDefined();
      setCurrent(null);
      expect(src.remoteStreamFor("peer-b")).toBeUndefined();
      // The withdrawal a tile sends on its way out is dropped, not thrown.
      expect(() => src.reportTileVideoQuality({}, "peer-b", null)).not.toThrow();
      expect(h.reports).toEqual([]);
      dispose();
    });
  });
});
