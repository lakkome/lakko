// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { addThroughInviteLink, type InviteConsume } from "./invite-add";

function deps(consume: InviteConsume, sendFails = false) {
  const log: string[] = [];
  return {
    log,
    d: {
      consume: async () => {
        log.push(`consume:${consume}`);
        return consume;
      },
      recordPending: async () => {
        log.push("record");
        return true;
      },
      sendRequest: async () => {
        log.push("send");
        if (sendFails) throw new Error("offline");
      },
      forgetPending: async () => {
        log.push("forget");
      },
    },
  };
}

describe("Add on an invite page (ADR-0079 amendment of 2026-10-19)", () => {
  it("keeps nothing when the consume answers that the link died while the page was open", async () => {
    for (const dead of ["exhausted", "not-found"] as const) {
      const { log, d } = deps(dead);
      await expect(addThroughInviteLink(d)).resolves.toBe("dead");
      // No pending contact was written, no request sent.
      expect(log).toEqual([`consume:${dead}`]);
    }
  });

  it("writes the pending contact only after the consume answered live, then sends", async () => {
    const { log, d } = deps("consumed");
    await expect(addThroughInviteLink(d)).resolves.toBe("added");
    expect(log).toEqual(["consume:consumed", "record", "send"]);
  });

  it("removes the pending contact again when the send fails", async () => {
    const { log, d } = deps("already-consumed", true);
    await expect(addThroughInviteLink(d)).rejects.toThrow("offline");
    expect(log).toEqual(["consume:already-consumed", "record", "send", "forget"]);
  });
});
