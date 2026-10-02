// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import {
  clearRecoveryCredentialOwed,
  isRecoveryCredentialOwed,
  markRecoveryCredentialOwed,
} from "./recovery-credential-owed";

function memoryStorage(): { getItem(k: string): string | null; setItem(k: string, v: string): void; removeItem(k: string): void } {
  const rows = new Map<string, string>();
  return {
    getItem: (k) => rows.get(k) ?? null,
    setItem: (k, v) => void rows.set(k, v),
    removeItem: (k) => void rows.delete(k),
  };
}

describe("the owed recovery-credential marker", () => {
  it("is per account and cleared only by clearing it", () => {
    const storage = memoryStorage();
    expect(isRecoveryCredentialOwed(storage, "a")).toBe(false);
    markRecoveryCredentialOwed(storage, "a");
    expect(isRecoveryCredentialOwed(storage, "a")).toBe(true);
    expect(isRecoveryCredentialOwed(storage, "b")).toBe(false);
    clearRecoveryCredentialOwed(storage, "a");
    expect(isRecoveryCredentialOwed(storage, "a")).toBe(false);
  });

  it("is absent without storage", () => {
    expect(isRecoveryCredentialOwed(undefined, "a")).toBe(false);
    expect(() => markRecoveryCredentialOwed(undefined, "a")).not.toThrow();
  });
});

describe("a fresh identity and the recovery credential (AuthContext wiring)", () => {
  const src = readFileSync(fileURLToPath(new URL("./AuthContext.tsx", import.meta.url)), "utf8");
  const start = src.indexOf("async mintFreshIdentity(");
  const end = src.indexOf("async confirmFreshPhraseSaved(");
  const mint = src.slice(start, end);

  it("replaces the credential after the publish, with the debt recorded first", () => {
    const publish = mint.indexOf("await uploadIdentityBundle(");
    const mark = mint.indexOf("markRecoveryCredentialOwed(");
    const replace = mint.indexOf("await client.replaceRecoveryCredential(");
    const clear = mint.indexOf("clearRecoveryCredentialOwed(");
    const stash = mint.indexOf("setPendingFreshPhrase({");
    expect(publish).toBeGreaterThan(0);
    expect(mark).toBeGreaterThan(publish);
    expect(replace).toBeGreaterThan(mark);
    expect(clear).toBeGreaterThan(replace);
    // The phrase is parked for the screen only after the attempt, carrying
    // whether the credential is still owed.
    expect(stash).toBeGreaterThan(clear);
    expect(mint).toMatch(/phrase,\s*recoveryCredentialOwed,/);
  });

  it("mints once: the retry repeats only the credential step", () => {
    const retryStart = src.indexOf("async retryFreshRecoveryCredential()");
    const retry = src.slice(retryStart, src.indexOf("},", src.indexOf("setPendingFreshPhrase({ ...pending", retryStart)));
    expect(retry).toContain("client.replaceRecoveryCredential(");
    expect(retry).not.toContain("RecoveryMnemonic.generate");
    expect(retry).not.toContain("uploadIdentityBundle");
    expect(src.match(/RecoveryMnemonic\.generate\(\)/g)?.length).toBe(1);
  });

  it("does not land the session while the credential is owed", () => {
    const confirm = src.slice(end, src.indexOf("async recover(", end));
    const refuse = confirm.indexOf("if (pending.recoveryCredentialOwed)");
    const land = confirm.indexOf("await finishCanonicalSession(");
    expect(refuse).toBeGreaterThan(0);
    expect(land).toBeGreaterThan(refuse);
  });

  it("retries an owed replace when a session lands, and holds the phrase back until then", () => {
    expect(src).toMatch(/settleOwedRecoveryCredential\(args\.accountIdB64, args\.accountIdentitySeed, args\.sessionToken\);/);
    const owed = src.slice(src.indexOf("async function restoreOwedSignupPhrase("));
    expect(owed.indexOf("if (isRecoveryCredentialOwed(storage, accountIdB64)) return;")).toBeGreaterThan(0);
  });
});
