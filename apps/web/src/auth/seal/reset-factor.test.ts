// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { AccountFactors } from "../protect/factors";
import {
  createResetGrantHolder,
  emailCodeExpired,
  emailCodeRefusal,
  resetFactorFailure,
  resetFactorRows,
  resetKeepsKey,
  resetSection,
  resetSubmitEnabled,
  resetWarning,
} from "./reset-factor";

const factors = (over: Partial<AccountFactors> = {}): AccountFactors => ({
  email: false,
  totp: false,
  passkeys: 0,
  mailAvailable: true,
  ...over,
});

describe("the reset's factor rows", () => {
  it("are the factors the account has, in the fixed order", () => {
    expect(resetFactorRows(factors({ email: true, totp: true, passkeys: 2 }), true)).toEqual([
      "totp",
      "email",
      "passkey",
    ]);
    expect(resetFactorRows(factors({ email: true }), true)).toEqual(["email"]);
    expect(resetFactorRows(factors(), true)).toEqual([]);
  });

  it("hide the passkey where this browser cannot use passkeys", () => {
    expect(resetFactorRows(factors({ totp: true, passkeys: 1 }), false)).toEqual(["totp"]);
  });
});

describe("the reset section", () => {
  const base = { passkeysSupported: true, passwordProofLive: false, confirmed: null, cannotUse: false } as const;

  it("draws nothing while the factors are unknown", () => {
    expect(resetSection({ ...base, factors: null })).toEqual({ kind: "loading" });
  });

  it("shows the password block only without a live password proof", () => {
    const f = factors({ totp: true });
    expect(resetSection({ ...base, factors: f })).toEqual({ kind: "choose", showPassword: true, rows: ["totp"] });
    expect(resetSection({ ...base, factors: f, passwordProofLive: true })).toEqual({
      kind: "choose",
      showPassword: false,
      rows: ["totp"],
    });
  });

  it("an account with none of the three cannot reset, with no way back to the rows", () => {
    expect(resetSection({ ...base, factors: factors() })).toEqual({ kind: "no-factor", accountHasNone: true });
  });

  it("'I cannot use any of these' shows the panel with Back; so does a passkey-only account here without passkeys", () => {
    expect(resetSection({ ...base, factors: factors({ email: true }), cannotUse: true })).toEqual({
      kind: "no-factor",
      accountHasNone: false,
    });
    expect(resetSection({ ...base, factors: factors({ passkeys: 1 }), passkeysSupported: false })).toEqual({
      kind: "no-factor",
      accountHasNone: false,
    });
  });

  it("collapses to the confirmed line, whatever else is true", () => {
    expect(resetSection({ ...base, factors: null, confirmed: "email", cannotUse: true })).toEqual({
      kind: "confirmed",
      factor: "email",
    });
  });

  it("enables the button only with a confirmed factor and the typed word", () => {
    const confirmed = resetSection({ ...base, factors: factors({ totp: true }), confirmed: "totp" });
    const choose = resetSection({ ...base, factors: factors({ totp: true }), passwordProofLive: true });
    expect(resetSubmitEnabled({ section: confirmed, wordTyped: true, busy: false })).toBe(true);
    expect(resetSubmitEnabled({ section: confirmed, wordTyped: false, busy: false })).toBe(false);
    expect(resetSubmitEnabled({ section: confirmed, wordTyped: true, busy: true })).toBe(false);
    // The password alone is not enough any more.
    expect(resetSubmitEnabled({ section: choose, wordTyped: true, busy: false })).toBe(false);
  });
});

describe("the reset factor grant", () => {
  it("is held for its account until it expires or is spent", () => {
    const h = createResetGrantHolder();
    h.put("a", { grant: "f1.x", factor: "totp", expiresAtMs: 1000 });
    expect(h.live("a", 999)?.factor).toBe("totp");
    expect(h.live("b", 0)).toBeNull();
    expect(h.live("a", 0)).toBeNull(); // another account's read dropped it
    h.put("a", { grant: "f1.x", factor: "totp", expiresAtMs: 1000 });
    expect(h.live("a", 1000)).toBeNull();
    h.put("a", { grant: "f1.y", factor: "email", expiresAtMs: 1000 });
    h.spend("f1.other");
    expect(h.live("a", 1)?.grant).toBe("f1.y");
    h.spend("f1.y");
    expect(h.live("a", 1)).toBeNull();
    h.put("a", { grant: "f1.z", factor: "passkey", expiresAtMs: 1000 });
    h.clear();
    expect(h.live("a", 1)).toBeNull();
  });
});

describe("factor failures", () => {
  const answer = (status: number | null, errorCode: string | null = null, retryAfterSecs: number | null = null) => ({
    status,
    errorCode,
    retryAfterSecs,
    network: status === null,
  });

  it("maps each server answer to what the section shows", () => {
    expect(resetFactorFailure(answer(401, "factor-refused"))).toEqual({ kind: "refused" });
    expect(resetFactorFailure(answer(403, "fresh-auth-required"))).toEqual({ kind: "password-needed" });
    expect(resetFactorFailure(answer(429, "too-many-attempts", 120))).toEqual({
      kind: "rate-limited",
      retryAfterSecs: 120,
    });
    expect(resetFactorFailure(answer(429)).kind).toBe("rate-limited");
    expect(resetFactorFailure(answer(400, "address-invalid"))).toEqual({ kind: "invalid-address" });
    expect(resetFactorFailure(answer(503))).toEqual({ kind: "send-failed" });
    expect(resetFactorFailure(answer(null))).toEqual({ kind: "offline" });
    expect(resetFactorFailure(answer(500))).toEqual({ kind: "generic" });
    // A bare 401 (no session) is not a wrong code.
    expect(resetFactorFailure(answer(401))).toEqual({ kind: "generic" });
  });

  it("a mailed code is used up after five tries and expires with its lifetime", () => {
    expect(emailCodeRefusal(1)).toBe("wrong");
    expect(emailCodeRefusal(4)).toBe("wrong");
    expect(emailCodeRefusal(5)).toBe("used-up");
    expect(emailCodeExpired(0, 600, 599_999)).toBe(false);
    expect(emailCodeExpired(0, 600, 600_000)).toBe(true);
    expect(emailCodeExpired(0, 0, 10_000_000)).toBe(false);
  });
});

describe("the warning above the reset panel (UX 15.1, amended 2026-11-28)", () => {
  const choose = { kind: "choose", showPassword: true, rows: ["totp"] } as const;

  it("is not drawn above the cannot-reset panel of an account with no factor", () => {
    const none = factors();
    const section = resetSection({
      factors: none,
      passkeysSupported: true,
      passwordProofLive: false,
      confirmed: null,
      cannotUse: false,
    });
    expect(section).toEqual({ kind: "no-factor", accountHasNone: true });
    expect(resetWarning({ section, factors: none, passwordUnknown: false })).toBeNull();
  });

  it("is not drawn above `I cannot use any of these`", () => {
    const f = factors({ totp: true });
    const section = resetSection({
      factors: f,
      passkeysSupported: true,
      passwordProofLive: false,
      confirmed: null,
      cannotUse: true,
    });
    expect(resetWarning({ section, factors: f, passwordUnknown: false })).toBeNull();
  });

  it("is not drawn while the password is unknown (17.2), nor while the factors load", () => {
    expect(resetWarning({ section: choose, factors: factors({ totp: true }), passwordUnknown: true })).toBeNull();
    expect(resetWarning({ section: { kind: "loading" }, factors: null, passwordUnknown: false })).toBeNull();
    expect(
      resetWarning({ section: { kind: "confirmed", factor: "totp" }, factors: null, passwordUnknown: false }),
    ).toBeNull();
  });

  it("is drawn above the rows and above a confirmed factor, without a claim about the chats", () => {
    const f = factors({ totp: true });
    for (const section of [choose, { kind: "confirmed", factor: "totp" } as const]) {
      expect(resetWarning({ section, factors: f, passwordUnknown: false })?.bulletKeys).toEqual([
        "restore_identity.reset.effect_devices",
        "restore_identity.reset.effect_passkeys",
        "restore_identity.reset.effect_safety_number",
        "restore_identity.reset.keeps.password_totp",
      ]);
    }
  });

  it("names only what the account has as still working, never the passkeys", () => {
    expect(resetKeepsKey(factors({ passkeys: 1 }))).toBe("restore_identity.reset.keeps.password");
    expect(resetKeepsKey(factors({ totp: true, passkeys: 3 }))).toBe("restore_identity.reset.keeps.password_totp");
    expect(resetKeepsKey(factors({ email: true }))).toBe("restore_identity.reset.keeps.password_email");
    expect(resetKeepsKey(factors({ email: true, totp: true }))).toBe(
      "restore_identity.reset.keeps.password_totp_email",
    );
  });
});
