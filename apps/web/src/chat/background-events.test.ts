// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { planBackgroundEvents } from "./background-events";
import type { IncomingChatEvent } from "./client";

const T = "2026-09-13T12:00:00Z";

const chat: IncomingChatEvent = {
  kind: "application",
  messageId: 1,
  text: "hi",
  plaintext: "hi",
  attachments: [],
  senderLeaf: 1,
  senderKey: "k" as never,
  createdAt: T,
};
const commit: IncomingChatEvent = { kind: "system", messageId: 2, systemKind: "commit", createdAt: T };
const offer: IncomingChatEvent = {
  kind: "callSignaling",
  messageId: 3,
  payloadBytes: new Uint8Array([0x52]),
  senderLeaf: 1,
  createdAt: T,
};
const edit: IncomingChatEvent = {
  kind: "op",
  messageId: 4,
  op: "edit",
  target: 1,
  text: "hello",
  emoji: undefined,
  isTyping: undefined,
  senderLeaf: 1,
  senderKey: "k" as never,
  createdAt: T,
};
const refusedProposal: IncomingChatEvent = {
  kind: "membershipProposal",
  messageId: 5,
  proposalKind: "remove",
  stored: false,
  addSignatureKey: undefined,
  removeTargetLeaf: 2,
  senderLeaf: 1,
  createdAt: T,
};
const storedProposal: IncomingChatEvent = { ...refusedProposal, messageId: 6, stored: true };
const beacon: IncomingChatEvent = { kind: "committerBeacon", messageId: 7, senderLeaf: 0, createdAt: T };
const secret: IncomingChatEvent = {
  kind: "pushRoutingSecret",
  messageId: 8,
  secret: new Uint8Array(32),
  mintEpoch: 3n,
  senderLeaf: 0,
  createdAt: T,
};
const profile: IncomingChatEvent = {
  kind: "profile",
  messageId: 9,
  profile: undefined,
  senderLeaf: 1,
  senderKey: "k" as never,
  createdAt: T,
};
const groupName: IncomingChatEvent = {
  kind: "group-name",
  messageId: 10,
  groupName: { name: "Trio", updatedAtMs: 5n },
  senderLeaf: 1,
  senderKey: "k" as never,
  createdAt: T,
};
const receipt: IncomingChatEvent = { ...edit, messageId: 11, op: "read-receipt", target: 1, text: "" };

describe("planBackgroundEvents", () => {
  const plan = planBackgroundEvents([
    chat,
    commit,
    offer,
    edit,
    refusedProposal,
    storedProposal,
    beacon,
    secret,
    profile,
    groupName,
    receipt,
  ]);

  it("files chat and system rows into the timeline, in journal order", () => {
    expect(plan.timeline.map((e) => e.messageId)).toEqual([1, 2]);
  });

  it("sends call signaling to the call host and never into the timeline or the view", () => {
    expect(plan.signaling.map((r) => r.messageId)).toEqual([3]);
    expect(plan.signaling[0]?.senderLeaf).toBe(1);
    expect(plan.timeline.some((e) => (e.messageId as number) === 3)).toBe(false);
    expect(plan.toView.some((e) => e.kind === "callSignaling")).toBe(false);
  });

  it("hands the chat view what only it can fold (rows, ops, proposals)", () => {
    expect(plan.toView.map((e) => e.messageId)).toEqual([1, 2, 4, 5, 6, 11]);
  });

  it("records only proposals the receipt gate stored", () => {
    expect(plan.storedProposals.map((e) => e.messageId)).toEqual([6]);
  });

  it("settles beacons, routing secrets and profiles where they are received", () => {
    expect(plan.beacons.map((e) => e.messageId)).toEqual([7]);
    expect(plan.pushSecrets.map((e) => e.messageId)).toEqual([8]);
    expect(plan.profiles.map((e) => e.messageId)).toEqual([9]);
    for (const kind of ["committerBeacon", "pushRoutingSecret", "profile"]) {
      expect(plan.toView.some((e) => e.kind === kind)).toBe(false);
    }
  });

  it("settles a group name where it is received, never in the view (ADR-0051)", () => {
    expect(plan.groupNames.map((e) => e.messageId)).toEqual([10]);
    expect(plan.toView.some((e) => e.kind === "group-name")).toBe(false);
    expect(plan.timeline.some((e) => (e.messageId as number) === 10)).toBe(false);
  });

  it("settles a read receipt in the durable store and still hands it to the view (ADR-0024)", () => {
    expect(plan.readReceipts.map((e) => e.messageId)).toEqual([11]);
    expect(plan.readReceipts.some((e) => e.op !== "read-receipt")).toBe(false);
  });

  it("flags a commit so the membership-shrink rotation runs (ADR-0043)", () => {
    expect(plan.sawCommit).toBe(true);
    expect(planBackgroundEvents([chat]).sawCommit).toBe(false);
  });
});
