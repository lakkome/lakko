// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Security follow-up of 2026-11-21: anyone who learns a rendezvous id can
 * write rows to it (the write spends a token, the read is open), so every
 * reader passes over a row that does not open or join and reads on, instead
 * of taking the first row as the answer. These tests drive each pairing
 * reader over a rendezvous whose first rows are junk.
 */
import { describe, expect, it } from "vitest";

import type { DsClient, FetchResult } from "../ds/client";
import { joinFirstWelcome, pollForHistoryBundle, pollForWelcome } from "./welcome-courier";
import { pollForRecoveryPayload } from "./recovery-payload";
import { pollForStateBundle, wrapStateRow } from "./state-bundle";

const RENDEZVOUS = new Uint8Array(32).fill(0x5a);

/** A rendezvous journal holding `rows` (ids 1..n), read as the DS reads it. */
function fakeDs(rows: readonly Uint8Array[]): { ds: DsClient; reads: { after: number }[] } {
  const reads: { after: number }[] = [];
  const ds = {
    async fetchRendezvous(
      _id: Uint8Array,
      options: { after?: number; limit?: number } = {},
    ): Promise<FetchResult> {
      const after = options.after ?? 0;
      const limit = options.limit ?? 100;
      reads.push({ after });
      const slice = rows.slice(after, after + limit);
      return {
        messages: slice.map((payload, i) => ({ id: after + i + 1, payload, createdAt: "t" })),
        hasMore: after + limit < rows.length,
      };
    },
  };
  return { ds: ds as unknown as DsClient, reads };
}

const JUNK = new Uint8Array([0xee, 0xee]);
const recoveryTagged = (body: number): Uint8Array => {
  const prefix = new TextEncoder().encode("lakko/recovery-payload/v3");
  const row = new Uint8Array(prefix.length + 1);
  row.set(prefix, 0);
  row[prefix.length] = body;
  return row;
};

describe("the Welcome", () => {
  it("a poll past a row already taken reads the next one", async () => {
    const { ds } = fakeDs([JUNK, new Uint8Array([1])]);
    const outcome = await pollForWelcome(RENDEZVOUS, { ds, after: 1, timeoutMs: 1_000 });
    expect(outcome).toEqual({ kind: "received", welcome: new Uint8Array([1]), welcomeCursor: 2 });
  });

  it("a row that does not join is passed over: the Welcome after it is joined", async () => {
    const { ds } = fakeDs([JUNK, JUNK, new Uint8Array([1])]);
    const tried: number[] = [];
    const joined = await joinFirstWelcome(
      RENDEZVOUS,
      { welcome: JUNK, welcomeCursor: 1 },
      (bytes) => {
        tried.push(bytes[0] ?? 0);
        if (bytes[0] !== 1) throw new Error("not a welcome");
        return "group";
      },
      { ds, timeoutMs: 1_000 },
    );
    expect(joined).toEqual({ joined: "group", welcomeCursor: 3 });
    expect(tried).toEqual([0xee, 0xee, 1]);
  });

  it("a Welcome that joins at once reads nothing more", async () => {
    const { ds, reads } = fakeDs([new Uint8Array([1])]);
    const joined = await joinFirstWelcome(
      RENDEZVOUS,
      { welcome: new Uint8Array([1]), welcomeCursor: 1 },
      () => "group",
      { ds, timeoutMs: 1_000 },
    );
    expect(joined.welcomeCursor).toBe(1);
    expect(reads).toEqual([]);
  });

  it("only rows that do not join: the first refusal is thrown when the window ends", async () => {
    const { ds } = fakeDs([JUNK, JUNK]);
    await expect(
      joinFirstWelcome(
        RENDEZVOUS,
        { welcome: JUNK, welcomeCursor: 1 },
        () => {
          throw new Error("first refusal");
        },
        { ds, timeoutMs: 50 },
      ),
    ).rejects.toThrow("first refusal");
  });
});

describe("the bundles after the Welcome", () => {
  it("the history poll passes over a row that does not open", async () => {
    const { ds } = fakeDs([new Uint8Array([1]), JUNK, JUNK, new Uint8Array([9])]);
    const outcome = await pollForHistoryBundle(RENDEZVOUS, 1, {
      ds,
      timeoutMs: 1_000,
      accept: (sealed) => sealed[0] === 9,
    });
    expect(outcome).toEqual({ kind: "received", sealedBytes: new Uint8Array([9]) });
  });

  it("the recovery poll passes over a tagged row that does not open", async () => {
    const good = recoveryTagged(9);
    const { ds } = fakeDs([new Uint8Array([1]), recoveryTagged(1), JUNK, good]);
    const outcome = await pollForRecoveryPayload(RENDEZVOUS, 1, {
      ds,
      timeoutMs: 1_000,
      accept: (row) => row[row.length - 1] === 9,
    });
    expect(outcome).toEqual({ kind: "received", row: good });
  });

  it("the state poll passes over a tagged row that does not open", async () => {
    const { ds } = fakeDs([
      new Uint8Array([1]),
      wrapStateRow(new Uint8Array([2])),
      JUNK,
      wrapStateRow(new Uint8Array([9])),
    ]);
    const outcome = await pollForStateBundle(RENDEZVOUS, 1, {
      ds,
      timeoutMs: 1_000,
      accept: (sealed) => sealed[0] === 9,
    });
    expect(outcome).toEqual({ kind: "received", sealed: new Uint8Array([9]) });
  });

  it("without a good row the polls time out instead of taking junk", async () => {
    const { ds } = fakeDs([new Uint8Array([1]), wrapStateRow(new Uint8Array([2])), recoveryTagged(1)]);
    expect(
      await pollForStateBundle(RENDEZVOUS, 1, { ds, timeoutMs: 50, accept: () => false }),
    ).toEqual({ kind: "timeout" });
    expect(
      await pollForRecoveryPayload(RENDEZVOUS, 1, { ds, timeoutMs: 50, accept: () => false }),
    ).toEqual({ kind: "timeout" });
  });
});
