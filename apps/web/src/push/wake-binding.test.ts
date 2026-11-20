// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0042, "A wake is shown only for the signed-in account" — the
 * foreground half of the wake binding (`push/wake-binding.ts`).
 *
 * Pins: the binding names the signed-in account (ADR-0078: no device
 * pseudonym); a different account signing in is reported so its push-gw
 * subscriptions are removed; Log out / Forget device clear the binding FIRST
 * and then close this app's delivered notifications. The worker-side gate is
 * pinned in `src/sw/push-decrypt.test.ts`.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";

import {
  closeDeliveredNotifications,
  previousBoundAccount,
  recordWakeBinding,
  releaseWakeBinding,
} from "./wake-binding";

const ACCT = "YWNjb3VudC1h";
const OTHER = "YWNjb3VudC1i";

describe("previousBoundAccount", () => {
  it("no binding: nothing to remove", () => {
    expect(previousBoundAccount(null, ACCT)).toBeNull();
  });
  it("a binding for the same account: nothing to remove", () => {
    expect(previousBoundAccount({ accountId: ACCT }, ACCT)).toBeNull();
  });
  it("a binding for another account: that account's subscriptions go", () => {
    expect(previousBoundAccount({ accountId: OTHER }, ACCT)).toBe(OTHER);
  });
});

describe("recordWakeBinding", () => {
  it("writes the account", async () => {
    const putBinding = vi.fn(async () => {});
    expect(await recordWakeBinding(ACCT, { putBinding })).toBe(true);
    expect(putBinding).toHaveBeenCalledWith({ accountId: ACCT });
  });

  it("a failed write is swallowed (the worker then shows nothing)", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const wrote = await recordWakeBinding(ACCT, {
      putBinding: async () => {
        throw new Error("quota");
      },
    });
    expect(wrote).toBe(false);
    warn.mockRestore();
  });
});

describe("releaseWakeBinding — Log out / Forget device", () => {
  function notifications(n: number): { close: () => void; closed: boolean }[] {
    return Array.from({ length: n }, () => {
      const note = { closed: false, close: () => undefined };
      note.close = () => {
        note.closed = true;
      };
      return note;
    });
  }

  it("clears the binding FIRST, then closes every delivered notification", async () => {
    const order: string[] = [];
    const shown = notifications(3);
    const closed = await releaseWakeBinding({
      clearBinding: async () => {
        order.push("clear");
      },
      getRegistration: async () => ({
        getNotifications: async () => {
          order.push("list");
          return shown as unknown as Notification[];
        },
      }),
    });
    expect(order).toEqual(["clear", "list"]);
    expect(closed).toBe(3);
    expect(shown.every((n) => n.closed)).toBe(true);
  });

  it("a failed clear still closes the notifications", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const shown = notifications(1);
    const closed = await releaseWakeBinding({
      clearBinding: async () => {
        throw new Error("idb blocked");
      },
      getRegistration: async () => ({
        getNotifications: async () => shown as unknown as Notification[],
      }),
    });
    expect(closed).toBe(1);
    expect(shown[0]?.closed).toBe(true);
    warn.mockRestore();
  });

  it("no service worker registration: nothing to close, no throw", async () => {
    expect(await closeDeliveredNotifications(async () => undefined)).toBe(0);
    expect(await closeDeliveredNotifications(async () => null)).toBe(0);
  });
});

describe("the callers (source)", () => {
  const read = (rel: string): string =>
    readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

  it("logout and forgetDevice release the binding before anything else", () => {
    const src = read("../auth/AuthContext.tsx");
    const firstStatement = (anchor: string): string => {
      const at = src.indexOf(anchor);
      expect(at).toBeGreaterThan(-1);
      const body = src
        .slice(at + anchor.length)
        .split("\n")
        .map((l) => l.trim())
        .filter((l) => l.length > 0 && !l.startsWith("//"));
      return body[0] ?? "";
    };
    expect(firstStatement("    logout() {")).toBe("void releaseWakeBinding();");
    expect(firstStatement("    async forgetDevice() {")).toBe("await releaseWakeBinding();");
  });

  it("every sign-in subscribe names the account", () => {
    const src = read("../auth/AuthContext.tsx");
    expect(src).not.toMatch(/subscribeToPush\(\)/);
    expect(src).not.toMatch(/subscribePushFor\(\)/);
    // Signup, the session landing, a guest account that was kept, recover,
    // and the end of a suspension (ADR-0098).
    // Each goes through `subscribePushFor`, the one caller of
    // `subscribeToPush`, which names the account and passes the "is this
    // account still signed in" check with it.
    expect(src.match(/subscribePushFor\([A-Za-z0-9.]+\)\.catch/g)?.length).toBe(5);
    expect(src.match(/subscribeToPush\(/g)?.length).toBe(1);
    expect(src).toMatch(/return subscribeToPush\(accountIdB64, \(\) => \{/);
  });

  it("the service worker gates before it posts to a tab or shows anything", () => {
    const sw = read("../../public/sw.js");
    const handler = sw.slice(sw.indexOf("async function handlePush(wake) {"));
    const gate = handler.indexOf("admission = await admitWake(wake);");
    const drop = handler.indexOf("if (admission === null) return;");
    expect(gate).toBeGreaterThan(-1);
    expect(drop).toBeGreaterThan(gate);
    expect(handler.indexOf("LAKKO_PUSH_WAKE")).toBeGreaterThan(drop);
    expect(handler.indexOf("showNotification")).toBeGreaterThan(drop);
    // The worker reads the same row key the foreground writes.
    expect(sw).toContain("return `${accountId}\\0${pushGidHex.toLowerCase()}`;");
    expect(read("./group-routing-store.ts")).toContain(
      "return `${accountIdB64}\\0${pushGidHex.toLowerCase()}`;",
    );
  });

  it("subscribeToPush removes another account's subscriptions, then records and re-subscribes", () => {
    const src = read("./subscribe.ts");
    const previous = src.indexOf("previousBoundAccount(existing, accountIdB64)");
    const unsubscribeOld = src.indexOf("await unsubscribeAllForAccount(previous)");
    const token = src.indexOf("setDevicePushToken(tokenBytes)");
    const record = src.indexOf("await recordWakeBinding(accountIdB64)");
    const resubscribe = src.indexOf("await resubscribeAllForAccount(accountIdB64)");
    expect(previous).toBeGreaterThan(-1);
    expect(unsubscribeOld).toBeGreaterThan(previous);
    expect(token).toBeGreaterThan(unsubscribeOld);
    expect(record).toBeGreaterThan(token);
    expect(resubscribe).toBeGreaterThan(record);
  });
});
