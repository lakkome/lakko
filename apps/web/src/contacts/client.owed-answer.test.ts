// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * An accepted request's owed entry, through the client: what it writes, in
 * what order, and that the retry takes it up. The outbox store and the two
 * writes are stood in for.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { PendingCommitEntry } from "../chat/commit-outbox";

const outbox = vi.hoisted(() => ({
  entries: [] as unknown[],
  put: vi.fn(async () => undefined),
  acked: vi.fn(async () => undefined),
}));

vi.mock("../chat/commit-outbox", async (original) => ({
  ...(await original<typeof import("../chat/commit-outbox")>()),
  loadUnacked: async () => outbox.entries,
  putEntryDurable: outbox.put,
  markAcked: outbox.acked,
}));

import { base64Encode } from "../auth/account-id";
import { ContactsClient, ContactsError, isAnswerUnreachable } from "./client";
import { MailboxUnreachableError } from "../reach/mailbox-write";

const GROUP = new Uint8Array(32).fill(1);
const KEY = new Uint8Array(32).fill(2);
const OWN = new Uint8Array(32).fill(3);

function entry(over: Partial<NonNullable<PendingCommitEntry["answer"]>> = {}): PendingCommitEntry {
  return {
    id: "e1",
    accountId: "ACC",
    groupId: base64Encode(GROUP),
    targetEpoch: "1",
    commitB64: "",
    secondArtifactB64: base64Encode(new Uint8Array([9])),
    secondArtifactKind: "welcome",
    sweepVerdictPending: true,
    sweepWelcomesOwed: true,
    acked: false,
    answer: {
      replyGrantB64: "REPLY",
      fromPubkeyB64: base64Encode(KEY),
      fromPseudonymB64: base64Encode(new Uint8Array(16).fill(4)),
      targets: [
        { accountKeyB64: base64Encode(OWN), isRecipient: false },
        { accountKeyB64: base64Encode(KEY), isRecipient: true },
      ],
      ...over,
    },
  };
}

function client() {
  const c = new ContactsClient();
  const deliver = vi.fn(async () => ({ welcomeMessageId: 1 }));
  const ack = vi.fn(async () => ({ messageId: 2, createdAt: "" }));
  c.deliverPreparedWelcomes = deliver as never;
  c.sendContactAck = ack as never;
  return { c, deliver, ack };
}

beforeEach(() => {
  outbox.entries = [];
  outbox.put.mockClear();
  outbox.acked.mockClear();
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

describe("the owed answer", () => {
  it("is written durably, marked so the restart loop leaves it alone", async () => {
    const { c } = client();
    const written = await c.recordOwedAnswer(
      "ACC",
      { fromPubkey: KEY, fromPseudonym: new Uint8Array(16), replyGrantB64: "REPLY" },
      {
        group: { groupId: GROUP, epoch: 1n } as never,
        welcome: new Uint8Array([9]),
        targets: [
          { accountKey: KEY, isRecipient: true },
          { accountKey: OWN, isRecipient: false },
        ],
      },
    );
    expect(outbox.put).toHaveBeenCalledWith(written);
    expect(written).toMatchObject({ sweepVerdictPending: true, sweepWelcomesOwed: true, acked: false });
    // Own mailbox first: whenever the requester holds it, every own device does.
    expect(written.answer?.targets.map((t) => t.isRecipient)).toEqual([false, true]);
  });

  it("delivers as an answer, under the reply grant, then acks, then closes", async () => {
    const { c, deliver, ack } = client();
    expect(await c.settleOwedAnswer(null, entry())).toBe("settled");
    expect(deliver.mock.calls[0]).toMatchObject({
      3: { replyGrantB64: "REPLY", answersRequest: true },
    });
    expect(deliver.mock.invocationCallOrder[0]).toBeLessThan(ack.mock.invocationCallOrder[0]!);
    expect(outbox.acked).toHaveBeenCalledWith("e1");
  });

  it("a failed Welcome sends no ack and keeps the entry", async () => {
    const { c, deliver, ack } = client();
    deliver.mockRejectedValue(new Error("offline"));
    expect(await c.settleOwedAnswer(null, entry())).toBe("owed");
    expect(ack).not.toHaveBeenCalled();
    expect(outbox.acked).not.toHaveBeenCalled();
  });

  it("the retry takes up every owed answer of the group and nothing else", async () => {
    const { c, deliver } = client();
    outbox.entries = [entry(), { ...entry(), id: "sweep", answer: undefined }];
    await c.retryOwedAnswers(null, base64Encode(GROUP));
    expect(deliver).toHaveBeenCalledTimes(1);
    expect(outbox.acked.mock.calls).toEqual([["e1"]]);
  });

  it("a retry with the Welcome landed does not write it again", async () => {
    const { c, deliver, ack } = client();
    expect(await c.settleOwedAnswer(null, entry({ welcomeLanded: true }))).toBe("settled");
    expect(deliver).not.toHaveBeenCalled();
    expect(ack).toHaveBeenCalledTimes(1);
  });

  it("runs one delivery per entry at a time", async () => {
    const { c, deliver } = client();
    let finish: () => void = () => {};
    deliver.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = () => resolve({ welcomeMessageId: 1 });
        }),
    );
    const first = c.settleOwedAnswer(null, entry());
    expect(await c.settleOwedAnswer(null, entry())).toBe("owed");
    finish();
    expect(await first).toBe("settled");
    expect(deliver).toHaveBeenCalledTimes(1);
  });

  it("tells an unreachable mailbox from any other failure, wrapped or not", () => {
    const unreachable = new MailboxUnreachableError(2);
    expect(isAnswerUnreachable(unreachable)).toBe(true);
    expect(isAnswerUnreachable(ContactsError.wrap(unreachable))).toBe(true);
    expect(isAnswerUnreachable(ContactsError.wrap(new Error("offline")))).toBe(false);
  });
});
