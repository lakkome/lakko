// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `subscribeToPush` across a sign-out. It is called fire-and-forget at
 * sign-in and waits on the permission prompt, which has no time limit; the
 * account it was called for can be gone when it continues. The wake binding
 * it records is what the service worker treats as "the signed-in account",
 * so it must never be recorded for an account that has left. ADR-0078: the
 * token is stored on the device and every subscription of the account is
 * sent again with it; another bound account's subscriptions are removed.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => ({
  bindings: [] as string[],
  registered: 0,
  rotated: [] as string[],
  tokens: 0,
  registerGate: null as null | Promise<void>,
}));

vi.mock("./group-routing", () => ({
  // ADR-0078: the token reaches push-gw only inside each sealed
  // subscription, so "registering" is re-sending every subscription.
  resubscribeAllForAccount: async (): Promise<void> => {
    calls.registered += 1;
    await calls.registerGate;
  },
  unsubscribeAllForAccount: async (account: string): Promise<void> => {
    calls.rotated.push(account);
  },
}));
vi.mock("./device-token", () => ({
  setDevicePushToken: () => {
    calls.tokens += 1;
    return true;
  },
  clearDevicePushToken: () => undefined,
}));
// Another account is bound on this browser: a subscribe that goes through
// removes that account's subscriptions.
vi.mock("./group-routing-store", () => ({ getWakeBinding: async () => ({ accountId: "OTHER" }) }));
vi.mock("./wake-binding", () => ({
  previousBoundAccount: (existing: { accountId: string } | null, account: string) =>
    existing !== null && existing.accountId !== account ? existing.accountId : null,
  recordWakeBinding: async (account: string) => {
    calls.bindings.push(account);
  },
  releaseWakeBinding: async () => undefined,
}));

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

const subscription = {
  endpoint: "https://push.example/abc",
  getKey: () => new Uint8Array([1, 2, 3]).buffer,
};

let permission: { promise: Promise<string>; resolve: (value: string) => void };

async function load(): Promise<typeof import("./subscribe")> {
  vi.resetModules();
  vi.stubEnv("VITE_LAKKO_WEBPUSH_VAPID_PUBLIC_KEY_B64URL", "AQID");
  return import("./subscribe");
}

beforeEach(() => {
  calls.bindings.length = 0;
  calls.registered = 0;
  calls.rotated.length = 0;
  calls.tokens = 0;
  calls.registerGate = null;
  permission = deferred<string>();
  vi.stubGlobal("navigator", {
    serviceWorker: {
      ready: Promise.resolve({
        pushManager: { getSubscription: async () => subscription },
      }),
    },
  });
  vi.stubGlobal("window", { PushManager: class {} });
  vi.stubGlobal("Notification", {
    permission: "default",
    requestPermission: () => permission.promise,
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
});

describe("subscribeToPush", () => {
  it("records the wake binding for the account that is still signed in", async () => {
    const { subscribeToPush } = await load();
    const run = subscribeToPush("A", () => true);
    permission.resolve("granted");
    expect(await run).not.toBeNull();
    expect(calls.rotated).toEqual(["OTHER"]);
    expect(calls.tokens).toBe(1);
    expect(calls.registered).toBe(1);
    expect(calls.bindings).toEqual(["A"]);
  });

  it("does nothing after a sign-out during the permission prompt", async () => {
    const { subscribeToPush } = await load();
    let current = true;
    const run = subscribeToPush("A", () => current);
    // The prompt stays open; A signs out and B signs in; then it is answered.
    current = false;
    permission.resolve("granted");
    expect(await run).toBeNull();
    expect(calls.rotated).toEqual([]);
    expect(calls.tokens).toBe(0);
    expect(calls.registered).toBe(0);
    expect(calls.bindings).toEqual([]);
  });

  it("reports nothing when the sign-out lands during the re-subscribe", async () => {
    const { subscribeToPush } = await load();
    let current = true;
    const gate = deferred<void>();
    calls.registerGate = gate.promise;
    const run = subscribeToPush("A", () => current);
    permission.resolve("granted");
    await vi.waitFor(() => expect(calls.registered).toBe(1));
    current = false;
    gate.resolve();
    expect(await run).toBeNull();
    // The binding was recorded before the re-subscribe began; the sign-out's
    // own release clears it.
    expect(calls.bindings).toEqual(["A"]);
  });
});
