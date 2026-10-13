// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `subscribeToPush` across a sign-out. It is called fire-and-forget at
 * sign-in and waits on the permission prompt, which has no time limit; the
 * account it was called for can be gone when it continues. The wake binding
 * it records is what the service worker treats as "the signed-in account",
 * so it must never be recorded for an account that has left.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => ({
  bindings: [] as string[],
  registered: 0,
  rotated: 0,
  registerGate: null as null | Promise<void>,
}));

vi.mock("./client", () => ({
  PushClient: class {
    async register(): Promise<void> {
      calls.registered += 1;
      await calls.registerGate;
    }
  },
}));
vi.mock("./device-id", () => ({
  clearDevicePushId: () => {
    calls.rotated += 1;
  },
  getOrCreateDevicePushId: () => new Uint8Array(32),
}));
vi.mock("./group-routing-store", () => ({ getWakeBinding: async () => null }));
vi.mock("./wake-binding", () => ({
  // Another account's pseudonym is on this browser: a subscribe that goes
  // through rotates it.
  pseudonymMustRotate: () => true,
  recordWakeBinding: async (account: string) => {
    calls.bindings.push(account);
  },
  releaseWakeBinding: async () => undefined,
}));
vi.mock("../tokens/anon-token-pool", () => ({ anonTokenQuotaExhausted: () => null }));

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
  calls.rotated = 0;
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
    expect(calls.rotated).toBe(0);
    expect(calls.registered).toBe(0);
    expect(calls.bindings).toEqual([]);
  });

  it("does not record the binding when the sign-out lands during the register request", async () => {
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
    expect(calls.bindings).toEqual([]);
  });
});
