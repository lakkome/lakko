// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { addThroughInviteLink, answerAsKnownContact, type InviteConsume } from "./invite-add";

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

describe("Open chat on the page of a person already in the contacts (2026-10-24)", () => {
  it("consumes, then sends the answer, and keeps no pending contact", async () => {
    const { log, d } = deps("consumed");
    expect(await answerAsKnownContact(d)).toBe("sent");
    expect(log).toEqual(["consume:consumed", "send"]);
  });

  it("sends again on a link this account used before", async () => {
    const { log, d } = deps("already-consumed");
    expect(await answerAsKnownContact(d)).toBe("sent");
    expect(log).toEqual(["consume:already-consumed", "send"]);
  });

  it("sends nothing on a link that no longer works", async () => {
    for (const dead of ["exhausted", "not-found"] as const) {
      const { log, d } = deps(dead);
      expect(await answerAsKnownContact(d)).toBe("dead");
      expect(log).toEqual([`consume:${dead}`]);
    }
  });

  it("lets a failed send reach the page", async () => {
    const { d } = deps("consumed", true);
    await expect(answerAsKnownContact(d)).rejects.toThrow("offline");
  });
});
