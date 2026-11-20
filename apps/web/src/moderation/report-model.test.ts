// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  acceptListVersion,
  afterSend,
  alsoKindFor,
  canSend,
  includedCounts,
  includedLine,
  newDraft,
  pickable,
  phaseAfterEdit,
  pickCandidatesFor,
  postOutcome,
  reasonKey,
  REPORT_COMMENT_MAX,
  REPORT_MAX_MESSAGES,
  showsEmptyHint,
  statusOf,
  togglePick,
  withComment,
  withReason,
  type ReportTargetSpec,
} from "./report-model";

const person: ReportTargetSpec = { kind: "person", accountKeyB64: "P", name: "Anna" };
const group: ReportTargetSpec = { kind: "group", groupIdB64: "G", handle: "club", name: "Club" };
const channel: ReportTargetSpec = { kind: "channel", channelIdHex: "00".repeat(16), name: "News" };

describe("a new draft", () => {
  it("blocks a person by default and leaves a group or channel unchecked", () => {
    expect(newDraft({ target: person, conversationKind: 1 })).toMatchObject({ also: "block", alsoChecked: true });
    expect(newDraft({ target: group, conversationKind: 2 })).toMatchObject({ also: "leave", alsoChecked: false });
    expect(newDraft({ target: channel, conversationKind: 3 })).toMatchObject({
      also: "unsubscribe",
      alsoChecked: false,
    });
  });

  it("shows no leave checkbox when there is nothing to leave", () => {
    expect(alsoKindFor(group, false)).toBeNull();
    expect(alsoKindFor(person, false)).toBe("block");
  });

  it("starts with no reason, so Send is off until one is chosen", () => {
    const d = newDraft({ target: person, conversationKind: 1 });
    expect(canSend(d, "editing")).toBe(false);
    const chosen = withReason(d, 3);
    expect(canSend(chosen, "editing")).toBe(true);
    expect(canSend(chosen, "sending")).toBe(false);
    expect(canSend(chosen, "failed")).toBe(true);
  });

  it("keeps Send off once the server said reporting is not available, whatever is edited", () => {
    const chosen = withReason(newDraft({ target: person, conversationKind: 1 }), 3);
    expect(canSend(chosen, "unavailable")).toBe(false);
    expect(phaseAfterEdit("unavailable")).toBe("unavailable");
    expect(phaseAfterEdit("sending")).toBe("sending");
    expect(phaseAfterEdit("failed")).toBe("editing");
    expect(phaseAfterEdit("limited")).toBe("editing");
  });

  it("ignores a reason code outside 1..8", () => {
    const d = newDraft({ target: person, conversationKind: 1 });
    expect(withReason(d, 0).reason).toBeNull();
    expect(withReason(d, 9).reason).toBeNull();
    expect(withReason(d, 8).reason).toBe(8);
  });

  it("keeps at most the limit of messages it is opened with", () => {
    const picked = Array.from({ length: REPORT_MAX_MESSAGES + 5 }, (_, i) => i + 1);
    expect(newDraft({ target: person, conversationKind: 1, picked }).picked).toHaveLength(REPORT_MAX_MESSAGES);
  });
});

describe("the comment", () => {
  it("is cut at the limit in characters, not bytes", () => {
    const d = newDraft({ target: person, conversationKind: 1 });
    const long = "ж".repeat(REPORT_COMMENT_MAX + 10);
    expect(Array.from(withComment(d, long).comment)).toHaveLength(REPORT_COMMENT_MAX);
    expect(withComment(d, "short").comment).toBe("short");
  });
});

describe("picking", () => {
  it("adds, removes, and refuses a tap past the limit", () => {
    let picked: readonly number[] = [];
    for (let i = 1; i <= REPORT_MAX_MESSAGES; i += 1) picked = togglePick(picked, i).picked;
    expect(picked).toHaveLength(REPORT_MAX_MESSAGES);
    const refused = togglePick(picked, 999);
    expect(refused.refused).toBe(true);
    expect(refused.picked).toBe(picked);
    const removed = togglePick(picked, 1);
    expect(removed.refused).toBe(false);
    expect(removed.picked).not.toContain(1);
  });

  it("offers only the reported author's received messages, never one's own", () => {
    const source = { kind: "chat" as const, groupIdB64: "G", authorKeyB64: "A" };
    expect(pickable({ incoming: true, deleted: false, authorKeyB64: "A" }, source, "ME")).toBe(true);
    expect(pickable({ incoming: true, deleted: false, authorKeyB64: "B" }, source, "ME")).toBe(false);
    expect(pickable({ incoming: true, deleted: true, authorKeyB64: "A" }, source, "ME")).toBe(false);
    expect(pickable({ incoming: false, deleted: false, authorKeyB64: "A" }, source, "ME")).toBe(false);
    expect(pickable({ incoming: true, deleted: false, authorKeyB64: undefined }, source, "ME")).toBe(false);
  });

  it("offers anyone's message but the reporter's own for a group target", () => {
    const source = { kind: "chat" as const, groupIdB64: "G", authorKeyB64: null };
    expect(pickable({ incoming: true, deleted: false, authorKeyB64: "B" }, source, "ME")).toBe(true);
    expect(pickable({ incoming: true, deleted: false, authorKeyB64: "ME" }, source, "ME")).toBe(false);
  });

  it("never picks chat rows for a channel source", () => {
    expect(
      pickable({ incoming: true, deleted: false, authorKeyB64: "A" }, { kind: "channel", channelIdHex: "x" }, null),
    ).toBe(false);
  });
});

describe("the Included line", () => {
  it("names nothing, the count, or the part that could be kept", () => {
    expect(includedLine(0, 0)).toEqual({ key: "report.included_none", args: {} });
    expect(includedLine(3, 3)).toEqual({ key: "report.included", args: { n: 3 } });
    expect(includedLine(2, 3)).toEqual({ key: "report.included_partial", args: { n: 2, total: 3 } });
  });

  it("counts fixed items as always kept", () => {
    const d = newDraft({
      target: person,
      conversationKind: 4,
      picked: [1, 2],
      fixed: [{ kind: "contact-request", wire: new Uint8Array([1]) }],
    });
    expect(includedCounts(d, 1)).toEqual({ kept: 2, total: 3 });
    expect(includedCounts(d, 9)).toEqual({ kept: 3, total: 3 });
  });

  it("shows the empty hint only with nothing included", () => {
    expect(showsEmptyHint(0)).toBe(true);
    expect(showsEmptyHint(1)).toBe(false);
  });
});

describe("after a send", () => {
  it("closes on success and runs the checkbox's action", () => {
    expect(afterSend({ kind: "sent", receiptId: "x".repeat(22) })).toEqual({ phase: "closed", runsAlso: true });
  });

  it("keeps the sheet on the network failure without running the action", () => {
    expect(afterSend({ kind: "failed" })).toEqual({ phase: "failed", runsAlso: false });
  });

  it("applies the block or leave on the daily limit and on a server without reporting", () => {
    expect(afterSend({ kind: "limited" })).toEqual({ phase: "limited", runsAlso: true });
    expect(afterSend({ kind: "unavailable" })).toEqual({ phase: "unavailable", runsAlso: true });
  });
});

describe("the server's answers", () => {
  it("reads a receipt only from a well-formed 201", () => {
    const id = "abcdefghijklmnopqrstuv";
    expect(postOutcome(201, { receipt_id: id })).toEqual({ kind: "sent", receiptId: id });
    expect(postOutcome(201, { receipt_id: "short" })).toEqual({ kind: "failed" });
    expect(postOutcome(429, { error: "rate-limited" })).toEqual({ kind: "limited" });
    expect(postOutcome(503, { error: "reporting-unavailable" })).toEqual({ kind: "unavailable" });
    expect(postOutcome(400, { error: "bad" })).toEqual({ kind: "failed" });
  });

  it("reads a status only from the three known values", () => {
    expect(statusOf(200, { status: "action" })).toBe("action");
    expect(statusOf(200, { status: "other" })).toBeNull();
    expect(statusOf(404, {})).toBeNull();
  });

  it("maps a reason name to its catalog key", () => {
    expect(reasonKey("child-safety")).toBe("report.reason.child_safety");
    expect(reasonKey("spam")).toBe("report.reason.spam");
  });
});

describe("the moderator list version floor", () => {
  it("accepts the first list and raises the floor", () => {
    expect(acceptListVersion(null, 3n)).toEqual({ accept: true, floor: 3n });
    expect(acceptListVersion(3n, 5n)).toEqual({ accept: true, floor: 5n });
    expect(acceptListVersion(5n, 5n)).toEqual({ accept: true, floor: 5n });
  });

  it("refuses a list older than one already seen", () => {
    expect(acceptListVersion(5n, 4n)).toEqual({ accept: false, floor: 5n });
  });
});

describe("the Choose messages list", () => {
  const c = (id: number, author: string | undefined, deleted = false) => ({
    id,
    authorKeyB64: author,
    text: `m${id}`,
    filesLabel: "",
    createdAt: "2026-11-08T00:00:00Z",
    deleted,
  });

  it("lists the reported author's messages, newest first, without deleted ones", () => {
    const list = pickCandidatesFor(
      [c(1, "A"), c(3, "A"), c(2, "B"), c(4, "A", true), c(5, "ME")],
      { kind: "chat", groupIdB64: "G", authorKeyB64: "A" },
      "ME",
    );
    expect(list.map((x) => x.id)).toEqual([3, 1]);
  });

  it("lists anyone's but the reporter's own for a group target", () => {
    const list = pickCandidatesFor(
      [c(1, "A"), c(2, "B"), c(3, "ME")],
      { kind: "chat", groupIdB64: "G", authorKeyB64: null },
      "ME",
    );
    expect(list.map((x) => x.id)).toEqual([2, 1]);
  });

  it("lists a channel's readable posts", () => {
    const list = pickCandidatesFor([c(10, undefined), c(11, undefined, true)], { kind: "channel", channelIdHex: "x" }, "ME");
    expect(list.map((x) => x.id)).toEqual([10]);
  });
});
