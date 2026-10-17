// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0079 (ordering of 2026-10-17) - a founded one-to-one's Welcome is
 * written to this account's own mailbox first, and to the recipient's only
 * after that write succeeded.
 */

import { describe, expect, it, vi } from "vitest";

import type { DiscoveryClient } from "../discovery/client";
import type { DsClient } from "../ds/client";
import { ContactsClient } from "./client";

function client(): ContactsClient {
  return new ContactsClient({} as unknown as DiscoveryClient, {} as unknown as DsClient, {
    onPersist: async () => {},
    onPersistWithCommit: async () => {},
  });
}

const own = new Uint8Array(32).fill(1);
const recipient = new Uint8Array(32).fill(2);
const targets = [
  { accountKey: recipient, isRecipient: true },
  { accountKey: own, isRecipient: false },
];

describe("deliverPreparedWelcomes", () => {
  it("writes the own mailbox before the recipient's", async () => {
    const c = client();
    const written: number[] = [];
    vi.spyOn(c, "sendWelcome").mockImplementation(async (key: Uint8Array) => {
      written.push(key[0] ?? 0);
      return { messageId: written.length } as never;
    });
    const out = await c.deliverPreparedWelcomes(new Uint8Array(32), new Uint8Array([9]), targets);
    expect(written).toEqual([1, 2]);
    // The recipient's write names the Welcome.
    expect(out.welcomeMessageId).toBe(2);
  });

  it("never writes the recipient after a failed own write", async () => {
    const c = client();
    const written: number[] = [];
    vi.spyOn(c, "sendWelcome").mockImplementation(async (key: Uint8Array) => {
      written.push(key[0] ?? 0);
      if (key[0] === 1) throw new Error("own mailbox unreachable");
      return { messageId: 1 } as never;
    });
    await expect(c.deliverPreparedWelcomes(new Uint8Array(32), new Uint8Array([9]), targets)).rejects.toThrow();
    expect(written).toEqual([1]);
  });
});
