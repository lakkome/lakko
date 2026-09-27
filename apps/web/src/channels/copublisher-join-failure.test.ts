// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The co-publisher join screen's failure mapping: the knock's three sentences
 * shared with the `/g/` join and the natives, `Try again` only on the general
 * one, never the raw error; a retired v1 link keeps its own fixed sentence.
 */
import { describe, expect, it } from "vitest";

import { coPublisherJoinFailure } from "./copublisher-join-failure";
import { LegacyChannelError } from "./legacy-v1";
import { GroupNotClaimedError } from "../ds/access";
import { JOIN_REQUEST_FAILED } from "../routes/join-group-failure";

describe("coPublisherJoinFailure", () => {
  it("a publisher group that returned no epoch", () => {
    expect(coPublisherJoinFailure(new GroupNotClaimedError())).toEqual({
      message: "This group isn't taking requests yet. Try again later.",
      offerRetry: false,
    });
  });

  it("a refusal explained by the device clock", () => {
    expect(
      coPublisherJoinFailure({ name: "DsHttpError", status: 401, clockSkewed: true }),
    ).toEqual({ message: "Check this device's date and time.", offerRetry: false });
  });

  it("anything else: the general sentence with Try again, never the raw error", () => {
    const n = coPublisherJoinFailure(new Error("ds /pow/challenge -> 503: overloaded"));
    expect(n).toEqual({ message: JOIN_REQUEST_FAILED, offerRetry: true });
    expect(n.message).not.toContain("overloaded");
  });

  it("a retired v1 link keeps its own fixed sentence, with no retry", () => {
    const err = new LegacyChannelError("invite-link");
    expect(coPublisherJoinFailure(err)).toEqual({ message: err.message, offerRetry: false });
  });
});
