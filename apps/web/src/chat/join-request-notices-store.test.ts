// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { onJoinRequestArrived } from "./join-request-notices";
import {
  clearJoinNoticesForAccount,
  clearJoinNoticesForGroup,
  joinRequestNoticesKey,
  readJoinNotices,
  unshownJoinRequestCount,
  updateJoinNotices,
} from "./join-request-notices-store";

class MemoryStorage implements Storage {
  private readonly m = new Map<string, string>();
  get length(): number {
    return this.m.size;
  }
  clear(): void {
    this.m.clear();
  }
  getItem(k: string): string | null {
    return this.m.get(k) ?? null;
  }
  key(i: number): string | null {
    return [...this.m.keys()][i] ?? null;
  }
  removeItem(k: string): void {
    this.m.delete(k);
  }
  setItem(k: string, v: string): void {
    this.m.set(k, v);
  }
}

const ACCOUNT = "acct";
const G1 = "g1";
const G2 = "g2";
const A = `${"A".repeat(43)}=`;
const B = `${"B".repeat(43)}=`;
const away = { openAndVisible: false, muted: false, notificationsAllowed: true };

describe("the join-request notice store", () => {
  it("keeps the sets per account and group", () => {
    const s = new MemoryStorage();
    updateJoinNotices(ACCOUNT, G1, (cur) => onJoinRequestArrived(cur, A, away).sets, s);
    updateJoinNotices(ACCOUNT, G2, (cur) => onJoinRequestArrived(cur, B, away).sets, s);
    expect(readJoinNotices(ACCOUNT, G1, s)).toEqual({ announced: [A], unshown: [A] });
    expect(unshownJoinRequestCount(ACCOUNT, G2, s)).toBe(1);
    expect(unshownJoinRequestCount("other", G1, s)).toBe(0);
    expect(unshownJoinRequestCount(null, G1, s)).toBe(0);
  });

  it("drops a group, then the account's row", () => {
    const s = new MemoryStorage();
    updateJoinNotices(ACCOUNT, G1, () => ({ announced: [A], unshown: [A] }), s);
    updateJoinNotices(ACCOUNT, G2, () => ({ announced: [B], unshown: [] }), s);
    clearJoinNoticesForGroup(ACCOUNT, G1, s);
    expect(readJoinNotices(ACCOUNT, G1, s)).toEqual({ announced: [], unshown: [] });
    expect(readJoinNotices(ACCOUNT, G2, s).announced).toEqual([B]);
    clearJoinNoticesForAccount(ACCOUNT, s);
    expect(s.getItem(joinRequestNoticesKey(ACCOUNT))).toBeNull();
  });

  it("removes the row when nothing is left and ignores keys that are not joiner keys", () => {
    const s = new MemoryStorage();
    updateJoinNotices(ACCOUNT, G1, () => ({ announced: [A], unshown: [] }), s);
    updateJoinNotices(ACCOUNT, G1, () => ({ announced: [], unshown: [] }), s);
    expect(s.getItem(joinRequestNoticesKey(ACCOUNT))).toBeNull();
    s.setItem(joinRequestNoticesKey(ACCOUNT), JSON.stringify({ [G1]: { a: ["junk", A, A], u: [7] } }));
    expect(readJoinNotices(ACCOUNT, G1, s)).toEqual({ announced: [A], unshown: [] });
  });
});
