// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { lastEditableOwnMessage, upEditsLastMessage } from "./edit-last";
import type { MessageOpState } from "./message-ops";
import type { TimelineRow } from "./timeline-row";

type Outgoing = Extract<TimelineRow, { kind: "outgoing" }>;

function own(id: number, over: Partial<Outgoing> = {}): TimelineRow {
  return {
    kind: "outgoing",
    tempId: id,
    plaintext: `m${id}`,
    attachments: [],
    ack: "sent",
    messageId: id,
    ...over,
  };
}

function unsent(tempId: number, ack: "pending" | "failed"): TimelineRow {
  return { kind: "outgoing", tempId, plaintext: `m${tempId}`, attachments: [], ack };
}

function incoming(messageId: number): TimelineRow {
  return {
    kind: "incoming",
    event: { kind: "application", messageId, text: "x" },
  } as unknown as TimelineRow;
}

const op = (v: Partial<MessageOpState>): MessageOpState => v as MessageOpState;

describe("upEditsLastMessage", () => {
  const base = {
    desktop: true,
    key: "ArrowUp",
    modified: false,
    composing: false,
    empty: true,
    editing: false,
    actionsAvailable: true,
  };
  it("is a bare Up in an empty composer, on the desktop", () => {
    expect(upEditsLastMessage(base)).toBe(true);
  });
  it("never in the browser build", () => {
    expect(upEditsLastMessage({ ...base, desktop: false })).toBe(false);
  });
  it("not with text, a modifier, an open edit, or a closed conversation", () => {
    expect(upEditsLastMessage({ ...base, empty: false })).toBe(false);
    expect(upEditsLastMessage({ ...base, modified: true })).toBe(false);
    expect(upEditsLastMessage({ ...base, composing: true })).toBe(false);
    expect(upEditsLastMessage({ ...base, editing: true })).toBe(false);
    expect(upEditsLastMessage({ ...base, actionsAvailable: false })).toBe(false);
    expect(upEditsLastMessage({ ...base, key: "ArrowDown" })).toBe(false);
  });
});

describe("lastEditableOwnMessage", () => {
  it("picks the newest own sent message, skipping incoming ones", () => {
    const rows = [own(1), own(2), incoming(3)];
    expect(lastEditableOwnMessage(rows, () => undefined)).toEqual({ messageId: 2, text: "m2" });
  });

  it("skips pending, failed, deleted and older-segment rows", () => {
    const ops = new Map([[5, op({ deleted: true })]]);
    const rows = [
      own(1),
      own(-7),
      unsent(4, "pending"),
      own(5),
      unsent(6, "failed"),
    ];
    expect(lastEditableOwnMessage(rows, (id) => ops.get(id))).toEqual({ messageId: 1, text: "m1" });
  });

  it("edits from the edited text", () => {
    const ops = new Map([[2, op({ editedText: "fixed" })]]);
    expect(lastEditableOwnMessage([own(2)], (id) => ops.get(id))).toEqual({
      messageId: 2,
      text: "fixed",
    });
  });

  it("is null when there is nothing to edit", () => {
    expect(lastEditableOwnMessage([], () => undefined)).toBeNull();
    expect(lastEditableOwnMessage([incoming(1)], () => undefined)).toBeNull();
  });
});
