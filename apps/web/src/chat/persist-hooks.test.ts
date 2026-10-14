// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `onPersist` hook of the page's MLS clients resolves only when the
 * snapshot was written (`persist-hooks.ts`): a persist that did not happen
 * stops the publish behind it.
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it, vi } from "vitest";

import { NoSessionPersistError, durableSnapshotHook, sessionAccountOf } from "./persist-hooks";

describe("durableSnapshotHook", () => {
  it("writes the snapshot of the account signed in now", async () => {
    const persistSnapshotDurable = vi.fn(async () => undefined);
    let account: string | null = "acct-1";
    const hook = durableSnapshotHook({ persistSnapshotDurable }, () => account);
    await hook();
    account = "acct-2";
    await hook();
    expect(persistSnapshotDurable.mock.calls).toEqual([["acct-1"], ["acct-2"]]);
  });

  it("refuses when no account is signed in", async () => {
    const persistSnapshotDurable = vi.fn(async () => undefined);
    const hook = durableSnapshotHook({ persistSnapshotDurable }, () => null);
    await expect(hook()).rejects.toBeInstanceOf(NoSessionPersistError);
    expect(persistSnapshotDurable).not.toHaveBeenCalled();
  });

  it("passes a refused write through (no Identity, no device key, a sealed account)", async () => {
    const refusal = new Error("persistSnapshotDurable: no device key");
    const hook = durableSnapshotHook(
      { persistSnapshotDurable: async () => Promise.reject(refusal) },
      () => "acct-1",
    );
    await expect(hook()).rejects.toBe(refusal);
  });
});

describe("sessionAccountOf", () => {
  it("names the account of a session only", () => {
    expect(sessionAccountOf({ kind: "session", account: { accountId: "a" } })).toBe("a");
    expect(sessionAccountOf({ kind: "anon" })).toBeNull();
    expect(sessionAccountOf({ kind: "loading" })).toBeNull();
  });
});

describe("the page's MLS clients", () => {
  it("never build their persist hook on the best-effort snapshot write", () => {
    // `persistSnapshot` answers success when there is nothing it can write;
    // a hook built on it lets a publish follow a persist that did not happen.
    const root = fileURLToPath(new URL("..", import.meta.url));
    const offenders: string[] = [];
    const walk = (dir: string): void => {
      for (const name of readdirSync(dir)) {
        const path = join(dir, name);
        if (statSync(path).isDirectory()) {
          walk(path);
          continue;
        }
        if (!/\.(ts|tsx)$/.test(name) || /\.test\.(ts|tsx)$/.test(name)) continue;
        const text = readFileSync(path, "utf8");
        if (/onPersist:\s*async/.test(text)) offenders.push(path.slice(root.length));
      }
    };
    walk(root);
    expect(offenders).toEqual([]);
  });
});
