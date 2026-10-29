// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { t } from "../l10n";
import { eventToPersistedMessage, summarizeGroupSlot } from "./background-log";
import type { IncomingChatEvent } from "./client";
import {
  isDrawnMembershipRow,
  membershipChangeOfBatch,
  membershipFromPersisted,
  membershipSentences,
  persistedFromMembership,
  sweepBatchWithOwnCommit,
  withDrawnMembershipRows,
  withMembershipChange,
  type MembershipChange,
} from "./membership-rows";
import type { PersistedMessage } from "./message-log-store";

type SystemEvent = Extract<IncomingChatEvent, { kind: "system" }>;

const commit = (messageId: number, membership?: MembershipChange): SystemEvent => ({
  kind: "system",
  messageId,
  systemKind: "commit",
  createdAt: "2026-10-29T10:00:00Z",
  ...(membership === undefined ? {} : { membership }),
});
const message = (messageId: number): IncomingChatEvent =>
  ({
    kind: "application",
    messageId,
    text: "hi",
    plaintext: "hi",
    attachments: [],
    senderLeaf: undefined,
    senderKey: undefined,
    createdAt: "2026-10-29T10:00:00Z",
  }) as IncomingChatEvent;

/** Every person key is its own account unless the map says otherwise. */
const judge = (
  before: string[] | null,
  after: string[] | null,
  options: { accounts?: Record<string, string>; superseded?: string[]; own?: string } = {},
) =>
  membershipChangeOfBatch({
    personKeysBefore: before,
    personKeysAfter: after,
    accountOfKey: (k) => options.accounts?.[k] ?? null,
    isSuperseded: (k) => options.superseded?.includes(k) ?? false,
    awaited: {},
    nowMs: 1_000,
    ownPersonKeyB64: options.own ?? "ME",
  });

const sentencesOf = (change: MembershipChange, names: Record<string, string> = {}): string[] =>
  membershipSentences(
    change,
    (key, name) => t(key, { name }),
    (k) => names[k] ?? "Member",
  );

describe("membershipChangeOfBatch", () => {
  it("names a person a batch added", () => {
    expect(judge(["ME", "A"], ["ME", "A", "B"]).change).toEqual({ joined: ["B"], left: [] });
  });

  it("names a person a batch removed", () => {
    expect(judge(["ME", "A", "B"], ["ME", "A"]).change).toEqual({ joined: [], left: ["B"] });
  });

  it("records nothing for a batch that only admitted or retired a device", () => {
    // Person keys are account keys (ADR-0050): a device change leaves them equal.
    expect(judge(["ME", "A"], ["ME", "A"]).change).toBeNull();
  });

  it("records nothing for a reset person's old and new key in one batch", () => {
    const r = judge(["ME", "OLD"], ["ME", "NEW"], {
      accounts: { OLD: "acct-x", NEW: "acct-x" },
      superseded: ["OLD"],
    });
    expect(r.change).toBeNull();
  });

  it("records nothing when the reset repair lands in two batches", () => {
    const accounts = { OLD: "acct-x", NEW: "acct-x" };
    const first = judge(["ME", "OLD"], ["ME"], { accounts, superseded: ["OLD"] });
    expect(first.change).toBeNull();
    const second = membershipChangeOfBatch({
      personKeysBefore: ["ME"],
      personKeysAfter: ["ME", "NEW"],
      accountOfKey: (k) => accounts[k as keyof typeof accounts] ?? null,
      isSuperseded: (k) => k === "OLD",
      awaited: first.awaited,
      nowMs: 2_000,
      ownPersonKeyB64: "ME",
    });
    expect(second.change).toBeNull();
  });

  it("never names this device's own person", () => {
    expect(judge(["A"], ["A", "ME"]).change).toBeNull();
    expect(judge(["ME", "A", "B"], ["A"]).change).toEqual({ joined: [], left: ["B"] });
  });

  it("records nothing on a torn read", () => {
    expect(judge(null, ["ME", "B"]).change).toBeNull();
    expect(judge(["ME"], null).change).toBeNull();
  });
});

describe("withMembershipChange", () => {
  const change: MembershipChange = { joined: ["B"], left: [] };

  it("records the change on the batch's last commit row only", () => {
    const out = withMembershipChange([commit(4), message(5), commit(7), message(8)], change, "group");
    expect((out[0] as SystemEvent).membership).toBeUndefined();
    expect((out[2] as SystemEvent).membership).toEqual(change);
    expect(out[1]).toEqual(message(5));
  });

  it("records nothing in a one-to-one", () => {
    const out = withMembershipChange([commit(7)], change, "chat");
    expect((out[0] as SystemEvent).membership).toBeUndefined();
  });

  it("records nothing for no change, and nothing in a batch with no commit", () => {
    expect((withMembershipChange([commit(7)], null, "group")[0] as SystemEvent).membership).toBeUndefined();
    expect(withMembershipChange([message(5)], change, "group")).toEqual([message(5)]);
  });
});

describe("drawing a membership row", () => {
  const row = (event: IncomingChatEvent) => ({ kind: "incoming", event });

  it("draws a person who joined as '{name} joined'", () => {
    const [ev] = withMembershipChange([commit(7)], judge(["ME"], ["ME", "B"]).change, "group");
    expect(isDrawnMembershipRow(row(ev as IncomingChatEvent), "group")).toBe(true);
    expect(sentencesOf((ev as SystemEvent).membership as MembershipChange, { B: "VG3" })).toEqual([
      "VG3 joined",
    ]);
  });

  it("draws a person who left as '{name} left', and an unknown one as Member", () => {
    const change = judge(["ME", "A", "B"], ["ME"]).change as MembershipChange;
    expect(sentencesOf(change, { A: "VG2" })).toEqual(["VG2 left", "Member left"]);
  });

  it("puts every joined sentence before every left one", () => {
    expect(sentencesOf({ joined: ["C"], left: ["A"] }, { A: "Ann", C: "Cy" })).toEqual([
      "Cy joined",
      "Ann left",
    ]);
  });

  it("does not draw a device change, a stored row without keys, or any row in a one-to-one", () => {
    const deviceOnly = withMembershipChange([commit(7)], judge(["ME", "A"], ["ME", "A"]).change, "group");
    expect(isDrawnMembershipRow(row(deviceOnly[0] as IncomingChatEvent), "group")).toBe(false);
    expect(isDrawnMembershipRow(row(commit(3)), "group")).toBe(false);
    expect(isDrawnMembershipRow(row(commit(3, { joined: ["B"], left: [] })), "chat")).toBe(false);
  });

  it("keeps every other row", () => {
    const join: IncomingChatEvent = { ...commit(9), systemKind: "external-join-proposal" };
    const rows = [row(commit(1)), row(message(2)), { kind: "outgoing" }, row(join)];
    expect(withDrawnMembershipRows(rows, "group")).toEqual([rows[1], rows[2], rows[3]]);
    expect(withDrawnMembershipRows(rows, "chat")).toEqual([rows[1], rows[2], rows[3]]);
  });
});

describe("the stored row", () => {
  it("round-trips the person keys, never a name", () => {
    const stored = eventToPersistedMessage(commit(7, { joined: ["B"], left: ["A"] }));
    expect(stored.membership).toEqual({ joinedKeysB64: ["B"], leftKeysB64: ["A"] });
    expect(JSON.stringify(stored)).not.toContain("VG3");
    expect(membershipFromPersisted(stored.membership)).toEqual({ joined: ["B"], left: ["A"] });
  });

  it("stores nothing for a commit row without keys, and reads malformed data as none", () => {
    expect(eventToPersistedMessage(commit(7)).membership).toBeUndefined();
    expect(persistedFromMembership({ joined: [], left: [] })).toBeUndefined();
    expect(membershipFromPersisted(undefined)).toBeUndefined();
    expect(membershipFromPersisted({ joinedKeysB64: [1, ""], leftKeysB64: "x" })).toBeUndefined();
  });

  it("is never the chat-list preview", () => {
    const text: PersistedMessage = {
      messageId: "6",
      direction: "incoming",
      text: "hello",
      attachments: [],
      createdAt: "2026-10-29T10:00:00Z",
    };
    const membershipRow = eventToPersistedMessage(commit(7, { joined: ["B"], left: [] }));
    const summary = summarizeGroupSlot({ cursor: "7", messages: [text, membershipRow] }, null);
    expect(summary.preview).toBe("hello");
  });
});

describe("this device's own commit (the sweep)", () => {
  const at = "2026-10-29T11:00:00Z";
  const committed = { kind: "committed", commitMessageId: 12 };

  it("adds the own commit row at the id the commit route answered, after the swept rows", () => {
    const batch = sweepBatchWithOwnCommit([message(10), commit(11)], committed, at);
    expect(batch.map((e) => ("messageId" in e ? e.messageId : null))).toEqual([10, 11, 12]);
    expect(batch[2]).toEqual({ kind: "system", messageId: 12, systemKind: "commit", createdAt: at });
  });

  it("shows the committer the same named row, folding in what the rows below it changed", () => {
    // Before the sweep's reads: ME, A. A foreign commit below ours removed A;
    // our own commit added B. One diff across the whole sweep.
    const change = judge(["ME", "A"], ["ME", "B"]).change;
    const batch = withMembershipChange(
      sweepBatchWithOwnCommit([commit(11)], committed, at),
      change,
      "group",
    );
    expect((batch[0] as SystemEvent).membership).toBeUndefined();
    const own = batch[1] as SystemEvent;
    expect(own.messageId).toBe(12);
    expect(isDrawnMembershipRow({ kind: "incoming", event: own }, "group")).toBe(true);
    expect(sentencesOf(own.membership as MembershipChange, { A: "VG2", B: "VG3" })).toEqual([
      "VG3 joined",
      "VG2 left",
    ]);
    expect(eventToPersistedMessage(own).membership).toEqual({
      joinedKeysB64: ["B"],
      leftKeysB64: ["A"],
    });
  });

  it("adds nothing when the sweep did not merge", () => {
    expect(sweepBatchWithOwnCommit([commit(11)], { kind: "lost", commitMessageId: 12 }, at)).toEqual([
      commit(11),
    ]);
    expect(sweepBatchWithOwnCommit([], { kind: "nothing-pending" }, at)).toEqual([]);
    expect(sweepBatchWithOwnCommit([], null, at)).toEqual([]);
  });

  it("never adds the own commit twice", () => {
    expect(sweepBatchWithOwnCommit([commit(12)], committed, at)).toEqual([commit(12)]);
  });

  it("is not drawn in a one-to-one, nor when the sweep only changed devices", () => {
    const inChat = withMembershipChange(
      sweepBatchWithOwnCommit([], committed, at),
      judge(["ME", "A"], ["ME", "A", "B"]).change,
      "chat",
    );
    expect(isDrawnMembershipRow({ kind: "incoming", event: inChat[0] as IncomingChatEvent }, "chat")).toBe(false);
    const devicesOnly = withMembershipChange(
      sweepBatchWithOwnCommit([], committed, at),
      judge(["ME", "A"], ["ME", "A"]).change,
      "group",
    );
    expect(
      isDrawnMembershipRow({ kind: "incoming", event: devicesOnly[0] as IncomingChatEvent }, "group"),
    ).toBe(false);
  });
});
