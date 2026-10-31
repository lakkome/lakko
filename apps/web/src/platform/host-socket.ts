// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The SFU signaling socket (ADR-0085 section 3). A WebSocket cannot ride the
 * app scheme, so with anti-censorship on the desktop the socket lives in the
 * host and the page reaches it with ordinary requests. The class has the shape
 * of the few `WebSocket` members `SfuSignalingClient` uses.
 *
 * The host dials the SFU it was configured with: nothing here names an
 * address, and a socket that cannot be opened through the tunnel fails. It
 * never falls back to the page's own `WebSocket` (invariant #4).
 */
import { getHostTunnel, HOST_TUNNEL_PATH, tunnelRoutesThroughHost } from "./host";

/** What `SfuSignalingClient` needs of a socket. */
export interface SfuSocket {
  readonly readyState: number;
  onopen: ((ev: Event) => void) | null;
  onmessage: ((ev: MessageEvent) => void) | null;
  onerror: ((ev: Event) => void) | null;
  onclose: ((ev: CloseEvent) => void) | null;
  send(data: string): void;
  close(code?: number, reason?: string): void;
}

const CONNECTING = 0;
const OPEN = 1;
const CLOSED = 3;

/** Where a `fetch` is made; a test replaces it. */
type Fetch = typeof fetch;

/** The socket held by the host. */
export class HostTunnelSocket implements SfuSocket {
  readyState = CONNECTING;
  onopen: ((ev: Event) => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onclose: ((ev: CloseEvent) => void) | null = null;

  private id: number | null = null;
  /** Frames go out one at a time: signaling order is meaningful and two
   *  requests in flight may arrive in either order. */
  private sending: Promise<unknown> = Promise.resolve();

  constructor(
    protocols: readonly string[],
    private readonly request: Fetch = (...args) => fetch(...args),
  ) {
    void this.run(protocols);
  }

  private async run(protocols: readonly string[]): Promise<void> {
    try {
      const res = await this.request(`${HOST_TUNNEL_PATH}/ws/open`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ subprotocols: protocols }),
      });
      if (!res.ok) throw new Error("open refused");
      const { id } = (await res.json()) as { id: number };
      if (this.readyState === CLOSED) {
        await this.post(id, "close");
        return;
      }
      this.id = id;
      this.readyState = OPEN;
      this.onopen?.(new Event("open"));
      await this.receive(id);
    } catch {
      this.fail();
    }
  }

  private async receive(id: number): Promise<void> {
    while (this.readyState === OPEN) {
      const res = await this.request(`${HOST_TUNNEL_PATH}/ws/${id}/recv`);
      if (!res.ok) throw new Error("recv refused");
      const { frames, closed } = (await res.json()) as { frames: string[]; closed: boolean };
      for (const data of frames) {
        if (this.readyState !== OPEN) return;
        this.onmessage?.(new MessageEvent("message", { data }));
      }
      if (closed) {
        this.finish(1006, "socket ended");
        return;
      }
    }
  }

  private post(id: number, verb: string, body: string | null = null): Promise<Response> {
    return this.request(`${HOST_TUNNEL_PATH}/ws/${id}/${verb}`, { method: "POST", body });
  }

  private fail(): void {
    if (this.readyState === CLOSED) return;
    this.onerror?.(new Event("error"));
    this.finish(1006, "tunnel");
  }

  private finish(code: number, reason: string): void {
    if (this.readyState === CLOSED) return;
    this.readyState = CLOSED;
    this.onclose?.(new CloseEvent("close", { code, reason }));
  }

  send(data: string): void {
    const id = this.id;
    if (id === null || this.readyState !== OPEN) throw new Error("socket is not open");
    this.sending = this.sending
      .then(() => this.post(id, "send", data))
      .then((res) => {
        if (!res.ok) throw new Error("send refused");
      })
      .catch(() => this.fail());
  }

  close(): void {
    if (this.readyState === CLOSED) return;
    this.readyState = CLOSED;
    const id = this.id;
    if (id !== null) void this.sending.then(() => this.post(id, "close")).catch(() => undefined);
  }
}

/** The desktop's socket to the SFU at `url`, through the host when the setting
 *  is on. The browser build does not come here: it opens its `WebSocket` itself. */
export async function openDesktopSfuSocket(url: string, protocols: string[]): Promise<SfuSocket> {
  if (tunnelRoutesThroughHost(await getHostTunnel())) {
    return new HostTunnelSocket(protocols);
  }
  return new WebSocket(url, protocols);
}
