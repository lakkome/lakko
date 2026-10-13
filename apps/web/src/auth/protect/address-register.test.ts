// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";

import {
  dropPendingAddress,
  heldAddress,
  installAddressRegister,
  publishHeldAddress,
  rememberAddress,
} from "./address-register";

const settle = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

function port(accountIdB64: string): { writes: (string | null)[]; uninstall: () => void } {
  const writes: (string | null)[] = [];
  const uninstall = installAddressRegister({
    accountIdB64,
    set: async (address) => {
      writes.push(address);
      // The synced state folds the write and reports the register back.
      publishHeldAddress(accountIdB64, address);
    },
  });
  return { writes, uninstall };
}

let cleanup: (() => void) | null = null;
afterEach(() => {
  cleanup?.();
  cleanup = null;
  dropPendingAddress();
});

describe("the recovery address the account's devices keep", () => {
  it("is written through the synced state when it is loaded, and read back", async () => {
    const p = port("alice");
    cleanup = p.uninstall;
    rememberAddress("alice", "name@example.com");
    await settle();
    expect(p.writes).toEqual(["name@example.com"]);
    expect(heldAddress("alice")).toBe("name@example.com");
    // Another account on this page reads nothing.
    expect(heldAddress("bob")).toBeNull();
  });

  it("a write made before the synced state is loaded waits, and is applied when it is", async () => {
    // The landing after a recovery by email: the address was just typed.
    rememberAddress("alice", "name@example.com");
    expect(heldAddress("alice")).toBeNull();
    const p = port("alice");
    cleanup = p.uninstall;
    await settle();
    expect(p.writes).toEqual(["name@example.com"]);
  });

  it("a waiting write is never applied to another account", async () => {
    rememberAddress("alice", "name@example.com");
    const p = port("bob");
    cleanup = p.uninstall;
    await settle();
    expect(p.writes).toEqual([]);
  });

  it("removing the recovery email writes the empty value", async () => {
    const p = port("alice");
    cleanup = p.uninstall;
    rememberAddress("alice", "name@example.com");
    rememberAddress("alice", null);
    await settle();
    expect(p.writes).toEqual(["name@example.com", null]);
    expect(heldAddress("alice")).toBeNull();
  });

  it("a register folded from another device shows here", () => {
    const p = port("alice");
    cleanup = p.uninstall;
    publishHeldAddress("alice", "other@example.com");
    expect(heldAddress("alice")).toBe("other@example.com");
  });

  it("is forgotten when the synced state goes (sign-out)", () => {
    const p = port("alice");
    publishHeldAddress("alice", "name@example.com");
    p.uninstall();
    expect(heldAddress("alice")).toBeNull();
  });

  it("never touches plain storage and never logs", () => {
    const code = readFileSync(fileURLToPath(new URL("./address-register.ts", import.meta.url)), "utf8")
      .split("\n")
      .filter((l) => !/^\s*(\*|\/\/|\/\*)/.test(l))
      .join("\n");
    expect(/localStorage|sessionStorage|indexedDB|\.setItem\(|console\./.test(code)).toBe(false);
  });
});
