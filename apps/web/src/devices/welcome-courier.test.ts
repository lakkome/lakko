// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A history poll that ends without a bundle says so in one plain line (seen
 * live 2026-11-23: a device added from web got no history and the log showed
 * nothing about it). The same lines as iOS and Android.
 */
import { describe, expect, it } from "vitest";
import { historyOutcomeLogLine } from "./welcome-courier";

describe("the history poll outcome line", () => {
  it("a timeout is logged as a plain line", () => {
    expect(historyOutcomeLogLine({ kind: "timeout" })).toBe(
      "history catch-up: no bundle arrived before the timeout",
    );
    expect(historyOutcomeLogLine({ kind: "cancelled" })).toBe("history catch-up: cancelled");
  });

  it("a received bundle logs nothing here", () => {
    expect(historyOutcomeLogLine({ kind: "received", sealedBytes: new Uint8Array([1]) })).toBeNull();
  });
});
