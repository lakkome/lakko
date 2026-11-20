// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The per-account record of passkeys this device found cannot carry keys
 * (docs/ux/identity-recovery.md 10.2, amended 2026-11-11).
 */
import { describe, expect, it } from "vitest";

import {
  addNoPrfPasskey,
  clearNoPrfPasskeys,
  NO_PRF_MAX_IDS,
  readNoPrfPasskeys,
  removeNoPrfPasskey,
} from "./passkey-no-prf";
import type { MarkerStorage } from "./phrase-gate";

class MemoryStorage implements MarkerStorage {
  readonly rows = new Map<string, string>();
  getItem(key: string): string | null {
    return this.rows.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.rows.set(key, value);
  }
  removeItem(key: string): void {
    this.rows.delete(key);
  }
}

const A = "QUFBQUFBQUFBQUFBQUFBQQ==";
const B = "QkJCQkJCQkJCQkJCQkJCQg==";

describe("the passkeys this device found cannot carry keys", () => {
  it("survive a new read of the same storage (a reload)", () => {
    const s = new MemoryStorage();
    addNoPrfPasskey(s, A, "cred-1");
    addNoPrfPasskey(s, A, "cred-2");
    addNoPrfPasskey(s, A, "cred-1");
    expect(readNoPrfPasskeys(s, A)).toEqual(["cred-2", "cred-1"]);
  });

  it("are kept per account, under a key that names the account", () => {
    const s = new MemoryStorage();
    addNoPrfPasskey(s, A, "cred-1");
    expect(readNoPrfPasskeys(s, B)).toEqual([]);
    expect([...s.rows.keys()].every((k) => k.includes(A))).toBe(true);
  });

  it("drop one passkey when it is removed, and the row with the last one", () => {
    const s = new MemoryStorage();
    addNoPrfPasskey(s, A, "cred-1");
    addNoPrfPasskey(s, A, "cred-2");
    removeNoPrfPasskey(s, A, "cred-1");
    expect(readNoPrfPasskeys(s, A)).toEqual(["cred-2"]);
    removeNoPrfPasskey(s, A, "cred-2");
    expect(s.rows.size).toBe(0);
  });

  it("are cleared with the account (Forget device)", () => {
    const s = new MemoryStorage();
    addNoPrfPasskey(s, A, "cred-1");
    addNoPrfPasskey(s, B, "cred-9");
    clearNoPrfPasskeys(s, A);
    expect(readNoPrfPasskeys(s, A)).toEqual([]);
    expect(readNoPrfPasskeys(s, B)).toEqual(["cred-9"]);
  });

  it("read as none when the row is damaged or storage is missing", () => {
    const s = new MemoryStorage();
    s.setItem(`lakko:passkey-no-prf:${A}`, "{not json");
    expect(readNoPrfPasskeys(s, A)).toEqual([]);
    s.setItem(`lakko:passkey-no-prf:${A}`, JSON.stringify(["ok", 5, null]));
    expect(readNoPrfPasskeys(s, A)).toEqual(["ok"]);
    expect(readNoPrfPasskeys(undefined, A)).toEqual([]);
  });

  it("keep at most the newest ids", () => {
    const s = new MemoryStorage();
    for (let i = 0; i < NO_PRF_MAX_IDS + 5; i += 1) addNoPrfPasskey(s, A, `cred-${i}`);
    const ids = readNoPrfPasskeys(s, A);
    expect(ids).toHaveLength(NO_PRF_MAX_IDS);
    expect(ids[ids.length - 1]).toBe(`cred-${NO_PRF_MAX_IDS + 4}`);
    expect(ids).not.toContain("cred-0");
  });
});
