// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { subscriberBand } from "./channel-facts";

describe("subscriberBand", () => {
  it("names the ADR-0059 bands at their edges", () => {
    expect([0, 9, 10, 99, 100, 999, 1000, 9999, 10_000, 99_999, 100_000].map(subscriberBand)).toEqual([
      "<10",
      "<10",
      "10+",
      "10+",
      "100+",
      "100+",
      "1k+",
      "1k+",
      "10k+",
      "10k+",
      "100k+",
    ]);
  });
});
