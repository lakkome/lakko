// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which tracks are a share's, how a one-to-one share is named in the SDP it
 * leaves under, and that this client's constants are the core's (ADR-0083).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasmNs from "../../wasm/lakko_ffi_wasm";
import { SCREEN_STREAM_SUFFIX, screenStreamPeerOf, withScreenStreamId } from "./screen-share-tracks";
import { splitStreamId } from "./light-stream";
import { SFRAME_STREAM_SCREEN_AUDIO, SFRAME_STREAM_SCREEN_SHARE } from "./sframe-transform";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("the constants", () => {
  it("are the core's", () => {
    expect(SCREEN_STREAM_SUFFIX).toBe(wasmNs.screenStreamSuffix());
    expect(SFRAME_STREAM_SCREEN_SHARE).toBe(wasmNs.sframeStreamScreen());
    expect(SFRAME_STREAM_SCREEN_AUDIO).toBe(wasmNs.sframeStreamScreenAudio());
  });
});

describe("screenStreamPeerOf", () => {
  it("names the peer of a share stream and nothing else", () => {
    expect(screenStreamPeerOf("abc~screen")).toBe("abc");
    expect(screenStreamPeerOf("abc")).toBeNull();
    expect(screenStreamPeerOf("abc~light")).toBeNull();
    expect(screenStreamPeerOf("~screen")).toBeNull();
    expect(screenStreamPeerOf("")).toBeNull();
  });

  it("is asked first because splitStreamId reads a share as a peer's main stream", () => {
    // The reason every caller checks the share first.
    expect(splitStreamId("abc~screen")).toEqual({ peerId: "abc~screen", role: "main" });
  });
});

describe("withScreenStreamId", () => {
  const id = "3f1c2a4b-5d6e-4f70-8a9b-0c1d2e3f4a5b";
  const sdp = [
    "v=0",
    "m=audio 9 UDP/TLS/RTP/SAVPF 111",
    "a=msid:camera-stream-id audio-track",
    "m=video 9 UDP/TLS/RTP/SAVPF 96",
    `a=msid:${id} screen-video-track`,
    `a=ssrc:1234 msid:${id} screen-video-track`,
    "m=audio 9 UDP/TLS/RTP/SAVPF 111",
    `a=msid:${id} screen-audio-track`,
    "",
  ].join("\r\n");

  it("names every occurrence of the share's stream id as a share", () => {
    const out = withScreenStreamId(sdp, id);
    expect(out.split(`${id}~screen`).length - 1).toBe(3);
    expect(out).toContain("a=msid:camera-stream-id audio-track");
  });

  it("is idempotent", () => {
    const once = withScreenStreamId(sdp, id);
    expect(withScreenStreamId(once, id)).toBe(once);
  });

  it("leaves the SDP alone with no share", () => {
    expect(withScreenStreamId(sdp, null)).toBe(sdp);
    expect(withScreenStreamId(sdp, "")).toBe(sdp);
  });

  it("makes the receiver read the stream as a share", () => {
    const out = withScreenStreamId(sdp, id);
    const msid = /a=msid:(\S+) screen-video-track/.exec(out)?.[1];
    expect(msid).toBeDefined();
    expect(screenStreamPeerOf(msid ?? "")).toBe(id);
  });
});
