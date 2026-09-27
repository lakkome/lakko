// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `/g/` join screen's failure copy: four fixed sentences shared with iOS
 * and Android, and never the raw error.
 */
import { describe, expect, it } from "vitest";

import { GroupNotClaimedError } from "../ds/access";
import {
  JOIN_REQUEST_FAILED,
  JOIN_SUCCEEDED,
  JOIN_SUCCEEDED_OPENING,
  JOIN_TRY_AGAIN,
  joinFailureNotice,
} from "./join-group-failure";

describe("joinFailureNotice", () => {
  it("a group that returned no epoch", () => {
    // Wrapped the way `ContactsError.wrap` wraps it.
    const err = { variant: { kind: "wrapped", cause: new GroupNotClaimedError() } };
    expect(joinFailureNotice(err)).toEqual({
      message: "This group isn't taking requests yet. Try again later.",
      offerRetry: false,
    });
  });

  it("a refusal explained by the device clock", () => {
    const err = { cause: { status: 401, clockSkewed: true } };
    expect(joinFailureNotice(err)).toEqual({
      message: "Check this device's date and time.",
      offerRetry: false,
    });
  });

  it("anything else: the fixed sentence and Try again, never the raw error", () => {
    const n = joinFailureNotice(new Error("ds /ds/group/x/knock -> 500: boom"));
    expect(n).toEqual({ message: JOIN_REQUEST_FAILED, offerRetry: true });
    expect(n.message).not.toContain("boom");
    expect(JOIN_REQUEST_FAILED).toBe(
      "Your request didn't go through. Check your connection and try again.",
    );
    expect(JOIN_TRY_AGAIN).toBe("Try again");
  });

  it("the success copy has no exclamation mark", () => {
    expect(JOIN_SUCCEEDED).toBe("You joined the group.");
    expect(JOIN_SUCCEEDED_OPENING).toBe("You joined the group. Opening the chat...");
  });
});
