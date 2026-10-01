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
    const n = coPublisherJoinFailure(new GroupNotClaimedError());
    expect(n.render()).toBe("This group isn't taking requests yet. Try again later.");
    expect(n.offerRetry).toBe(false);
  });

  it("a refusal explained by the device clock", () => {
    const n = coPublisherJoinFailure({ name: "DsHttpError", status: 401, clockSkewed: true });
    expect(n.render()).toBe("Check this device's date and time.");
    expect(n.offerRetry).toBe(false);
  });

  it("anything else: the general sentence with Try again, never the raw error", () => {
    const n = coPublisherJoinFailure(new Error("ds /pow/challenge -> 503: overloaded"));
    expect(n.render()).toBe(JOIN_REQUEST_FAILED());
    expect(n.offerRetry).toBe(true);
    expect(n.render()).not.toContain("overloaded");
  });

  it("a retired v1 link keeps its own fixed sentence, with no retry", () => {
    const err = new LegacyChannelError("invite-link");
    const n = coPublisherJoinFailure(err);
    expect(n.render()).toBe(err.message);
    expect(n.offerRetry).toBe(false);
  });
});
