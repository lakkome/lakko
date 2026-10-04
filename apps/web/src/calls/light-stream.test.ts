// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  LIGHT_STREAM_SUFFIX,
  lightStreamIdOf,
  splitStreamId,
  withLightStream,
  withoutLightStream,
} from "./light-stream";

describe("splitStreamId", () => {
  it("uses the suffix the SFU appends", () => {
    expect(LIGHT_STREAM_SUFFIX).toBe("~light");
  });

  it("reads a plain stream id as the peer's main stream", () => {
    expect(splitStreamId("peerX")).toEqual({ peerId: "peerX", light: false });
    expect(splitStreamId("0193a7c2-1f4e-7abc-9def-000000000001")).toEqual({
      peerId: "0193a7c2-1f4e-7abc-9def-000000000001",
      light: false,
    });
  });

  it("reads the suffixed id as that peer's light companion", () => {
    expect(splitStreamId("peerX~light")).toEqual({ peerId: "peerX", light: true });
    expect(splitStreamId(lightStreamIdOf("peerX"))).toEqual({ peerId: "peerX", light: true });
  });

  it("only strips the suffix at the end, and once", () => {
    expect(splitStreamId("peer~lightX")).toEqual({ peerId: "peer~lightX", light: false });
    expect(splitStreamId("peerX~light~light")).toEqual({ peerId: "peerX~light", light: true });
    expect(splitStreamId("peerX~LIGHT")).toEqual({ peerId: "peerX~LIGHT", light: false });
  });

  it("does not read the bare suffix or an empty id as a companion", () => {
    expect(splitStreamId("~light")).toEqual({ peerId: "~light", light: false });
    expect(splitStreamId("")).toEqual({ peerId: "", light: false });
  });
});

describe("the peer -> light stream map", () => {
  const a = { id: "a" };
  const b = { id: "b" };

  it("sets a peer's companion without touching the map it was given", () => {
    const empty = new Map<string, { id: string }>();
    const one = withLightStream(empty, "peerX", a);
    expect(empty.size).toBe(0);
    expect(one.get("peerX")).toBe(a);
    // The same stream again changes nothing (no reactive churn).
    expect(withLightStream(one, "peerX", a)).toBe(one);
    expect(withLightStream(one, "peerX", b).get("peerX")).toBe(b);
  });

  it("removes a peer's companion", () => {
    const one = withLightStream(new Map<string, { id: string }>(), "peerX", a);
    expect(withoutLightStream(one, "peerX").has("peerX")).toBe(false);
    expect(withoutLightStream(one, "peerY")).toBe(one);
  });

  it("does not let an older stream's end remove the newer one", () => {
    const replaced = withLightStream(
      withLightStream(new Map<string, { id: string }>(), "peerX", a),
      "peerX",
      b,
    );
    expect(withoutLightStream(replaced, "peerX", a)).toBe(replaced);
    expect(withoutLightStream(replaced, "peerX", b).has("peerX")).toBe(false);
  });
});
