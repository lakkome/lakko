// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { dropsFromSupersededSender, planBackgroundEvents } from "./background-events";
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
const profileRequest: IncomingChatEvent = {
  kind: "profileRequest",
  messageId: 12,
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
const pin: IncomingChatEvent = {
  kind: "pin",
  messageId: 13,
  pin: { messageId: 1, pinned: true, updatedAtMs: 7n },
  senderLeaf: 1,
  senderKey: "k" as never,
  createdAt: T,
};

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
    profileRequest,
    pin,
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

  it("settles a profile request where it is received, never in the view or the timeline (ADR-0048)", () => {
    expect(plan.profileRequests.map((e) => e.messageId)).toEqual([12]);
    expect(plan.toView.some((e) => e.kind === "profileRequest")).toBe(false);
    expect(plan.timeline.some((e) => (e.messageId as number) === 12)).toBe(false);
  });

  it("settles a group name where it is received, never in the view (ADR-0051)", () => {
    expect(plan.groupNames.map((e) => e.messageId)).toEqual([10]);
    expect(plan.toView.some((e) => e.kind === "group-name")).toBe(false);
    expect(plan.timeline.some((e) => (e.messageId as number) === 10)).toBe(false);
  });

  it("settles a pin where it is received, never in the view (ADR-0052)", () => {
    expect(plan.pins.map((e) => e.messageId)).toEqual([13]);
    expect(plan.toView.some((e) => e.kind === "pin")).toBe(false);
    expect(plan.timeline.some((e) => (e.messageId as number) === 13)).toBe(false);
  });

  it("settles a read receipt in the durable store and still hands it to the view (ADR-0024)", () => {
    expect(plan.readReceipts.map((e) => e.messageId)).toEqual([11]);
    expect(plan.readReceipts.some((e) => e.op !== "read-receipt")).toBe(false);
  });

  it("settles a message op against the stored rows and still hands it to the view (ADR-0023)", () => {
    // Edit / delete / reaction are the message, not status: a view that never
    // mounts must not be the only place they land, or a delete that arrives
    // while its chat is closed is lost with the in-memory buffer and its row
    // can never be decrypted again.
    expect(plan.messageOps.map((e) => e.messageId)).toEqual([4]);
    expect(plan.toView.some((e) => e.kind === "op" && e.messageId === 4)).toBe(true);
  });

  it("keeps typing and read receipts out of the message ops", () => {
    // They share the `op` kind and nothing else: folding a typing signal into
    // a transcript would write a status change into the record. Planned over
    // its own batch, because the shared fixture carries no typing event and
    // the assertion would otherwise hold vacuously.
    const typing: IncomingChatEvent = {
      ...edit,
      messageId: 12,
      op: "typing",
      target: 0,
      text: "",
      isTyping: true,
    };
    const own = planBackgroundEvents([typing, receipt, edit]);
    expect(own.messageOps.map((e) => e.messageId)).toEqual([4]);
    expect(own.toView).toHaveLength(3);
  });

  it("flags a commit so the membership-shrink rotation runs (ADR-0043)", () => {
    expect(plan.sawCommit).toBe(true);
    expect(planBackgroundEvents([chat]).sawCommit).toBe(false);
  });
});

describe("nothing from a superseded sender acts (ADR-0073 R.8)", () => {
  const retired = (k: string): boolean => k === "k";
  const everything = [
    chat,
    { ...offer, senderKey: "k" as never },
    edit,
    receipt,
    profile,
    profileRequest,
    groupName,
    pin,
    beacon,
    secret,
  ] as IncomingChatEvent[];

  it("a call offer, an op and a chat row from a retired key are dropped in ANY conversation", () => {
    const plan = planBackgroundEvents(everything, retired);
    expect(plan.timeline).toEqual([]);
    expect(plan.signaling).toEqual([]);
    expect(plan.messageOps).toEqual([]);
    expect(plan.readReceipts).toEqual([]);
    // In a conversation repaired in place its service envelopes still apply
    // until the eviction lands.
    expect(plan.profiles).toHaveLength(1);
    expect(plan.profileRequests).toHaveLength(1);
    expect(plan.groupNames).toHaveLength(1);
    expect(plan.pins).toHaveLength(1);
    expect(plan.beacons).toHaveLength(1);
    expect(plan.pushSecrets).toHaveLength(1);
  });

  it("in a conversation that is unrepairable for that reset, or moved, its control and service envelopes are dropped too", () => {
    const plan = planBackgroundEvents(everything, retired, undefined, {
      sender: retired,
      // The two leaf-only envelopes come from leaf 0, which is the retired
      // person's.
      senderKeyOfLeaf: (leaf) => (leaf === 0 ? "k" : undefined),
    });
    expect(plan.timeline).toEqual([]);
    expect(plan.signaling).toEqual([]);
    expect(plan.profiles).toEqual([]);
    expect(plan.profileRequests).toEqual([]);
    expect(plan.groupNames).toEqual([]);
    expect(plan.pins).toEqual([]);
    expect(plan.beacons).toEqual([]);
    expect(plan.pushSecrets).toEqual([]);
    expect(plan.toView).toEqual([]);
  });

  it("another member's envelopes in the same conversation are untouched", () => {
    const other = (e: IncomingChatEvent): IncomingChatEvent =>
      ({ ...e, senderKey: "someone-else" }) as IncomingChatEvent;
    const plan = planBackgroundEvents([other(chat), other(profile), other(groupName), other(pin)], retired, undefined, {
      sender: retired,
    });
    expect(plan.timeline).toHaveLength(1);
    expect(plan.profiles).toHaveLength(1);
    expect(plan.groupNames).toHaveLength(1);
    expect(plan.pins).toHaveLength(1);
  });

  it("a commit and a proposal are MLS state and are never dropped by sender", () => {
    const inert = { sender: () => true, senderKeyOfLeaf: () => "k" };
    expect(dropsFromSupersededSender(commit, () => true, inert)).toBe(false);
    expect(dropsFromSupersededSender(storedProposal, () => true, inert)).toBe(false);
    const plan = planBackgroundEvents([commit, storedProposal], () => true, undefined, inert);
    expect(plan.sawCommit).toBe(true);
    expect(plan.storedProposals).toHaveLength(1);
  });

  it("a leaf-only envelope whose sender cannot be named is not dropped", () => {
    expect(dropsFromSupersededSender(beacon, retired, { sender: retired })).toBe(false);
    expect(
      dropsFromSupersededSender(beacon, retired, { sender: retired, senderKeyOfLeaf: () => undefined }),
    ).toBe(false);
  });

  it("the open chat's own poll asks the same question (one rule for both consumers)", () => {
    const inert = { sender: retired, senderKeyOfLeaf: (leaf: number) => (leaf === 0 ? "k" : undefined) };
    for (const e of everything) expect(dropsFromSupersededSender(e, retired, inert)).toBe(true);
    for (const e of everything) {
      const live = { ...e, senderKey: "live", senderLeaf: 5 } as IncomingChatEvent;
      expect(dropsFromSupersededSender(live, retired, inert)).toBe(false);
    }
  });
});
