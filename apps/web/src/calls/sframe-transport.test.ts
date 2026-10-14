// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which interface carries encoded frames to the SFrame worker
 * (`sframe-transport.ts`), decided from what the page exposes.
 *
 * No browser runs here. What is pinned is the decision and its two
 * consequences; whether a given engine behaves as its interface says is not
 * something this suite can see.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  sframePeerConnectionConfig,
  sframeTransport,
  sframeTransportLine,
} from "./sframe-transport";

class WithStreams {
  createEncodedStreams(): unknown {
    return {};
  }
}
class WithoutStreams {}
class ScriptTransform {}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("sframeTransport", () => {
  it("prefers RTCRtpScriptTransform, also where createEncodedStreams exists", () => {
    expect(sframeTransport({ RTCRtpScriptTransform: ScriptTransform })).toBe("script-transform");
    expect(
      sframeTransport({
        RTCRtpScriptTransform: ScriptTransform,
        RTCRtpSender: WithStreams,
        RTCRtpReceiver: WithStreams,
      }),
    ).toBe("script-transform");
  });

  it("takes the insertable-streams interface where only that exists", () => {
    expect(sframeTransport({ RTCRtpSender: WithStreams, RTCRtpReceiver: WithStreams })).toBe(
      "encoded-streams",
    );
  });

  it("is null where neither exists, or where only one side can take its streams", () => {
    expect(sframeTransport({})).toBeNull();
    expect(sframeTransport({ RTCRtpSender: WithoutStreams, RTCRtpReceiver: WithoutStreams })).toBeNull();
    expect(sframeTransport({ RTCRtpSender: WithStreams, RTCRtpReceiver: WithoutStreams })).toBeNull();
    expect(sframeTransport({ RTCRtpSender: WithoutStreams, RTCRtpReceiver: WithStreams })).toBeNull();
    expect(sframeTransport({ RTCRtpSender: WithStreams })).toBeNull();
    // Not a constructor: not taken for the interface.
    expect(sframeTransport({ RTCRtpScriptTransform: {} })).toBeNull();
  });

  it("reads the page's own globals by default", () => {
    expect(sframeTransport()).toBeNull();
    vi.stubGlobal("RTCRtpSender", WithStreams);
    vi.stubGlobal("RTCRtpReceiver", WithStreams);
    expect(sframeTransport()).toBe("encoded-streams");
    vi.stubGlobal("RTCRtpScriptTransform", ScriptTransform);
    expect(sframeTransport()).toBe("script-transform");
  });
});

describe("sframePeerConnectionConfig", () => {
  const base: RTCConfiguration = { bundlePolicy: "max-bundle", rtcpMuxPolicy: "require", iceServers: [] };

  it("adds encodedInsertableStreams on the insertable-streams path only", () => {
    const config = sframePeerConnectionConfig(base, "encoded-streams") as RTCConfiguration & {
      encodedInsertableStreams?: boolean;
    };
    expect(config.encodedInsertableStreams).toBe(true);
    expect(config.bundlePolicy).toBe("max-bundle");
    expect(config.rtcpMuxPolicy).toBe("require");
    expect(config.iceServers).toBe(base.iceServers);
    // The caller's object is not written to.
    expect("encodedInsertableStreams" in base).toBe(false);
  });

  it("leaves the configuration as it is on the script-transform path and with no transport", () => {
    expect(sframePeerConnectionConfig(base, "script-transform")).toBe(base);
    expect(sframePeerConnectionConfig(base, null)).toBe(base);
    expect("encodedInsertableStreams" in sframePeerConnectionConfig(base, "script-transform")).toBe(false);
  });

  it("follows the page's globals when no transport is stated", () => {
    expect(sframePeerConnectionConfig(base)).toBe(base);
    vi.stubGlobal("RTCRtpSender", WithStreams);
    vi.stubGlobal("RTCRtpReceiver", WithStreams);
    expect("encodedInsertableStreams" in sframePeerConnectionConfig(base)).toBe(true);
    vi.stubGlobal("RTCRtpScriptTransform", ScriptTransform);
    expect(sframePeerConnectionConfig(base)).toBe(base);
  });
});

describe("sframeTransportLine", () => {
  it("names the path in use", () => {
    expect(sframeTransportLine("script-transform")).toBe("sframe transport: script-transform");
    expect(sframeTransportLine("encoded-streams")).toBe("sframe transport: encoded-streams");
    expect(sframeTransportLine(null)).toBe("sframe transport: none");
  });
});
