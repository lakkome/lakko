// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/second-device.md section 2.1, "When it is done", scan route: a
 * linked device sends the screen straight back to the Devices list, read
 * again, with no "added" line.
 */
import { describe, expect, it } from "vitest";
import { afterScanLinked } from "./scan-linked";

describe("after a scanned device is linked", () => {
  it("returns to the list and reads it again, with no line when the keys were sent", () => {
    expect(afterScanLinked({ recoveryPublished: true })).toEqual({
      add: "closed",
      reloadList: true,
      keysNotSent: false,
    });
  });

  it("still returns to the list when no keys were sent, keeping only the keys-not-sent line", () => {
    expect(afterScanLinked({ recoveryPublished: false })).toEqual({
      add: "closed",
      reloadList: true,
      keysNotSent: true,
    });
  });
});
