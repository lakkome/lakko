// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/message-actions.md` section 8 — selection mode: the gate, the cap,
 * the count, the two buttons and every string the spec pins.
 */
import { describe, expect, it } from "vitest";

import {
  BULK_DELETE_CANCEL_LABEL,
  BULK_DELETE_CONFIRM_LABEL,
  DELETE_HELPER,
  SELECTION_LIMIT,
  atSelectionLimit,
  bulkDeleteConfirmation,
  canSelectRow,
  confirmDeleteLabel,
  deleteDecision,
  enterSelection,
  forwardEnabled,
  leaveSelection,
  oldestFirst,
  progressLabel,
  selectionCountLabel,
  selectionMarkName,
  toggleSelected,
  withoutDone,
  type Selection,
} from "./selection";

function ofSize(n: number): Selection {
  return new Set(Array.from({ length: n }, (_, i) => i + 1));
}

describe("canSelectRow — the gates the single actions already compute", () => {
  it("a message with an id, not deleted, whose send is complete", () => {
    expect(canSelectRow({ hasId: true, deleted: false, sendComplete: true })).toBe(true);
  });
  it("no id: a date separator, a system row, a pending outgoing bubble", () => {
    expect(canSelectRow({ hasId: false, deleted: false, sendComplete: true })).toBe(false);
  });
  it("a deleted message", () => {
    expect(canSelectRow({ hasId: true, deleted: true, sendComplete: true })).toBe(false);
  });
  it("a send that has not completed or has failed (the Forward gate)", () => {
    expect(canSelectRow({ hasId: true, deleted: false, sendComplete: false })).toBe(false);
  });
});

describe("entering and leaving", () => {
  it("Select enters with the message it was opened on already selected", () => {
    expect([...enterSelection(42)]).toEqual([42]);
  });
  it("leaving clears the selection", () => {
    expect(leaveSelection().size).toBe(0);
  });
});

describe("toggleSelected and the limit", () => {
  it("adds, then removes, returning a new set each time", () => {
    const empty = leaveSelection();
    const one = toggleSelected(empty, 7);
    expect([...one]).toEqual([7]);
    expect(one).not.toBe(empty);
    const none = toggleSelected(one, 7);
    expect(none.size).toBe(0);
    expect(none).not.toBe(one);
  });
  it("the limit is 100", () => {
    expect(SELECTION_LIMIT).toBe(100);
  });
  it("at the limit a further tap does nothing and the same set comes back", () => {
    const full = ofSize(SELECTION_LIMIT);
    expect(atSelectionLimit(full)).toBe(true);
    const after = toggleSelected(full, 1000);
    expect(after).toBe(full);
    expect(after.size).toBe(SELECTION_LIMIT);
  });
  it("at the limit deselecting still works", () => {
    const full = ofSize(SELECTION_LIMIT);
    const after = toggleSelected(full, 1);
    expect(after.size).toBe(SELECTION_LIMIT - 1);
    expect(after.has(1)).toBe(false);
    expect(atSelectionLimit(after)).toBe(false);
  });
  it("one below the limit still admits one more", () => {
    const almost = ofSize(SELECTION_LIMIT - 1);
    expect(atSelectionLimit(almost)).toBe(false);
    expect(toggleSelected(almost, 1000).size).toBe(SELECTION_LIMIT);
  });
});

describe("selectionCountLabel — the count in the middle of the bar", () => {
  it('"1 selected", "12 selected"', () => {
    expect(selectionCountLabel(1)).toBe("1 selected");
    expect(selectionCountLabel(12)).toBe("12 selected");
  });
  it('at the cap: "100 selected, the limit"', () => {
    expect(selectionCountLabel(100)).toBe("100 selected, the limit");
  });
  it("never below zero", () => {
    expect(selectionCountLabel(-3)).toBe("0 selected");
  });
});

describe("deleteDecision — enabled only when every selected message is own", () => {
  it("empty: disabled, nothing to explain", () => {
    expect(deleteDecision([])).toEqual({ enabled: false, helper: null });
  });
  it("all own: enabled, no helper", () => {
    expect(deleteDecision([{ own: true }, { own: true }])).toEqual({ enabled: true, helper: null });
  });
  it("one of someone else's: disabled with the helper line", () => {
    expect(deleteDecision([{ own: true }, { own: false }])).toEqual({
      enabled: false,
      helper: DELETE_HELPER(),
    });
    expect(DELETE_HELPER()).toBe("Only your own messages can be deleted.");
  });
});

describe("forwardEnabled — when the selection is not empty", () => {
  it("empty is disabled, one is enabled", () => {
    expect(forwardEnabled(leaveSelection())).toBe(false);
    expect(forwardEnabled(enterSelection(3))).toBe(true);
  });
});

describe("the confirmation", () => {
  it('"Delete 12 messages?" / "Delete 1 message?"', () => {
    expect(confirmDeleteLabel(12)).toBe("Delete 12 messages?");
    expect(confirmDeleteLabel(1)).toBe("Delete 1 message?");
  });
  it("a destructive Delete and a Cancel, no body", () => {
    expect(bulkDeleteConfirmation(3)).toEqual({
      title: "Delete 3 messages?",
      body: null,
      confirmLabel: BULK_DELETE_CONFIRM_LABEL(),
      cancelLabel: BULK_DELETE_CANCEL_LABEL(),
      tone: "danger",
    });
    expect(BULK_DELETE_CONFIRM_LABEL()).toBe("Delete");
    expect(BULK_DELETE_CANCEL_LABEL()).toBe("Cancel");
  });
});

describe("progressLabel — what a stopped bulk action says", () => {
  it('"Deleted 5 of 12." / "Forwarded 5 of 12."', () => {
    expect(progressLabel("deleted", 5, 12)).toBe("Deleted 5 of 12.");
    expect(progressLabel("forwarded", 5, 12)).toBe("Forwarded 5 of 12.");
  });
});

describe("the order and the leftover", () => {
  it("oldest first: ascending DS id, whatever order they were selected in", () => {
    expect(oldestFirst(new Set([30, 4, 17]))).toEqual([4, 17, 30]);
  });
  it("the messages that were acted on leave the selection; the rest stay", () => {
    const left = withoutDone(new Set([4, 17, 30]), [4, 17]);
    expect([...left]).toEqual([30]);
  });
});

describe("selectionMarkName — the message's text or kind", () => {
  const bubbleOf = (kind: "voice" | "photo"): "voice" | "photo" => kind;
  it("the text on one line", () => {
    expect(selectionMarkName({ text: "  two\nlines ", attachments: [] }, bubbleOf)).toBe("two lines");
  });
  it("the kind of the first attachment when there is no text", () => {
    expect(selectionMarkName({ text: "", attachments: ["voice" as const] }, bubbleOf)).toBe(
      "Voice message",
    );
    expect(selectionMarkName({ text: "", attachments: ["photo" as const] }, bubbleOf)).toBe("Photo");
  });
  it("neither: the same fallback a missing quote gets", () => {
    expect(selectionMarkName({ text: "", attachments: [] }, bubbleOf)).toBe("Message");
  });
});
