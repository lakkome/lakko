// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  loadPendingPublisherRequests,
  markPendingPublisherLinkDead,
  recordPendingPublisherRequest,
} from "./pending-publisher-requests";

beforeEach(() => {
  const map = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
  });
});

const record = (requestedAtMs: number) => ({
  channelIdHex: "aa".repeat(16),
  groupIdHex: "bb".repeat(32),
  joinerKeyB64: "key",
  name: "News",
  description: "",
  requestedAtMs,
});

describe("pending publisher requests (UX 15.2.2)", () => {
  it("a dead link keeps the earlier request, marked, so a late approval still lands", () => {
    recordPendingPublisherRequest("acct", record(100));
    markPendingPublisherLinkDead("acct", "aa".repeat(16));
    const [r] = loadPendingPublisherRequests("acct");
    expect(r?.requestedAtMs).toBe(100);
    expect(r?.linkDead).toBe(true);
  });

  it("an Ask again that went out replaces the record and restarts the day", () => {
    recordPendingPublisherRequest("acct", record(100));
    recordPendingPublisherRequest("acct", record(900));
    const list = loadPendingPublisherRequests("acct");
    expect(list).toHaveLength(1);
    expect(list[0]?.requestedAtMs).toBe(900);
    expect(list[0]?.linkDead).toBeUndefined();
  });
});
