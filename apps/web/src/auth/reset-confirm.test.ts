// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The typed confirmation and the inline failure line of "Reset account keys"
 * (`reset-confirm.ts`, identity-recovery UX section 15.1).
 */
import { describe, expect, it } from "vitest";

import { RESET_CONFIRM_WORD, resetConfirmed, resetStartFailure } from "./reset-confirm";

describe("resetConfirmed", () => {
  it("accepts exactly the word, with surrounding whitespace forgiven", () => {
    expect(RESET_CONFIRM_WORD).toBe("RESET");
    expect(resetConfirmed("RESET")).toBe(true);
    expect(resetConfirmed(" RESET ")).toBe(true);
    expect(resetConfirmed("RESET\n")).toBe(true);
  });

  it("refuses anything else: case, a partial word, an empty field, extra words", () => {
    for (const typed of ["", "reset", "Reset", "RESE", "RESETT", "RESET keys", "R E S E T"]) {
      expect(resetConfirmed(typed)).toBe(false);
    }
  });
});

describe("resetStartFailure", () => {
  it("names the limit first, then offline, then the rest", () => {
    expect(resetStartFailure({ rateLimitedSecs: 30, offline: false })).toBe("rate-limited");
    expect(resetStartFailure({ rateLimitedSecs: 30, offline: true })).toBe("rate-limited");
    expect(resetStartFailure({ rateLimitedSecs: null, offline: true })).toBe("offline");
    expect(resetStartFailure({ rateLimitedSecs: null, offline: false })).toBe("other");
  });
});
