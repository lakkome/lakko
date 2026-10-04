// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The password-fresh reset grant (`reset-grant.ts`; ADR-0065 amended
 * 2026-10-04, identity-recovery UX section 15.1a): the wire read, the
 * in-memory holder, the warning screen's decision and the failure mapping.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import {
  createResetGrantHolder,
  isPasswordRequired,
  resetFormState,
  resetGrantOf,
  resetLoginFailure,
  type ResetFormInput,
} from "./reset-grant";

describe("resetGrantOf", () => {
  it("reads the two fields of a password login's finish", () => {
    expect(resetGrantOf({ reset_grant: "g1.opaque", reset_grant_expires_at_ms: 1_000 })).toEqual({
      grant: "g1.opaque",
      expiresAtMs: 1_000,
    });
  });

  it("answers null, never throws, for a response without a usable grant", () => {
    for (const wire of [
      null,
      undefined,
      {},
      { reset_grant: "g" },
      { reset_grant_expires_at_ms: 5 },
      { reset_grant: "", reset_grant_expires_at_ms: 5 },
      { reset_grant: 7, reset_grant_expires_at_ms: 5 },
      { reset_grant: "g", reset_grant_expires_at_ms: "5" },
      { reset_grant: "g", reset_grant_expires_at_ms: Number.NaN },
      { reset_grant: "g", reset_grant_expires_at_ms: 0 },
    ]) {
      expect(resetGrantOf(wire)).toBeNull();
    }
  });
});

describe("createResetGrantHolder", () => {
  const grant = { grant: "g-a", expiresAtMs: 10_000 };

  it("holds nothing at first", () => {
    expect(createResetGrantHolder().live("acct-a", 0)).toBeNull();
  });

  it("hands the grant out until its expiry, and not at or after it", () => {
    const holder = createResetGrantHolder();
    holder.put("acct-a", grant);
    expect(holder.live("acct-a", 0)).toBe("g-a");
    expect(holder.live("acct-a", 9_999)).toBe("g-a");
    expect(holder.live("acct-a", 10_000)).toBeNull();
  });

  it("drops an expired grant for good, even if the clock steps back", () => {
    const holder = createResetGrantHolder();
    holder.put("acct-a", grant);
    expect(holder.live("acct-a", 10_001)).toBeNull();
    expect(holder.live("acct-a", 1)).toBeNull();
  });

  it("is bound to the account it was issued for", () => {
    const holder = createResetGrantHolder();
    holder.put("acct-a", grant);
    expect(holder.live("acct-b", 0)).toBeNull();
    // A read for another account drops it: it is never kept around for a
    // session it does not belong to.
    expect(holder.live("acct-a", 0)).toBeNull();
  });

  it("is single use: spending it leaves nothing", () => {
    const holder = createResetGrantHolder();
    holder.put("acct-a", grant);
    holder.spend("g-a");
    expect(holder.live("acct-a", 0)).toBeNull();
  });

  it("spending an older grant keeps a newer one", () => {
    const holder = createResetGrantHolder();
    holder.put("acct-a", grant);
    holder.put("acct-a", { grant: "g-new", expiresAtMs: 20_000 });
    holder.spend("g-a");
    expect(holder.live("acct-a", 0)).toBe("g-new");
  });

  it("clear drops it (sign-out, account wipe)", () => {
    const holder = createResetGrantHolder();
    holder.put("acct-a", grant);
    holder.clear();
    expect(holder.live("acct-a", 0)).toBeNull();
  });
});

describe("resetFormState", () => {
  const base: ResetFormInput = {
    grantLive: true,
    passwordRevealed: false,
    usernameKnown: true,
    password: "",
    username: "",
    confirmed: false,
    blocked: false,
  };

  it("with a live grant is exactly section 15.1: no field, the word alone enables the button", () => {
    expect(resetFormState(base)).toEqual({
      showPassword: false,
      showUsername: false,
      canSubmit: false,
    });
    expect(resetFormState({ ...base, confirmed: true })).toEqual({
      showPassword: false,
      showUsername: false,
      canSubmit: true,
    });
  });

  it("without a grant shows the password field and needs both the password and the word", () => {
    const noGrant = { ...base, grantLive: false };
    expect(resetFormState(noGrant).showPassword).toBe(true);
    expect(resetFormState(noGrant).showUsername).toBe(false);
    expect(resetFormState({ ...noGrant, confirmed: true }).canSubmit).toBe(false);
    expect(resetFormState({ ...noGrant, password: "pw" }).canSubmit).toBe(false);
    expect(resetFormState({ ...noGrant, password: "pw", confirmed: true }).canSubmit).toBe(true);
  });

  it("keeps the field once revealed, and then still needs the password", () => {
    const revealed = { ...base, passwordRevealed: true, confirmed: true };
    expect(resetFormState(revealed).showPassword).toBe(true);
    expect(resetFormState(revealed).canSubmit).toBe(false);
    expect(resetFormState({ ...revealed, password: "pw" }).canSubmit).toBe(true);
  });

  it("asks for the username too when the device does not know it", () => {
    const noName = {
      ...base,
      grantLive: false,
      usernameKnown: false,
      password: "pw",
      confirmed: true,
    };
    expect(resetFormState(noName).showUsername).toBe(true);
    expect(resetFormState(noName).canSubmit).toBe(false);
    expect(resetFormState({ ...noName, username: "   " }).canSubmit).toBe(false);
    expect(resetFormState({ ...noName, username: "alice" }).canSubmit).toBe(true);
    // No username field without the password field.
    expect(resetFormState({ ...base, usernameKnown: false }).showUsername).toBe(false);
  });

  it("is disabled while busy or waiting out a limit, whatever is typed", () => {
    expect(resetFormState({ ...base, confirmed: true, blocked: true }).canSubmit).toBe(false);
    expect(
      resetFormState({ ...base, grantLive: false, password: "pw", confirmed: true, blocked: true })
        .canSubmit,
    ).toBe(false);
  });
});

describe("isPasswordRequired", () => {
  it("is the 403 with the password-required code and nothing else", () => {
    expect(isPasswordRequired({ status: 403, errorCode: "password-required" })).toBe(true);
    expect(isPasswordRequired({ status: 403, errorCode: null })).toBe(false);
    expect(isPasswordRequired({ status: 403, errorCode: "forbidden" })).toBe(false);
    expect(isPasswordRequired({ status: 401, errorCode: "password-required" })).toBe(false);
    expect(isPasswordRequired({ status: null, errorCode: "password-required" })).toBe(false);
  });
});

describe("resetLoginFailure", () => {
  it("reads a 401, a 404 and a local exchange failure as a wrong password", () => {
    expect(resetLoginFailure({ status: 401, network: false })).toBe("wrong-password");
    expect(resetLoginFailure({ status: 404, network: false })).toBe("wrong-password");
    expect(resetLoginFailure({ status: null, network: false })).toBe("wrong-password");
  });

  it("never reads a limit, an outage or the network as a wrong password", () => {
    expect(resetLoginFailure({ status: 429, network: false })).toBe("other");
    expect(resetLoginFailure({ status: 503, network: false })).toBe("other");
    expect(resetLoginFailure({ status: 500, network: false })).toBe("other");
    expect(resetLoginFailure({ status: null, network: true })).toBe("other");
  });
});

describe("the grant stays in memory", () => {
  const read = (rel: string): string =>
    readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

  it("the holder module touches no storage and no log", () => {
    const src = read("./reset-grant.ts");
    for (const needle of ["localStorage", "sessionStorage", "indexedDB", "console.", "plainStorage"]) {
      expect(src.includes(needle)).toBe(false);
    }
  });

  it("the reset sends the grant on both calls", () => {
    const client = read("./client.ts");
    const start = client.indexOf("async resetAccountKeys(");
    const body = client.slice(start, client.indexOf("\n  }\n", start));
    expect(body.match(/reset_grant: resetGrant/g)?.length).toBe(2);
  });

  it("the owed-publish retry asks for no grant", () => {
    const ctx = read("./AuthContext.tsx");
    const start = ctx.indexOf("async function retryKeyResetPublish(");
    expect(start).toBeGreaterThan(0);
    const end = ctx.indexOf("\n  }\n", start);
    expect(ctx.slice(start, end).includes("resetGrants")).toBe(false);
  });
});
