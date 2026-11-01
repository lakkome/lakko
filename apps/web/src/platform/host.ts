// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The desktop host's app routes (ADR-0085 section 4, `docs/ux/desktop.md`
 * sections 2, 3 and 3.1).
 *
 * The page talks to the Tauri host with `fetch` on its own origin, under
 * {@link HOST_APP_PATH}; the host talks to the page with a `lakko:host` DOM
 * event on `window`. Every function here is a no-op (or a refusal) outside
 * the desktop app, so a browser build never sends a host request.
 */
import { isDesktop } from "./index";

/** Path prefix of the host's app routes. */
export const HOST_APP_PATH = "/__host/app";
export const HOST_TUNNEL_PATH = "/__host/tunnel";

/** The DOM event the host dispatches on `window`. */
export const HOST_EVENT = "lakko:host";

/** What the host can tell the page. */
export type HostEventKind = "quit-requested" | "open-settings" | "open-target" | "still-running" | "link-arrived";

/** The system page for one device (`docs/ux/desktop.md` section 4.2). */
export type PrivacyPage = "microphone" | "camera";

/** The desktop-only settings the host keeps on this computer. */
export interface HostSettings {
  keepRunning: boolean;
  openAtLogin: boolean;
}

/** What the page tells the host. */
export interface HostAppState {
  callLive: boolean;
  unread: number;
}

/** Thrown when a host request fails, with the HTTP status (0: not desktop). */
export class HostRequestError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "HostRequestError";
    this.status = status;
  }
}

function parseSettings(v: unknown): HostSettings {
  const o = (typeof v === "object" && v !== null ? v : {}) as Record<string, unknown>;
  return {
    keepRunning: typeof o.keepRunning === "boolean" ? o.keepRunning : true,
    openAtLogin: typeof o.openAtLogin === "boolean" ? o.openAtLogin : false,
  };
}

function requireDesktop(): void {
  if (!isDesktop()) throw new HostRequestError(0, "not the desktop app");
}

/** Anti-censorship as the host holds it (`docs/ux/desktop.md` section 9). */
export type HostTunnelPhase = "disabled" | "unconfigured" | "arming" | "armed" | "unavailable";

export interface HostTunnelState {
  readonly enabled: boolean;
  readonly phase: HostTunnelPhase;
}

const TUNNEL_PHASES: readonly HostTunnelPhase[] = [
  "disabled",
  "unconfigured",
  "arming",
  "armed",
  "unavailable",
];

export function parseTunnelState(v: unknown): HostTunnelState {
  const o = (typeof v === "object" && v !== null ? v : {}) as Record<string, unknown>;
  const phase = TUNNEL_PHASES.find((p) => p === o.phase);
  if (phase === undefined || typeof o.enabled !== "boolean") {
    throw new HostRequestError(0, "bad tunnel state");
  }
  return { enabled: o.enabled, phase };
}

/**
 * Whether Lakko's traffic must not leave the ordinary way. Only `unconfigured`
 * keeps a switched-on setting from counting: that build has no way to honour
 * it, and the settings screen says so.
 */
export function tunnelRoutesThroughHost(state: HostTunnelState): boolean {
  return state.enabled && state.phase !== "unconfigured";
}

export async function getHostTunnel(): Promise<HostTunnelState> {
  requireDesktop();
  const res = await fetch(`${HOST_TUNNEL_PATH}/state`);
  if (!res.ok) throw new HostRequestError(res.status, "tunnel state");
  return parseTunnelState(await res.json());
}

export async function setHostTunnel(enabled: boolean): Promise<HostTunnelState> {
  requireDesktop();
  const res = await fetch(`${HOST_TUNNEL_PATH}/enabled`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ enabled }),
  });
  if (!res.ok) throw new HostRequestError(res.status, "tunnel setting");
  return parseTunnelState(await res.json());
}

/**
 * Tell the host whether a call is live and how many conversations are
 * unread (the Dock badge; 0 removes it). Does nothing outside the desktop
 * app; a failure is swallowed (the badge is cosmetic, the call flag is
 * re-sent on the next change).
 */
export async function postHostState(state: HostAppState): Promise<void> {
  if (!isDesktop()) return;
  try {
    await fetch(`${HOST_APP_PATH}/state`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        callLive: state.callLive,
        unread: Math.max(0, Math.floor(state.unread)),
      }),
    });
  } catch {
    // The host is the page's own process; a failed post is retried with the
    // next state change.
  }
}

/** Read the desktop-only settings. */
export async function getHostSettings(): Promise<HostSettings> {
  requireDesktop();
  const res = await fetch(`${HOST_APP_PATH}/settings`, { method: "GET" });
  if (!res.ok) throw new HostRequestError(res.status, "settings read failed");
  return parseSettings(await res.json());
}

/** Write part of the desktop-only settings; returns the settings now in force. */
export async function setHostSettings(patch: Partial<HostSettings>): Promise<HostSettings> {
  requireDesktop();
  const res = await fetch(`${HOST_APP_PATH}/settings`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(patch),
  });
  if (!res.ok) throw new HostRequestError(res.status, "settings write failed");
  return parseSettings(await res.json());
}

/** Ask the host to quit. Only after the person confirmed. */
export async function requestHostQuit(): Promise<void> {
  requireDesktop();
  const res = await fetch(`${HOST_APP_PATH}/quit`, { method: "POST" });
  if (!res.ok) throw new HostRequestError(res.status, "quit failed");
}

/**
 * Subscribe to one kind of host event. Returns the unsubscribe function.
 * Outside the desktop app nothing is subscribed. The handler gets the event's
 * `target` (an app path) for `open-target`, `null` otherwise.
 */
export function onHostEvent(kind: HostEventKind, handler: (target: string | null) => void): () => void {
  if (!isDesktop()) return () => {};
  const listener = (e: Event): void => {
    const detail = (e as CustomEvent<unknown>).detail as { kind?: unknown; target?: unknown } | null;
    if (detail === null || typeof detail !== "object" || detail.kind !== kind) return;
    const target = typeof detail.target === "string" && isAppPath(detail.target) ? detail.target : null;
    handler(target);
  };
  window.addEventListener(HOST_EVENT, listener);
  return () => window.removeEventListener(HOST_EVENT, listener);
}

/** A path inside the app; the host checks the same before it sends one. */
export function isAppPath(p: string): boolean {
  return (
    p.startsWith("/") &&
    !p.startsWith("//") &&
    !p.includes("\\") &&
    !p.includes("://") &&
    // eslint-disable-next-line no-control-regex
    !/[\u0000-\u001f\u007f]/.test(p)
  );
}

/** One system notification, already rendered in the interface language. */
export interface HostNotification {
  /** Notifications with one tag replace each other where the OS can. */
  tag: string;
  title: string;
  body: string;
  /** The app path a click opens; `null` only brings the window forward. */
  target: string | null;
  /** Accept and Decline on a ringing call, where the OS has buttons. */
  call?: HostCallButtons;
}

/** The buttons of a ringing call's notification. */
export interface HostCallButtons {
  accept: string;
  decline: string;
  /** Lowercase hex of the call id, handed back with the press. */
  callId: string;
  /** The caller offered video, handed back with the press. */
  video: boolean;
}

/** A press of Accept or Decline. */
export interface HostCallAction {
  readonly action: "accept" | "decline";
  readonly callId: string;
  readonly video: boolean;
}

/** Subscribe to presses of Accept and Decline. Nothing outside the desktop app. */
export function onHostCallAction(handler: (action: HostCallAction) => void): () => void {
  if (!isDesktop()) return () => {};
  const listener = (e: Event): void => {
    const d = (e as CustomEvent<unknown>).detail as
      | { kind?: unknown; action?: unknown; callId?: unknown; video?: unknown }
      | null;
    if (d === null || typeof d !== "object" || d.kind !== "call-action") return;
    if ((d.action !== "accept" && d.action !== "decline") || typeof d.callId !== "string") return;
    handler({ action: d.action, callId: d.callId, video: d.video === true });
  };
  window.addEventListener(HOST_EVENT, listener);
  return () => window.removeEventListener(HOST_EVENT, listener);
}

/**
 * Ask the host to show a system notification (`docs/ux/desktop.md` section
 * 4). Resolves `false` outside the desktop app or where the host cannot show
 * one; never throws.
 */
export async function showHostNotification(n: HostNotification): Promise<boolean> {
  if (!isDesktop()) return false;
  try {
    const res = await fetch(`${HOST_APP_PATH}/notify`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(n),
    });
    return res.ok;
  } catch {
    return false;
  }
}

/** Take a delivered notification back. Never throws. */
export async function withdrawHostNotification(tag: string): Promise<void> {
  if (!isDesktop()) return;
  try {
    await fetch(`${HOST_APP_PATH}/withdraw`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ tag }),
    });
  } catch {
    // best-effort
  }
}

/** The tray menu in the interface language (Windows, Linux). Never throws. */
export async function setHostTrayLabels(labels: { open: string; quit: string }): Promise<void> {
  if (!isDesktop()) return;
  try {
    await fetch(`${HOST_APP_PATH}/labels`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(labels),
    });
  } catch {
    // The menu keeps its English labels.
  }
}

/** Whether this OS has a link to its microphone and camera settings. */
export async function hostCanOpenPrivacy(): Promise<boolean> {
  if (!isDesktop()) return false;
  try {
    const res = await fetch(`${HOST_APP_PATH}/capabilities`, { method: "GET" });
    if (!res.ok) return false;
    const v = (await res.json()) as { privacySettings?: unknown };
    return v.privacySettings === true;
  } catch {
    return false;
  }
}

/**
 * Hand decrypted bytes to the system save dialog (`docs/ux/desktop.md` section
 * 6). `false` when the person cancelled; throws when the file was not written.
 * The body is bytes, never a Blob: the host reads a Blob empty.
 */
export async function saveFileOnHost(bytes: Uint8Array, name: string): Promise<boolean> {
  const res = await fetch(`/__host/save-file?name=${encodeURIComponent(name)}`, {
    method: "POST",
    body: bytes.slice(),
  });
  if (res.status === 204) return false;
  if (!res.ok) throw new HostRequestError(res.status, "save-file");
  return true;
}

/** A link the OS handed over, whole. */
export interface HostLink {
  readonly kind: "i" | "c" | "g";
  readonly id: string;
  readonly secret: string;
}

/** The link waiting at the host, once. `null` when there is none. */
export async function takeHostLink(): Promise<HostLink | null> {
  if (!isDesktop()) return null;
  try {
    const res = await fetch(`${HOST_APP_PATH}/take-link`, { method: "POST" });
    if (res.status !== 200) return null;
    const v = (await res.json()) as { kind?: unknown; id?: unknown; secret?: unknown };
    if (v.kind !== "i" && v.kind !== "c" && v.kind !== "g") return null;
    if (typeof v.id !== "string" || typeof v.secret !== "string") return null;
    return { kind: v.kind, id: v.id, secret: v.secret };
  } catch {
    return null;
  }
}

/** Open the system page for a device. Never throws. */
export async function openHostPrivacy(what: PrivacyPage): Promise<void> {
  if (!isDesktop()) return;
  try {
    await fetch(`${HOST_APP_PATH}/open-privacy`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ what }),
    });
  } catch {
    // The screen still says where to go.
  }
}

/** Open an `http` or `https` link in the system's default browser. Never
 *  throws. */
export async function openHostUrl(url: string): Promise<void> {
  if (!isDesktop()) return;
  try {
    await fetch(`${HOST_APP_PATH}/open-url`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ url }),
    });
  } catch {
    // The link is still on screen.
  }
}

/** Bounce the Dock icon once / flash the taskbar. Never throws. */
export async function requestHostAttention(): Promise<void> {
  if (!isDesktop()) return;
  try {
    await fetch(`${HOST_APP_PATH}/attention`, { method: "POST" });
  } catch {
    // cosmetic
  }
}
