// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 step 3: the anonymous-token pool of a suspended account. It draws
 * `blob-read` alone, with the restricted session (files in chats still
 * open), and reads a `403 account-suspended` from the issuer as "not now":
 * no retry before the backoff, a take refused at once.
 */
import { describe, expect, it } from "vitest";

import { AnonTokensSuspendedError } from "./anon-token-pool";
import { fakeTokenPool } from "./anon-token-test-doubles";

const S1 = "s1.QUNDT1VOVA.0.dev.1.2.tag";

async function settle(): Promise<void> {
  for (let i = 0; i < 20; i += 1) await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function issued(issuer: ReturnType<typeof fakeTokenPool>["issuer"]): string[] {
  return issuer
    .filter((r) => r.url === "/api/auth/auth/tokens")
    .map((r) => (r.body as { context: string }).context);
}

describe("the pool while suspended", () => {
  it("draws blob-read alone, with the restricted session", async () => {
    const { pool, issuer } = fakeTokenPool({ session: null });
    pool.setSuspended(true);
    pool.setSessionToken(S1);
    await settle();
    expect(issued(issuer)).toEqual(["blob-read"]);
    expect(issuer.find((r) => r.url === "/api/auth/auth/tokens")?.authorization).toBe(`Bearer ${S1}`);
    expect((await pool.take("blob-read")).context).toBe("blob-read");
  });

  it("refuses a take of any other context at once, without an issuance", async () => {
    const { pool, issuer } = fakeTokenPool({ session: null });
    pool.setSuspended(true);
    pool.setSessionToken(S1);
    await settle();
    await expect(pool.take("ds-knock")).rejects.toBeInstanceOf(AnonTokensSuspendedError);
    expect(issued(issuer)).toEqual(["blob-read"]);
  });

  it("reads 403 account-suspended as not now: no loop, a take refused at once", async () => {
    const { pool, issuer, timers } = fakeTokenPool({
      issueScript: [{ status: 403, json: { error: "account-suspended" }, context: "ds-knock" }],
    });
    await settle();
    expect(issued(issuer).filter((c) => c === "ds-knock")).toHaveLength(1);
    await expect(pool.take("ds-knock")).rejects.toBeInstanceOf(AnonTokensSuspendedError);
    // The next scheduled refill inside the backoff asks nothing.
    timers.fireScheduled();
    await settle();
    expect(issued(issuer).filter((c) => c === "ds-knock")).toHaveLength(1);
  });

  it("draws every context again once the suspension ended", async () => {
    const { pool, issuer, timers } = fakeTokenPool({ session: null });
    pool.setSuspended(true);
    pool.setSessionToken(S1);
    await settle();
    pool.setSuspended(false);
    pool.setSessionToken("v3.QUNDT1VOVA.0.tag");
    timers.fireScheduled();
    await settle();
    expect(issued(issuer)).toContain("ds-knock");
  });
});
