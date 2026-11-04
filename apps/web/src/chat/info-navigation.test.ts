// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The back destinations of `docs/ux/conversation-info.md` section 3.1 on the
 * web: Add members returns to the group's info, the info to the
 * conversation, and the contact screen to the screen it covers.
 */
import { describe, expect, it } from "vitest";

import {
  contactBackState,
  contactBackTarget,
  coversOrigin,
  infoBackStep,
  infoParamOf,
  infoScreenOf,
} from "./info-navigation";

describe("info screens", () => {
  it("are read from the parameter, for a group only", () => {
    expect(infoScreenOf("group", true)).toBe("group");
    expect(infoScreenOf("add", true)).toBe("add-members");
    expect(infoScreenOf(undefined, true)).toBeNull();
    expect(infoScreenOf("other", true)).toBeNull();
    expect(infoScreenOf("group", false)).toBeNull();
    expect(infoScreenOf("add", false)).toBeNull();
  });

  it("round-trip through their parameter", () => {
    expect(infoScreenOf(infoParamOf("group"), true)).toBe("group");
    expect(infoScreenOf(infoParamOf("add-members"), true)).toBe("add-members");
  });

  it("step back: Add members to the info, the info to the conversation", () => {
    expect(infoBackStep("add-members")).toBe("group");
    expect(infoBackStep("group")).toBeNull();
  });
});

describe("the contact screen's back control", () => {
  it("returns to the screen recorded in the history entry", () => {
    expect(contactBackTarget(contactBackState("/groups/abc?info=group"))).toBe("/groups/abc?info=group");
    expect(contactBackTarget(contactBackState("/chat/xyz"))).toBe("/chat/xyz");
  });

  it("has no target without a recorded in-app path", () => {
    expect(contactBackTarget(null)).toBeNull();
    expect(contactBackTarget(undefined)).toBeNull();
    expect(contactBackTarget({})).toBeNull();
    expect(contactBackTarget({ lkBackTo: 7 })).toBeNull();
    expect(contactBackTarget({ lkBackTo: "https://example.com/" })).toBeNull();
    expect(contactBackTarget({ lkBackTo: "//example.com/x" })).toBeNull();
    expect(contactBackTarget({ lkBackTo: "/\\example.com" })).toBeNull();
  });

  it("covers a chat or a group's info, never the Contacts list beside it", () => {
    expect(coversOrigin("/chat/abc")).toBe(true);
    expect(coversOrigin("/groups/abc?info=group")).toBe(true);
    expect(coversOrigin("/")).toBe(true);
    expect(coversOrigin("/contacts")).toBe(false);
    expect(coversOrigin("/contacts/abc")).toBe(false);
  });
});
