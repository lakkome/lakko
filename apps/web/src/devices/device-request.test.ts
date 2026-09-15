// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { comparisonCode } from "./device-request";

describe("comparisonCode", () => {
  it("is six digits in three pairs, deterministic, and payload-bound", async () => {
    const a = new Uint8Array([1, 2, 3, 4, 5]);
    const b = new Uint8Array([1, 2, 3, 4, 6]);
    const ca = await comparisonCode(a);
    expect(ca).toMatch(/^\d\d \d\d \d\d$/);
    expect(await comparisonCode(a)).toBe(ca);
    expect(await comparisonCode(b)).not.toBe(ca);
  });
});
