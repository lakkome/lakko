// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { withdrawDeletedMessageNotifications } from "./deleted-notifications";

const G = "ab".repeat(32);

function shown(data: unknown): { data: unknown; closed: boolean; close: () => void } {
  const n = {
    data,
    closed: false,
    close: () => {
      n.closed = true;
    },
  };
  return n;
}

describe("withdrawDeletedMessageNotifications", () => {
  it("closes only the notification standing for a deleted message of this group", async () => {
    const forDeleted = shown({ wake: { groupHex: G }, previewMessageId: "42" });
    const forOther = shown({ wake: { groupHex: G }, previewMessageId: "43" });
    const otherGroup = shown({ wake: { groupHex: "cd".repeat(32) }, previewMessageId: "42" });
    const generic = shown({ wake: { groupHex: G } });
    const reg = {
      getNotifications: async () => [forDeleted, forOther, otherGroup, generic] as unknown as Notification[],
    };
    const closed = await withdrawDeletedMessageNotifications(G, [42], async () => reg);
    expect(closed).toBe(1);
    expect([forDeleted.closed, forOther.closed, otherGroup.closed, generic.closed]).toEqual([
      true,
      false,
      false,
      false,
    ]);
  });

  it("never throws without a registration", async () => {
    expect(await withdrawDeletedMessageNotifications(G, [1], async () => null)).toBe(0);
    expect(
      await withdrawDeletedMessageNotifications(G, [1], async () => {
        throw new Error("no worker");
      }),
    ).toBe(0);
  });
});
