// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { linkAnswerAction } from "./accept-request";

describe("linkAnswerAction (ADR-0079 section 3)", () => {
  it("accepts with no card only when the register says the link is live", () => {
    expect(linkAnswerAction(true, "accept")).toBe("accept");
  });

  it("drops a request already accepted through the link (another device answered)", () => {
    expect(linkAnswerAction(true, "answered")).toBe("drop");
  });

  it("shows a card with the note for a link that no longer works or a fold that never loaded", () => {
    expect(linkAnswerAction(true, "dead")).toBe("card-dead-link");
    expect(linkAnswerAction(true, null)).toBe("card-dead-link");
  });

  it("leaves a request without a link answer unchanged", () => {
    expect(linkAnswerAction(false, null)).toBe("card");
    expect(linkAnswerAction(false, "accept")).toBe("card");
  });
});
