// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import { HostTunnelSocket } from "./host-socket";
import { parseTunnelState, tunnelRoutesThroughHost } from "./host";

/** A host that answers the socket routes from a script. */
function host(script: { open?: Response; frames?: string[][] } = {}) {
  const calls: Array<{ url: string; body: unknown }> = [];
  const batches = [...(script.frames ?? [])];
  let release: (() => void) | null = null;
  const request = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
    const path = String(url);
    calls.push({ url: path, body: init?.body });
    if (path.endsWith("/ws/open")) {
      return script.open ?? Response.json({ id: 7 });
    }
    if (path.endsWith("/recv")) {
      const frames = batches.shift();
      if (frames !== undefined) return Response.json({ frames, closed: false });
      await new Promise<void>((resolve) => {
        release = resolve;
      });
      return Response.json({ frames: [], closed: true });
    }
    return new Response(null, { status: 204 });
  }) as unknown as typeof fetch;
  return { request, calls, end: () => release?.() };
}

const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

describe("HostTunnelSocket", () => {
  it("opens with the offered subprotocols and names no address", async () => {
    const h = host();
    const socket = new HostTunnelSocket(["lakko-sfu-v1", "lakko-sfu-token.x"], h.request);
    const opened = vi.fn();
    socket.onopen = opened;
    await settle();
    expect(opened).toHaveBeenCalledOnce();
    expect(socket.readyState).toBe(1);
    expect(h.calls[0]?.url).toBe("/__host/tunnel/ws/open");
    expect(JSON.parse(String(h.calls[0]?.body))).toEqual({
      subprotocols: ["lakko-sfu-v1", "lakko-sfu-token.x"],
    });
    h.end();
  });

  it("delivers frames in order and sees the socket end", async () => {
    const h = host({ frames: [["a", "b"], ["c"]] });
    const socket = new HostTunnelSocket(["p"], h.request);
    const got: string[] = [];
    socket.onmessage = (ev) => got.push(String(ev.data));
    const closed = vi.fn();
    socket.onclose = closed;
    await settle();
    expect(got).toEqual(["a", "b", "c"]);
    expect(closed).not.toHaveBeenCalled();
    h.end();
    await settle();
    expect(closed).toHaveBeenCalledOnce();
    expect(socket.readyState).toBe(3);
  });

  it("sends one frame at a time, in order", async () => {
    const h = host();
    const socket = new HostTunnelSocket(["p"], h.request);
    await settle();
    socket.send("first");
    socket.send("second");
    await settle();
    const sent = h.calls.filter((c) => c.url.endsWith("/send")).map((c) => c.body);
    expect(sent).toEqual(["first", "second"]);
    h.end();
  });

  it("fails loudly when the tunnel refuses, and goes nowhere else", async () => {
    const h = host({ open: Response.json({ error: "tunnel-unavailable" }, { status: 503 }) });
    const socket = new HostTunnelSocket(["p"], h.request);
    const errored = vi.fn();
    const closed = vi.fn();
    socket.onerror = errored;
    socket.onclose = closed;
    await settle();
    expect(errored).toHaveBeenCalledOnce();
    expect(closed).toHaveBeenCalledOnce();
    expect(h.calls).toHaveLength(1);
  });

  it("closes a socket that opened after the caller left", async () => {
    const h = host();
    const socket = new HostTunnelSocket(["p"], h.request);
    socket.close();
    await settle();
    expect(h.calls.map((c) => c.url)).toEqual(["/__host/tunnel/ws/open", "/__host/tunnel/ws/7/close"]);
  });
});

describe("tunnelRoutesThroughHost", () => {
  it("keeps every switched-on phase off the direct path except a build with no pin", () => {
    expect(tunnelRoutesThroughHost({ enabled: true, phase: "armed" })).toBe(true);
    expect(tunnelRoutesThroughHost({ enabled: true, phase: "arming" })).toBe(true);
    expect(tunnelRoutesThroughHost({ enabled: true, phase: "unavailable" })).toBe(true);
    expect(tunnelRoutesThroughHost({ enabled: true, phase: "unconfigured" })).toBe(false);
    expect(tunnelRoutesThroughHost({ enabled: false, phase: "disabled" })).toBe(false);
  });

  it("refuses a state it cannot read", () => {
    expect(parseTunnelState({ enabled: true, phase: "armed" }).phase).toBe("armed");
    expect(() => parseTunnelState({ enabled: true, phase: "other" })).toThrow();
    expect(() => parseTunnelState({ phase: "armed" })).toThrow();
    expect(() => parseTunnelState(null)).toThrow();
  });
});
