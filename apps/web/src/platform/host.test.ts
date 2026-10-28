// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const platform = { desktop: true };
vi.mock("./index", () => ({ isDesktop: (): boolean => platform.desktop }));

import {
  getHostSettings,
  HOST_EVENT,
  HostRequestError,
  onHostEvent,
  postHostState,
  requestHostQuit,
  setHostSettings,
} from "./host";

interface Call {
  url: string;
  method: string;
  body: unknown;
}

let calls: Call[];
let reply: (url: string) => Response;

beforeEach(() => {
  platform.desktop = true;
  calls = [];
  reply = () => new Response(null, { status: 204 });
  vi.stubGlobal("fetch", (url: string, init?: RequestInit) => {
    calls.push({
      url,
      method: init?.method ?? "GET",
      body: typeof init?.body === "string" ? JSON.parse(init.body) : undefined,
    });
    return Promise.resolve(reply(url));
  });
  vi.stubGlobal("window", new EventTarget());
});

afterEach(() => {
  vi.unstubAllGlobals();
});

function json(v: unknown, status = 200): Response {
  return new Response(JSON.stringify(v), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("host app routes on desktop", () => {
  it("posts the state, clamping the count", async () => {
    await postHostState({ callLive: true, unread: -3 });
    await postHostState({ callLive: false, unread: 4 });
    expect(calls).toEqual([
      { url: "/__host/app/state", method: "POST", body: { callLive: true, unread: 0 } },
      { url: "/__host/app/state", method: "POST", body: { callLive: false, unread: 4 } },
    ]);
  });

  it("swallows a failed state post", async () => {
    vi.stubGlobal("fetch", () => Promise.reject(new Error("down")));
    await expect(postHostState({ callLive: false, unread: 0 })).resolves.toBeUndefined();
  });

  it("reads the settings", async () => {
    reply = () => json({ keepRunning: false, openAtLogin: true });
    await expect(getHostSettings()).resolves.toEqual({ keepRunning: false, openAtLogin: true });
    expect(calls[0]).toMatchObject({ url: "/__host/app/settings", method: "GET" });
  });

  it("writes a partial update and returns what the host now holds", async () => {
    reply = () => json({ keepRunning: true, openAtLogin: true });
    await expect(setHostSettings({ openAtLogin: true })).resolves.toEqual({
      keepRunning: true,
      openAtLogin: true,
    });
    expect(calls[0]).toEqual({
      url: "/__host/app/settings",
      method: "POST",
      body: { openAtLogin: true },
    });
  });

  it("refuses with the status when the login item could not be written", async () => {
    reply = () => new Response("login item", { status: 500 });
    const err = await setHostSettings({ openAtLogin: true }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(HostRequestError);
    expect((err as HostRequestError).status).toBe(500);
  });

  it("posts quit", async () => {
    await requestHostQuit();
    expect(calls).toEqual([{ url: "/__host/app/quit", method: "POST", body: undefined }]);
  });

  it("delivers only the subscribed kind, until unsubscribed", () => {
    const seen: string[] = [];
    const off = onHostEvent("quit-requested", () => seen.push("quit"));
    const offSettings = onHostEvent("open-settings", () => seen.push("settings"));
    window.dispatchEvent(new CustomEvent(HOST_EVENT, { detail: { kind: "quit-requested" } }));
    window.dispatchEvent(new CustomEvent(HOST_EVENT, { detail: { kind: "open-settings" } }));
    window.dispatchEvent(new CustomEvent(HOST_EVENT, { detail: null }));
    off();
    window.dispatchEvent(new CustomEvent(HOST_EVENT, { detail: { kind: "quit-requested" } }));
    offSettings();
    expect(seen).toEqual(["quit", "settings"]);
  });
});

describe("in the browser build", () => {
  beforeEach(() => {
    platform.desktop = false;
  });

  it("sends no host request and subscribes to nothing", async () => {
    await postHostState({ callLive: true, unread: 3 });
    await expect(getHostSettings()).rejects.toBeInstanceOf(HostRequestError);
    await expect(setHostSettings({ keepRunning: false })).rejects.toBeInstanceOf(
      HostRequestError,
    );
    await expect(requestHostQuit()).rejects.toBeInstanceOf(HostRequestError);
    const seen: string[] = [];
    onHostEvent("quit-requested", () => seen.push("quit"));
    window.dispatchEvent(new CustomEvent(HOST_EVENT, { detail: { kind: "quit-requested" } }));
    expect(seen).toEqual([]);
    expect(calls).toEqual([]);
  });
});
