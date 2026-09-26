// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { dedupByMessageId, type TimelineRow } from "./timeline-row";

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
