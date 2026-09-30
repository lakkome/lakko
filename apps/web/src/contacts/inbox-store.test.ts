// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The stranger inbox (`inbox-store.ts`) is one row per account: two accounts
 * on one browser never see each other's cursor or pending requests, and
 * Forget device drops only the forgotten account's row.
 */
import { describe, expect, it } from "vitest";

import { clearInboxForAccount, loadInbox, saveInbox } from "./inbox-store";

function memory(): Pick<Storage, "getItem" | "setItem" | "removeItem" | "key" | "length"> & {
  data: Map<string, string>;
} {
  const data = new Map<string, string>();
  return {
    data,
    get length() {
      return data.size;
    },
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, String(v)),
    removeItem: (k: string) => void data.delete(k),
    key: (i: number) => [...data.keys()][i] ?? null,
  };
}

const CARA = "Y2FyYS1hY2NvdW50LTAwMDA=";
const EVE = "ZXZlLWFjY291bnQtMDAwMDA=";
const ROW = { messageId: 4, payloadB64: "AQID", createdAt: "2026-09-30T00:00:00Z" };

describe("the stranger inbox belongs to one account", () => {
  it("two accounts never see each other's cursor or pending list", () => {
    const st = memory();
    saveInbox(CARA, { cursor: 9, pending: [ROW] }, st);
    expect(loadInbox(EVE, st)).toEqual({ cursor: 0, pending: [] });
    saveInbox(EVE, { cursor: 2, pending: [] }, st);
    expect(loadInbox(CARA, st)).toEqual({ cursor: 9, pending: [ROW] });
  });

  it("Forget device drops only the forgotten account's row", () => {
    const st = memory();
    saveInbox(CARA, { cursor: 9, pending: [ROW] }, st);
    saveInbox(EVE, { cursor: 2, pending: [] }, st);

    clearInboxForAccount(CARA, st);

    expect(loadInbox(CARA, st)).toEqual({ cursor: 0, pending: [] });
    expect(loadInbox(EVE, st)).toEqual({ cursor: 2, pending: [] });
  });
});
