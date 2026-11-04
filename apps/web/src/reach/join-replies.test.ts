// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** `reach/join-replies.ts` — a joiner's reply grant outlives a reload: a
 *  sealed knock is read once, so an approval after a reload must still find
 *  the grant the Welcome is written under (C-69). */
import { afterEach, describe, expect, it, vi } from "vitest";

const rows = new Map<string, unknown>();

vi.mock("../storage/account-kv", () => ({
  loadAccountJson: async (account: string, row: string) => structuredClone(rows.get(`${account}/${row}`) ?? null),
  saveAccountJson: async (account: string, row: string, body: unknown) => {
    rows.set(`${account}/${row}`, structuredClone(body));
  },
}));

const joiner = new Uint8Array(32).fill(7);
const other = new Uint8Array(32).fill(9);
const G1 = new Uint8Array(32).fill(1);
const G2 = new Uint8Array(32).fill(2);

async function fresh() {
  vi.resetModules();
  return import("./join-replies");
}

afterEach(() => {
  rows.clear();
});

describe("a kept reply grant", () => {
  it("lives the reply key's 7 days, also across a reopen, then goes", async () => {
    let clock = 1_000_000;
    const m = await fresh();
    m.setJoinReplyClock(() => clock);
    await m.openJoinReplies("acct");
    await m.rememberJoinReply(G1, joiner, "g");
    clock += m.JOIN_REPLY_LIFETIME_MS - 1;
    expect(m.joinReplyFor(G1, joiner)).toBe("g");

    const reloaded = await fresh();
    reloaded.setJoinReplyClock(() => clock);
    await reloaded.openJoinReplies("acct");
    expect(reloaded.joinReplyFor(G1, joiner)).toBe("g");
    clock += 2;
    expect(reloaded.joinReplyFor(G1, joiner)).toBeNull();
  });

  it("survives a reopen (a reload or a restart)", async () => {
    const first = await fresh();
    await first.openJoinReplies("acct");
    await first.rememberJoinReply(G1, joiner, "grant-b64");

    const reloaded = await fresh();
    expect(reloaded.joinReplyFor(G1, joiner)).toBeNull();
    await reloaded.openJoinReplies("acct");
    await reloaded.joinRepliesOpened();
    expect(reloaded.joinReplyFor(G1, joiner)).toBe("grant-b64");
  });

  it("goes when the joiner is refused, durably", async () => {
    const m = await fresh();
    await m.openJoinReplies("acct");
    await m.rememberJoinReply(G1, joiner, "g1");
    await m.rememberJoinReply(G1, other, "g2");
    await m.forgetJoinReply(G1, joiner);

    const reloaded = await fresh();
    await reloaded.openJoinReplies("acct");
    expect(reloaded.joinReplyFor(G1, joiner)).toBeNull();
    expect(reloaded.joinReplyFor(G1, other)).toBe("g2");
  });

  it("a grant kept before the account opened is saved when it does", async () => {
    const m = await fresh();
    await m.rememberJoinReply(G1, joiner, "early");
    await m.openJoinReplies("acct");

    const reloaded = await fresh();
    await reloaded.openJoinReplies("acct");
    expect(reloaded.joinReplyFor(G1, joiner)).toBe("early");
  });

  it("is per account", async () => {
    const m = await fresh();
    await m.openJoinReplies("acct");
    await m.rememberJoinReply(G1, joiner, "a's");
    await m.openJoinReplies("other-acct");
    expect(m.joinReplyFor(G1, joiner)).toBeNull();
  });

  it("is per request: a refusal in one group keeps the joiner's grant for another", async () => {
    const m = await fresh();
    await m.openJoinReplies("acct");
    await m.rememberJoinReply(G1, joiner, "for-g1");
    await m.rememberJoinReply(G2, joiner, "for-g2");
    expect(m.joinReplyFor(G1, joiner)).toBe("for-g1");
    expect(m.joinReplyFor(G2, joiner)).toBe("for-g2");
    await m.forgetJoinReply(G1, joiner);
    expect(m.joinReplyFor(G1, joiner)).toBeNull();
    expect(m.joinReplyFor(G2, joiner)).toBe("for-g2");
  });
});
