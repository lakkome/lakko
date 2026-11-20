// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { dedupByMessageId, seedTimelineFromLog, type TimelineRow } from "./timeline-row";

function out(messageId: number | undefined, tempId: number): TimelineRow {
  return {
    kind: "outgoing",
    tempId,
    plaintext: "x",
    attachments: [],
    ack: messageId === undefined ? "pending" : "sent",
    ...(messageId === undefined ? {} : { messageId }),
  };
}

function inc(messageId: number): TimelineRow {
  return {
    kind: "incoming",
    event: {
      kind: "application",
      messageId,
      text: "y",
      plaintext: "y",
      attachments: [],
      senderLeaf: 1,
      senderKey: "k",
      createdAt: "2026-09-26T00:00:00Z",
    },
  };
}

describe("dedupByMessageId", () => {
  it("keeps one row per DS id, the first one, in order", () => {
    const rows = [out(7, 1), inc(8), out(7, 2), out(7, 3), inc(8)];
    expect(dedupByMessageId(rows).map((r) => (r.kind === "outgoing" ? `out:${r.tempId}` : "in:8"))).toEqual([
      "out:1",
      "in:8",
    ]);
  });

  it("keeps every unsent bubble, which has no id yet", () => {
    const rows = [out(undefined, 1), out(undefined, 2), out(9, 3)];
    expect(dedupByMessageId(rows)).toHaveLength(3);
  });

  it("returns the rows untouched when there is nothing to collapse", () => {
    const rows = [inc(1), out(2, 1), inc(3)];
    expect(dedupByMessageId(rows)).toEqual(rows);
  });
});

function commit(messageId: number): TimelineRow {
  return { kind: "incoming", event: { kind: "system", messageId, systemKind: "commit", createdAt: "t" } } as TimelineRow;
}

function ids(rows: readonly TimelineRow[]): string[] {
  return rows.map((r) =>
    r.kind === "outgoing" ? `out:${r.messageId ?? "pending"}` : `in:${r.event.messageId}`,
  );
}

describe("seedTimelineFromLog", () => {
  it("is the stored rows when the view holds nothing yet", () => {
    const stored = [out(1, -1), inc(2)];
    expect(seedTimelineFromLog([], stored)).toEqual(stored);
  });

  it("keeps the stored history when a row reached the view first", () => {
    // 2026-11-20: a one-to-one whose other person had deleted their account
    // showed only this person's own messages. A view that already holds a row
    // when the log is read must still show the stored "hello" (2).
    const stored = [out(1, -1), inc(2), commit(3)];
    const current = [out(4, 7)];
    expect(ids(seedTimelineFromLog(current, stored))).toEqual(["out:1", "in:2", "in:3", "out:4"]);
  });

  it("keeps the view's own copy of a row it already holds", () => {
    const live = inc(2);
    const stored = [inc(1), inc(2)];
    const seeded = seedTimelineFromLog([live], stored);
    expect(ids(seeded)).toEqual(["in:1", "in:2"]);
    expect(seeded[1]).toBe(live);
  });

  it("leaves a bubble with no DS id yet last", () => {
    const stored = [inc(1), inc(2)];
    const current = [out(undefined, 9)];
    expect(ids(seedTimelineFromLog(current, stored))).toEqual(["in:1", "in:2", "out:pending"]);
  });
});
