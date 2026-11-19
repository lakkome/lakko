// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 live walk 2026-11-19 - a send refused because this device is
 * behind is recognised through its wrappers, so log sites stay quiet on it.
 */
import { describe, expect, it } from "vitest";

import { ConversationBehindError, isConversationBehind } from "./client";
import { ContactsError } from "../contacts/client";

describe("isConversationBehind", () => {
  it("recognises the refusal, plain, as a cause, and inside a ContactsError", () => {
    const behind = new ConversationBehindError();
    expect(isConversationBehind(behind)).toBe(true);
    expect(isConversationBehind(new Error("outer", { cause: behind }))).toBe(true);
    expect(isConversationBehind(ContactsError.wrap(behind))).toBe(true);
  });

  it("leaves every other failure alone", () => {
    expect(isConversationBehind(new Error("network"))).toBe(false);
    expect(isConversationBehind(ContactsError.wrap(new Error("x")))).toBe(false);
    expect(isConversationBehind(undefined)).toBe(false);
    expect(isConversationBehind("text")).toBe(false);
  });
});
