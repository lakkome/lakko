// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  GENERIC_FRAME_DESCRIPTOR_URI,
  armGenericFrameDescriptor,
  type HeaderExtensionTransceiver,
  type NegotiableHeaderExtension,
} from "./frame-descriptor";

const DEPENDENCY_DESCRIPTOR_URI =
  "https://aomediacodec.github.io/av1-rtp-spec/#dependency-descriptor-rtp-header-extension";

function transceiver(
  kind: string,
  extensions: NegotiableHeaderExtension[] | null,
  opts: { throws?: boolean } = {},
): HeaderExtensionTransceiver & { readonly log: NegotiableHeaderExtension[][] } {
  const log: NegotiableHeaderExtension[][] = [];
  let current = extensions ?? [];
  const t: HeaderExtensionTransceiver & { readonly log: NegotiableHeaderExtension[][] } = {
    receiver: { track: { kind } },
    log,
  };
  if (extensions !== null) {
    t.getHeaderExtensionsToNegotiate = () => current;
    t.setHeaderExtensionsToNegotiate = (list) => {
      if (opts.throws === true) throw new Error("InvalidModificationError");
      log.push(list);
      current = list;
    };
  }
  return t;
}

const stopped: NegotiableHeaderExtension[] = [
  { uri: "urn:ietf:params:rtp-hdrext:sdes:mid", direction: "sendrecv" },
  { uri: GENERIC_FRAME_DESCRIPTOR_URI, direction: "stopped" },
  { uri: DEPENDENCY_DESCRIPTOR_URI, direction: "stopped" },
];

describe("armGenericFrameDescriptor", () => {
  it("turns the stopped descriptor on for a video transceiver and leaves the rest", () => {
    const video = transceiver("video", stopped);
    expect(armGenericFrameDescriptor([video])).toEqual({ armed: 1, unsupported: 0 });
    expect(video.log).toHaveLength(1);
    const set = video.log[0] ?? [];
    expect(set.find((h) => h.uri === GENERIC_FRAME_DESCRIPTOR_URI)?.direction).toBe("sendrecv");
    // The dependency descriptor stays stopped: with both on, libwebrtc sends
    // only the dependency descriptor, which the natives do not negotiate.
    expect(set.find((h) => h.uri === DEPENDENCY_DESCRIPTOR_URI)?.direction).toBe("stopped");
    expect(set.find((h) => h.uri.endsWith("sdes:mid"))?.direction).toBe("sendrecv");
  });

  it("does not touch an audio transceiver", () => {
    const audio = transceiver("audio", stopped);
    expect(armGenericFrameDescriptor([audio])).toEqual({ armed: 0, unsupported: 0 });
    expect(audio.log).toHaveLength(0);
  });

  it("is idempotent once the descriptor is already negotiated", () => {
    const video = transceiver("video", [
      { uri: GENERIC_FRAME_DESCRIPTOR_URI, direction: "sendrecv" },
    ]);
    expect(armGenericFrameDescriptor([video, video])).toEqual({ armed: 2, unsupported: 0 });
    expect(video.log).toHaveLength(0);
  });

  it("counts a browser without the API instead of failing", () => {
    const video = transceiver("video", null);
    expect(armGenericFrameDescriptor([video])).toEqual({ armed: 0, unsupported: 1 });
  });

  it("counts a browser that does not know the extension", () => {
    const video = transceiver("video", [
      { uri: "urn:ietf:params:rtp-hdrext:sdes:mid", direction: "sendrecv" },
    ]);
    expect(armGenericFrameDescriptor([video])).toEqual({ armed: 0, unsupported: 1 });
  });

  it("counts a browser that refuses the change", () => {
    const video = transceiver("video", stopped, { throws: true });
    expect(armGenericFrameDescriptor([video])).toEqual({ armed: 0, unsupported: 1 });
  });

  it("skips a transceiver whose receiver has no track yet", () => {
    const bare: HeaderExtensionTransceiver = { receiver: { track: null } };
    expect(armGenericFrameDescriptor([bare])).toEqual({ armed: 0, unsupported: 0 });
  });
});
