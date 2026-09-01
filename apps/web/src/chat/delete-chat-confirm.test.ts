// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6.2.4 — the `Delete this chat?` confirmation.
 *
 * The strings already lived in `membership-ended.ts` and are already pinned
 * there; this module adds only the two things §6.2.4 leaves to the widget and
 * `window.confirm` could not express — the destructive action's LABEL, which
 * names the chat, and the tone that colours it.
 *
 * This is the one control in the whole design that destroys user data on
 * request, with no undo and no export, so its confirmation is the only
 * protection there is.
 */

import { describe, expect, it } from "vitest";

import {
  DELETE_CHAT_BODY,
  DELETE_CHAT_TITLE,
} from "./membership-ended";
import {
  DELETE_CHAT_CANCEL_LABEL,
  DELETE_CHAT_CONFIRM_LABEL,
  deleteChatConfirmation,
} from "./delete-chat-confirm";

describe("the delete-chat confirmation", () => {
  it("reuses §6.2.4's already-pinned copy rather than restating it", () => {
    // The strings have one definition. A second literal here would be a
    // second place for them to drift, which is the failure mode the
    // `membership-ended.ts` split exists to prevent.
    const c = deleteChatConfirmation({ chatLabel: "Ops team" });
    expect(c.title).toBe(DELETE_CHAT_TITLE);
    expect(c.body).toBe(DELETE_CHAT_BODY);
  });

  it("keeps the body exact rather than sweeping", () => {
    // §6.2.4: the precision is load-bearing — the messages really are gone,
    // and the dead MLS handle is not. Pinned as a literal here because this
    // is the sentence a user reads before losing a transcript that exists
    // nowhere else.
    expect(deleteChatConfirmation({ chatLabel: null }).body).toBe(
      "This removes the conversation and its messages from this device. " +
        "There is no copy anywhere else and this cannot be undone.",
    );
  });

  it("names the chat in the action button", () => {
    expect(deleteChatConfirmation({ chatLabel: "Ops team" }).confirmLabel).toBe(
      "Delete Ops team",
    );
    expect(DELETE_CHAT_CONFIRM_LABEL).toBe("Delete");
    expect(DELETE_CHAT_CANCEL_LABEL).toBe("Cancel");
  });

  it("falls back to the bare verb for an unnamed chat", () => {
    for (const blank of [null, undefined, "", "   "]) {
      expect(deleteChatConfirmation({ chatLabel: blank }).confirmLabel).toBe(
        "Delete",
      );
    }
  });

  it("is destructive", () => {
    expect(deleteChatConfirmation({ chatLabel: "Ops team" }).tone).toBe(
      "danger",
    );
  });
});
