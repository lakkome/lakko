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

import { knockOriginFor, openKnockOrigins, pruneKnockOrigins, rememberKnockOrigin } from "./knock-origins";

describe("knock origins", () => {
  it("are kept per group: a second group's request has its own time", async () => {
    await openKnockOrigins("acct");
    await rememberKnockOrigin("group-a", "joiner", { askedAtMs: 1_000, issuerKeyB64: null });
    expect(knockOriginFor("group-b", "joiner")).toBeUndefined();
    await rememberKnockOrigin("group-b", "joiner", { askedAtMs: 2_000, issuerKeyB64: "issuer" });
    expect(knockOriginFor("group-a", "joiner")?.askedAtMs).toBe(1_000);
    expect(knockOriginFor("group-b", "joiner")).toEqual({ askedAtMs: 2_000, issuerKeyB64: "issuer" });
  });

  it("a request that stopped waiting gives up its time to the next one", async () => {
    await openKnockOrigins("acct");
    await rememberKnockOrigin("group-c", "joiner", { askedAtMs: 1_000, issuerKeyB64: "anna" });
    // Asked again while still waiting: the same request keeps its first time.
    await rememberKnockOrigin("group-c", "joiner", { askedAtMs: 5_000, issuerKeyB64: "anna" });
    expect(knockOriginFor("group-c", "joiner")?.askedAtMs).toBe(1_000);
    // Approved: no longer waiting, so the record goes; other groups keep theirs.
    await rememberKnockOrigin("group-d", "joiner", { askedAtMs: 3_000, issuerKeyB64: null });
    await pruneKnockOrigins("group-c", ["someone-else"]);
    expect(knockOriginFor("group-c", "joiner")).toBeUndefined();
    expect(knockOriginFor("group-d", "joiner")?.askedAtMs).toBe(3_000);
    // He left and asked again: dated by this request.
    await rememberKnockOrigin("group-c", "joiner", { askedAtMs: 9_000, issuerKeyB64: null }, true);
    expect(knockOriginFor("group-c", "joiner")).toEqual({ askedAtMs: 9_000, issuerKeyB64: null });
  });

  it("a newly kept request replaces a record no prune removed", async () => {
    await openKnockOrigins("acct");
    await rememberKnockOrigin("group-e", "joiner", { askedAtMs: 1_000, issuerKeyB64: "anna" });
    await rememberKnockOrigin("group-e", "joiner", { askedAtMs: 7_000, issuerKeyB64: null }, true);
    expect(knockOriginFor("group-e", "joiner")).toEqual({ askedAtMs: 7_000, issuerKeyB64: null });
  });
});
