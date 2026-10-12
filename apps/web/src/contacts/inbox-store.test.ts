// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The stranger inbox (`inbox-store.ts`) is one row per account: two accounts
 * on one browser never see each other's cursor or pending requests, and
 * Forget device drops only the forgotten account's row.
 */
import { describe, expect, it } from "vitest";

import {
  clearInboxForAccount,
  createPendingInbox,
  loadInbox,
  MAX_WAITING_REQUESTS,
  saveInbox,
  withWaiting,
} from "./inbox-store";

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

describe("an unshown request waiting for a verdict survives the page (UX 16.6)", () => {
  const row = (messageId: number) => ({
    messageId,
    payload: new Uint8Array([1, 2, 3]),
    createdAt: "2026-10-11T00:00:00Z",
  });
  const open = (st: ReturnType<typeof memory>) =>
    createPendingInbox({ accountIdB64: CARA, isAccepted: () => false, onChange: () => undefined, storage: st });

  it("is kept on disk with the cursor, never listed, and read back after a reload", () => {
    const st = memory();
    const inbox = open(st);
    inbox.advanceCursor(7);
    expect(inbox.holdWaiting(row(5))).toBe(true);
    expect(inbox.pending()).toEqual([]);
    // The cursor moves past it; the row stays.
    inbox.advanceCursor(9);
    const reloaded = open(st);
    expect(reloaded.cursor()).toBe(9);
    expect(reloaded.waitingRows().map((r) => r.messageId)).toEqual([5]);
    expect(reloaded.pending()).toEqual([]);
  });

  it("is dropped for good once it has its verdict", () => {
    const st = memory();
    const inbox = open(st);
    inbox.holdWaiting(row(5));
    inbox.dropWaiting(5);
    expect(open(st).waitingRows()).toEqual([]);
    expect(loadInbox(CARA, st)).toEqual({ cursor: 0, pending: [] });
  });

  it("is held once, and never past the bound", () => {
    let waiting: readonly { messageId: number; payloadB64: string; createdAt: string }[] = [];
    for (let i = 0; i < MAX_WAITING_REQUESTS; i += 1) {
      waiting = withWaiting(waiting, { ...ROW, messageId: i }).waiting;
    }
    expect(waiting).toHaveLength(MAX_WAITING_REQUESTS);
    expect(withWaiting(waiting, { ...ROW, messageId: 3 })).toEqual({ waiting, kept: true });
    expect(withWaiting(waiting, { ...ROW, messageId: 999 })).toEqual({ waiting, kept: false });
  });

  it("a write that fails keeps nothing and says so", () => {
    const st = memory();
    const inbox = open(st);
    st.setItem = () => {
      throw new Error("quota");
    };
    expect(inbox.holdWaiting(row(5))).toBe(false);
    expect(inbox.waitingRows()).toEqual([]);
  });

  it("goes with the account's row", () => {
    const st = memory();
    open(st).holdWaiting(row(5));
    clearInboxForAccount(CARA, st);
    expect(open(st).waitingRows()).toEqual([]);
  });
});
