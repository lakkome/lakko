// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §2 — a profile this device owes a group leaves before its next
 * message there. The real `ProfilePublishScheduler`, installed as the hook,
 * and the real `ChatClient.sendMessage` over a stand-in group and DS.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { ChatClient } from "./client";
import { installBeforeSend, runBeforeSend } from "./before-send";
import { ProfilePublishScheduler } from "../profile/publish-scheduler";
import type { DsClient } from "../ds/client";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

const GROUP_ID = new Uint8Array([9, 9, 9, 9]);
const GROUP_B64 = btoa(String.fromCharCode(...GROUP_ID));
const IDENTITY = {} as unknown as WasmIdentity;

function mockGroup(order: string[]): WasmGroup {
  return {
    createApplicationMessage: vi.fn(() => {
      order.push("seal");
      return new Uint8Array([1, 2, 3]);
    }),
    get groupId(): Uint8Array {
      return GROUP_ID;
    },
  } as unknown as WasmGroup;
}

function client(order: string[]): ChatClient {
  const ds = {
    publishGroup: vi.fn(async () => {
      order.push("publish message");
      return { id: 7, createdAt: "2026-11-04T00:00:00Z" };
    }),
  } as unknown as DsClient;
  return new ChatClient(ds, { identityProvider: () => IDENTITY, onPersist: async () => undefined });
}

let uninstall: () => void = () => undefined;
afterEach(() => uninstall());

describe("a send waits for the profile it owes the group", () => {
  it("joined last: the growth republish waiting out its jitter goes out before the message", async () => {
    const order: string[] = [];
    const scheduler = new ProfilePublishScheduler<number>({
      now: () => 0,
      setTimer: () => 1,
      clearTimer: () => undefined,
      random: () => 0.99,
      publish: async (g, reason) => {
        order.push(`profile ${reason} ${g}`);
      },
    });
    uninstall = installBeforeSend((g) => scheduler.beforeSend(g));
    scheduler.groupGrew(GROUP_B64);
    await client(order).sendMessage(null, mockGroup(order), new Uint8Array([1]));
    expect(order).toEqual([`profile growth ${GROUP_B64}`, "seal", "publish message"]);
  });

  it("nothing owed: the message goes out alone", async () => {
    const order: string[] = [];
    const scheduler = new ProfilePublishScheduler<number>({
      now: () => 0,
      setTimer: () => 1,
      clearTimer: () => undefined,
      random: () => 0,
      publish: async () => {
        order.push("profile");
      },
    });
    uninstall = installBeforeSend((g) => scheduler.beforeSend(g));
    await client(order).sendMessage(null, mockGroup(order), new Uint8Array([1]));
    expect(order).toEqual(["seal", "publish message"]);
  });
});

describe("runBeforeSend", () => {
  it("no hook: returns at once", async () => {
    await expect(runBeforeSend("g")).resolves.toBeUndefined();
  });

  it("a hook that throws does not stop the send", async () => {
    uninstall = installBeforeSend(async () => {
      throw new Error("ds down");
    });
    await expect(runBeforeSend("g")).resolves.toBeUndefined();
  });

  it("a hook that never settles is waited for at most the bound", async () => {
    vi.useFakeTimers();
    try {
      uninstall = installBeforeSend(() => new Promise<void>(() => undefined));
      let done = false;
      const run = runBeforeSend("g", 1_000).then(() => {
        done = true;
      });
      await vi.advanceTimersByTimeAsync(999);
      expect(done).toBe(false);
      await vi.advanceTimersByTimeAsync(1);
      await run;
      expect(done).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("an uninstalled hook is not run, and a stale uninstall does not clear a newer hook", async () => {
    const first = vi.fn(async () => undefined);
    const second = vi.fn(async () => undefined);
    const clearFirst = installBeforeSend(first);
    uninstall = installBeforeSend(second);
    clearFirst();
    await runBeforeSend("g");
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledWith("g");
  });
});
