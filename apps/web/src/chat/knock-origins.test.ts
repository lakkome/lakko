// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** `chat/knock-origins.ts` — where a request to join came from is kept per
 *  request (group and joiner): one person asking two groups is two requests,
 *  and the second group's line never reads the first group's time. */
import { describe, expect, it, vi } from "vitest";

const rows = new Map<string, unknown>();
vi.mock("../storage/account-kv", () => ({
  loadAccountJson: async (account: string, row: string) => structuredClone(rows.get(`${account}/${row}`) ?? null),
  saveAccountJson: async (account: string, row: string, body: unknown) => {
    rows.set(`${account}/${row}`, structuredClone(body));
  },
}));

import { knockOriginFor, openKnockOrigins, rememberKnockOrigin } from "./knock-origins";

describe("knock origins", () => {
  it("are kept per group: a second group's request has its own time", async () => {
    await openKnockOrigins("acct");
    await rememberKnockOrigin("group-a", "joiner", { askedAtMs: 1_000, issuerKeyB64: null });
    expect(knockOriginFor("group-b", "joiner")).toBeUndefined();
    await rememberKnockOrigin("group-b", "joiner", { askedAtMs: 2_000, issuerKeyB64: "issuer" });
    expect(knockOriginFor("group-a", "joiner")?.askedAtMs).toBe(1_000);
    expect(knockOriginFor("group-b", "joiner")).toEqual({ askedAtMs: 2_000, issuerKeyB64: "issuer" });
  });
});
