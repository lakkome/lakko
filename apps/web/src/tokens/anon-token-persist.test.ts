// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0054 S5 — the anonymous-token pool survives a relaunch: tokens are
 * written through to the store, loaded when a session is installed (dead
 * epochs dropped on load), kept per account (Log out and an account switch
 * leave every account's rows; forgetting an account removes only its own).
 * Batch sizes follow how often each action happens.
 */
import { describe, expect, it } from "vitest";

import {
  AnonTokenUnavailableError,
  LOW_WATER,
  REFILL_BATCH,
  type AnonTokenContext,
} from "./anon-token-pool";
import { memoryAnonTokenStore } from "./anon-token-store";
import { fakeTokenPool } from "./anon-token-test-doubles";

const ACCOUNT = "QUNDT1VOVA";
/** 1_700_000_000 s / 86_400 s: the fake wasm's current epoch. */
const EPOCH = 19675;

function issuances(issuer: ReturnType<typeof fakeTokenPool>["issuer"]) {
  return issuer.filter((r) => r.url === "/api/auth/auth/tokens");
}

describe("the pool survives a relaunch", () => {
  it("a new pool over the same store hands out the stored tokens without drawing", async () => {
    const store = memoryAnonTokenStore();
    const first = fakeTokenPool({ store });
    expect((await first.pool.take("discovery-lookup")).headerValue).toBe("tok-1");
    await first.pool.whenPersisted();
    expect(store.rows.get(`${ACCOUNT}\u0000discovery-lookup`)).toHaveLength(REFILL_BATCH["discovery-lookup"] - 1);

    // Relaunch: a fresh pool, the same store, the same account.
    const second = fakeTokenPool({ store });
    const token = await second.pool.take("discovery-lookup");
    expect(token.headerValue).toBe("tok-2");
    expect(issuances(second.issuer)).toHaveLength(0);
  });

  it("a token taken is gone from the store, so a relaunch never offers it again", async () => {
    const store = memoryAnonTokenStore();
    const first = fakeTokenPool({ store });
    await first.pool.take("push-register"); // tok-1
    await first.pool.take("push-register"); // tok-2
    await first.pool.whenPersisted();
    const second = fakeTokenPool({ store });
    expect((await second.pool.take("push-register")).headerValue).toBe("tok-3");
  });

  it("dead-epoch tokens are dropped on load, and from the store", async () => {
    const store = memoryAnonTokenStore();
    await store.save(ACCOUNT, "invite-consume", [
      { epoch: String(EPOCH - 5), headerValue: "dead" },
      { epoch: String(EPOCH - 1), headerValue: "previous-epoch" },
      { epoch: String(EPOCH), headerValue: "current-epoch" },
    ]);
    const { pool, issuer } = fakeTokenPool({ store });
    await pool.ready();
    expect(pool.size("invite-consume")).toBe(2);
    await pool.whenPersisted();
    expect(store.rows.get(`${ACCOUNT}\u0000invite-consume`)?.map((t) => t.headerValue)).toEqual([
      "previous-epoch",
      "current-epoch",
    ]);
    expect((await pool.take("invite-consume")).headerValue).toBe("previous-epoch");
    expect(issuances(issuer)).toHaveLength(0);
  });

  it("a store holding only dead tokens refills as an empty pool does", async () => {
    const store = memoryAnonTokenStore();
    await store.save(ACCOUNT, "ds-knock", [{ epoch: String(EPOCH - 3), headerValue: "dead" }]);
    const { pool, issuer } = fakeTokenPool({ store });
    expect((await pool.take("ds-knock")).headerValue).toBe("tok-1");
    expect(issuances(issuer)).toHaveLength(1);
  });

  it("another account's stored tokens are not loaded", async () => {
    const store = memoryAnonTokenStore();
    await store.save("QU5PVEhFUg", "discovery-lookup", [{ epoch: String(EPOCH), headerValue: "theirs" }]);
    const { pool } = fakeTokenPool({ store });
    await pool.ready();
    expect(pool.size("discovery-lookup")).toBe(0);
  });
});

describe("the stored tokens belong to the account", () => {
  const OTHER = "QU5PVEhFUg";
  const OTHER_SESSION = `v1.${OTHER}.0.tag`;
  const rowsOf = (store: ReturnType<typeof memoryAnonTokenStore>, account: string): string[] =>
    [...store.rows.keys()].filter((k) => k.startsWith(`${account}\u0000`));

  it("two accounts' tokens coexist in the store", async () => {
    const store = memoryAnonTokenStore();
    const { pool } = fakeTokenPool({ store });
    await pool.take("discovery-lookup");
    pool.setSessionToken(OTHER_SESSION);
    await pool.take("push-register");
    await pool.whenPersisted();
    expect(rowsOf(store, ACCOUNT)).toEqual([`${ACCOUNT}\u0000discovery-lookup`]);
    expect(rowsOf(store, OTHER)).toEqual([`${OTHER}\u0000push-register`]);
  });

  it("logout keeps the account's tokens, and its next sign-in loads them", async () => {
    const store = memoryAnonTokenStore();
    const { pool, issuer } = fakeTokenPool({ store });
    expect((await pool.take("discovery-lookup")).headerValue).toBe("tok-1");
    pool.setSessionToken(null);
    pool.unbindIfNoSession();
    expect(pool.size("discovery-lookup")).toBe(0);
    await pool.whenPersisted();
    expect(store.rows.get(`${ACCOUNT}\u0000discovery-lookup`)).toHaveLength(
      REFILL_BATCH["discovery-lookup"] - 1,
    );
    // The same account signs in again, on this pool and after a relaunch.
    pool.setSessionToken(`v1.${ACCOUNT}.1.tag`);
    expect((await pool.take("discovery-lookup")).headerValue).toBe("tok-2");
    await pool.whenPersisted();
    const relaunch = fakeTokenPool({ store });
    expect((await relaunch.pool.take("discovery-lookup")).headerValue).toBe("tok-3");
    expect(issuances(issuer)).toHaveLength(1);
    expect(issuances(relaunch.issuer)).toHaveLength(0);
  });

  it("signing in as another account never yields the first account's tokens", async () => {
    const store = memoryAnonTokenStore();
    const { pool, issuer } = fakeTokenPool({ store });
    await pool.take("discovery-lookup"); // tok-1, tok-2.. tok-8 held for ACCOUNT
    await pool.whenPersisted();
    pool.setSessionToken(OTHER_SESSION);
    await pool.ready();
    expect(pool.size("discovery-lookup")).toBe(0);
    const theirs = await pool.take("discovery-lookup");
    // Drawn fresh for OTHER (the fake issuer numbers tokens per batch).
    expect(issuances(issuer)).toHaveLength(2);
    const ours = store.rows.get(`${ACCOUNT}\u0000discovery-lookup`)?.map((t) => t.headerValue) ?? [];
    expect(ours).not.toContain(theirs.headerValue);
    // ACCOUNT's rows are untouched by the switch.
    expect(ours).toHaveLength(REFILL_BATCH["discovery-lookup"] - 1);
  });

  it("a write queued before an account switch lands under its own account", async () => {
    const store = memoryAnonTokenStore();
    const { pool } = fakeTokenPool({ store });
    await pool.take("invite-consume"); // its write is still queued
    pool.setSessionToken(OTHER_SESSION);
    await pool.whenPersisted();
    expect(store.rows.get(`${ACCOUNT}\u0000invite-consume`)).toHaveLength(
      REFILL_BATCH["invite-consume"] - 1,
    );
    expect(rowsOf(store, OTHER)).toEqual([]);
  });

  it("a take that started before an account switch fails instead of taking the new account's token", async () => {
    const store = memoryAnonTokenStore();
    await store.save(OTHER, "ds-knock", [{ epoch: String(EPOCH), headerValue: "theirs" }]);
    const { pool, issuer } = fakeTokenPool({ store });
    await pool.ready();
    const pending = pool.take("ds-knock").catch((e: unknown) => e);
    pool.setSessionToken(OTHER_SESSION);
    const outcome = await pending;
    expect(outcome).toBeInstanceOf(AnonTokenUnavailableError);
    await pool.ready();
    expect(pool.size("ds-knock")).toBe(1);
    expect(issuances(issuer)).toHaveLength(0);
  });

  it("forgetAccount removes only that account's rows, after the writes queued before it", async () => {
    const store = memoryAnonTokenStore();
    await store.save(OTHER, "ds-knock", [{ epoch: String(EPOCH), headerValue: "theirs" }]);
    const { pool } = fakeTokenPool({ store });
    await pool.take("invite-consume"); // the take's write is still queued
    // "QUNDT1VOVA" is base64url of the bytes "ACCOUNT"; the wipe names the
    // account in standard base64.
    pool.forgetAccount("QUNDT1VOVA==");
    await pool.whenPersisted();
    expect(rowsOf(store, ACCOUNT)).toEqual([]);
    expect(store.rows.get(`${OTHER}\u0000ds-knock`)?.map((t) => t.headerValue)).toEqual(["theirs"]);
    expect(pool.size("invite-consume")).toBe(0);
  });
});

describe("batch sizes follow how often the action happens", () => {
  const expected: Array<[AnonTokenContext, number, number]> = [
    ["discovery-lookup", 8, 2],
    ["push-register", 4, 1],
    ["invite-consume", 2, 1],
    ["ds-knock", 2, 1],
    ["discovery-register", 1, 1],
  ];

  it.each(expected)("%s: batch %i, refill below %i", async (context, batch, low) => {
    expect(REFILL_BATCH[context]).toBe(batch);
    expect(LOW_WATER[context]).toBe(low);
    const { pool, issuer } = fakeTokenPool();
    await pool.take(context);
    const body = issuances(issuer)[0]?.body as { blinded_elements: string[] };
    expect(body.blinded_elements).toHaveLength(batch);
  });

  it("a rare context is not refilled until it is empty", async () => {
    const { pool, issuer } = fakeTokenPool();
    await pool.take("invite-consume"); // batch 2, one left
    for (let i = 0; i < 5; i += 1) await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(issuances(issuer)).toHaveLength(1);
    expect(pool.size("invite-consume")).toBe(1);
  });
});
