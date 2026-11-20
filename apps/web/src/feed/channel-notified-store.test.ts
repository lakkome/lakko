// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  channelNotifiedKey,
  clearChannelNotified,
  readChannelNotified,
  writeChannelNotified,
} from "./channel-notified-store";

function fakeStorage(): Storage {
  const m = new Map<string, string>();
  return {
    get length() {
      return m.size;
    },
    clear: () => m.clear(),
    getItem: (k) => m.get(k) ?? null,
    key: (i) => Array.from(m.keys())[i] ?? null,
    removeItem: (k) => void m.delete(k),
    setItem: (k, v) => void m.set(k, v),
  };
}

const A = "QUNDT1VOVA==";
const C1 = "aa".repeat(16);
const C2 = "bb".repeat(16);

describe("the last notified post per channel", () => {
  it("is kept per account, never moves back, and drops channels no longer held", () => {
    const s = fakeStorage();
    writeChannelNotified(A, C1, 5, new Set([C1, C2]), s);
    writeChannelNotified(A, C2, 3, new Set([C1, C2]), s);
    writeChannelNotified(A, C1, 2, new Set([C1, C2]), s);
    expect(readChannelNotified(A, s)).toEqual({ [C1]: 5, [C2]: 3 });
    writeChannelNotified(A, C2, 4, new Set([C2]), s);
    expect(readChannelNotified(A, s)).toEqual({ [C2]: 4 });
    expect(readChannelNotified("other", s)).toEqual({});
  });

  it("reads a damaged row as empty and is removed by the wipe", () => {
    const s = fakeStorage();
    s.setItem(channelNotifiedKey(A), "{not json");
    expect(readChannelNotified(A, s)).toEqual({});
    s.setItem(channelNotifiedKey(A), JSON.stringify({ [C1]: 2, short: 3, [C2]: -1 }));
    expect(readChannelNotified(A, s)).toEqual({ [C1]: 2 });
    clearChannelNotified(A, s);
    expect(s.getItem(channelNotifiedKey(A))).toBeNull();
  });
});
