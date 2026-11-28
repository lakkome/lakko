// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import type { AccountFactors } from "./factors";
import {
  protectStepCanContinue,
  protectStepDone,
  protectStepRows,
  protectStepShown,
  protectStepTitleKey,
  protectStepTotpWarning,
  LANDING_FACTORS_WAIT_MS,
  landingWaitsForFactors,
} from "./protect-step";

const factors = (over: Partial<AccountFactors> = {}): AccountFactors => ({
  email: false,
  totp: false,
  passkeys: 0,
  mailAvailable: true,
  ...over,
});
const read = (rel: string): string => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

describe("the Protect your account step (UX 19.1a)", () => {
  it("offers the email, the authenticator, then a passkey, in that order", () => {
    expect(protectStepRows(true, true, true)).toEqual(["email", "totp", "passkey"]);
  });

  it("always offers the authenticator, whatever else is missing", () => {
    expect(protectStepRows(false, false, false)).toEqual(["totp"]);
    expect(protectStepRows(null, true, false)).toEqual(["totp"]);
  });

  it("has no email row when mail is not set up, or while that is not known", () => {
    expect(protectStepRows(false, true, true)).toEqual(["totp", "passkey"]);
    expect(protectStepRows(null, true, true)).toEqual(["totp", "passkey"]);
  });

  it("has the passkey row only where this browser can use passkeys", () => {
    expect(protectStepRows(true, true, false)).toEqual(["email", "totp"]);
  });

  it("has no email and no passkey row for an account without a username", () => {
    expect(protectStepRows(true, false, true)).toEqual(["totp"]);
  });

  it("the recovery phrase is never one of the required rows", () => {
    for (const mail of [true, false, null]) {
      for (const name of [true, false]) {
        for (const passkeys of [true, false]) {
          expect(protectStepRows(mail, name, passkeys)).not.toContain("phrase");
        }
      }
    }
  });

  it("marks the rows that are done", () => {
    expect(protectStepDone({ factors: factors(), phraseSaved: false })).toEqual([]);
    expect(protectStepDone({ factors: factors({ email: true }), phraseSaved: false })).toEqual(["email"]);
    expect(protectStepDone({ factors: factors({ totp: true }), phraseSaved: false })).toEqual(["totp"]);
    expect(protectStepDone({ factors: factors({ passkeys: 2 }), phraseSaved: false })).toEqual(["passkey"]);
    expect(protectStepDone({ factors: factors({ email: true, totp: true, passkeys: 1 }), phraseSaved: true })).toEqual([
      "email",
      "totp",
      "passkey",
      "phrase",
    ]);
  });

  it("marks nothing of the three while the factors are not known", () => {
    expect(protectStepDone({ factors: null, phraseSaved: false })).toEqual([]);
    expect(protectStepDone({ factors: null, phraseSaved: true })).toEqual(["phrase"]);
  });

  it("Continue needs one of the first three", () => {
    expect(protectStepCanContinue(factors())).toBe(false);
    expect(protectStepCanContinue(factors({ email: true }))).toBe(true);
    expect(protectStepCanContinue(factors({ totp: true }))).toBe(true);
    expect(protectStepCanContinue(factors({ passkeys: 1 }))).toBe(true);
  });

  it("a saved phrase alone does not enable Continue, and unknown factors keep it disabled", () => {
    // `protectStepCanContinue` takes no phrase at all: nothing about the
    // phrase can enable it.
    expect(protectStepCanContinue.length).toBe(1);
    expect(protectStepCanContinue(null)).toBe(false);
  });

  it("warns when the authenticator is the only one of the three, with the phrase wording when one is saved", () => {
    expect(protectStepTotpWarning(factors({ totp: true }), false)).toBe("protect.totp_only.warning");
    expect(protectStepTotpWarning(factors({ totp: true }), true)).toBe("protect.totp_only.warning_phrase");
  });

  it("the warning goes as soon as an email or a passkey is added, and is not shown without the authenticator", () => {
    expect(protectStepTotpWarning(factors({ totp: true, email: true }), false)).toBeNull();
    expect(protectStepTotpWarning(factors({ totp: true, passkeys: 1 }), false)).toBeNull();
    expect(protectStepTotpWarning(factors({ email: true }), false)).toBeNull();
    expect(protectStepTotpWarning(factors(), false)).toBeNull();
    expect(protectStepTotpWarning(null, false)).toBeNull();
  });
});

describe("the landing rule (UX 19.1c)", () => {
  const base = { hasUsername: true, guest: false, markerOwed: false };

  it("shows the step to an account with a username and none of the three", () => {
    expect(protectStepShown({ ...base, factors: factors() })).toBe(true);
  });

  it("does not show it once any one of the three exists", () => {
    expect(protectStepShown({ ...base, factors: factors({ email: true }) })).toBe(false);
    expect(protectStepShown({ ...base, factors: factors({ totp: true }) })).toBe(false);
    expect(protectStepShown({ ...base, factors: factors({ passkeys: 1 }) })).toBe(false);
  });

  it("never flashes the step while the factors are not known", () => {
    expect(protectStepShown({ ...base, factors: null })).toBe(false);
  });

  it("the sign-up's marker shows it whatever is known, until Continue clears it", () => {
    expect(protectStepShown({ ...base, markerOwed: true, factors: null })).toBe(true);
    expect(protectStepShown({ ...base, markerOwed: true, factors: factors({ email: true }) })).toBe(true);
  });

  it("a guest account, and an account without a username, never see it", () => {
    expect(protectStepShown({ hasUsername: true, guest: true, markerOwed: true, factors: factors() })).toBe(false);
    expect(protectStepShown({ hasUsername: false, guest: false, markerOwed: true, factors: factors() })).toBe(false);
  });

  it("the heading is the sign-up's while its marker is owed, the required one otherwise", () => {
    expect(protectStepTitleKey(true)).toBe("protect.title");
    expect(protectStepTitleKey(false)).toBe("protect.title_required");
  });

  it("the auth context decides the step by this rule and lets nobody past it without a factor", () => {
    const ctx = read("../AuthContext.tsx");
    const owed = ctx.slice(ctx.indexOf("const protectStepOwed = (): boolean => {"));
    expect(owed.slice(0, owed.indexOf("\n  };")).includes("protectStepShown({")).toBe(true);
    const finish = ctx.slice(ctx.indexOf("    finishProtectStep() {"));
    const body = finish.slice(0, finish.indexOf("\n    },"));
    const gate = body.indexOf("if (!protectStepCanContinue(accountFactors(s.account.accountId))) return;");
    expect(gate).toBeGreaterThan(0);
    expect(gate).toBeLessThan(body.indexOf("clearProtectStepOwed("));
  });
});

describe("sign-up shows no recovery phrase (ADR-0075 D2)", () => {
  const ctx = read("../AuthContext.tsx");
  const start = ctx.indexOf("    async signup(username, password, options) {");
  const body = ctx.slice(start, ctx.indexOf("\n    async signupGuest(", start));

  it("stores the phrase on the device exactly as before", () => {
    expect(start).toBeGreaterThan(0);
    expect(body.includes('await storeDevicePhrase(accountIdB64, result.recoveryPhrase, deviceKey, "signup");')).toBe(
      true,
    );
  });

  it("owes the protect step, and neither shows the words nor marks them owed", () => {
    expect(body.includes("markProtectStepOwed(plainStorage(), accountIdB64);")).toBe(true);
    expect(body.includes("setSignupPhrase(")).toBe(false);
    expect(body.includes("markSignupPhraseOwed(")).toBe(false);
    // The step is owed before the session lands, so no guard sees a session
    // without it.
    expect(body.indexOf("markProtectStepOwed(")).toBeLessThan(body.indexOf('kind: "session"'));
  });

  it("the sign-up page renders no phrase and no confirmation of one", () => {
    const page = read("../../routes/Signup.tsx");
    expect(/PhraseGrid|WordSlots|recovery_phrase\./.test(page)).toBe(false);
  });

  it("the step has no way off but Continue, and no browser dialog", () => {
    const prompt = read("./ProtectAccountPrompt.tsx");
    expect(prompt.includes("protect.skip")).toBe(false);
    expect(prompt.includes("requestConfirm(")).toBe(false);
    expect(/window\.(confirm|alert|prompt)\(/.test(prompt)).toBe(false);
    // One call ends the step, and it is behind the rule.
    expect(prompt.match(/auth\.finishProtectStep\(\)/g)?.length).toBe(1);
    expect(prompt.includes("if (!canContinue()) return;")).toBe(true);
    // Disabled is drawn disabled with its line, never hidden.
    expect(prompt.includes("disabled={!canContinue()}")).toBe(true);
    expect(prompt.includes('t("protect.continue_needs")')).toBe(true);
  });
});

describe("no surface logs or keeps the address or a code", () => {
  const files = [
    "./RecoveryEmailForm.tsx",
    "./RemoveRecoveryEmail.tsx",
    "./ProtectionCard.tsx",
    "./AccountNotices.tsx",
    "./recovery-email-client.ts",
    "./email-flow.ts",
    "./factors.ts",
    "./last-factor.ts",
    "./PasswordOnlyNotice.tsx",
    "../recover/EmailRecover.tsx",
  ];

  it("has no console call and no storage write in the email surfaces", () => {
    for (const file of files) {
      const code = read(file);
      expect(/console\.(log|info|warn|error|debug)\(/.test(code), file).toBe(false);
      expect(/\b(localStorage|sessionStorage|indexedDB)\b/.test(code), file).toBe(false);
      expect(/\.setItem\(/.test(code), file).toBe(false);
    }
  });
});

describe("new words are shown only to a phrase-protected account (alignment A and I)", () => {
  const ctx = read("../AuthContext.tsx");

  it("owed words of any other account are dropped, never withheld", () => {
    const start = ctx.indexOf("async function restoreOwedSignupPhrase(");
    const body = ctx.slice(start, ctx.indexOf("\n  }\n", start));
    const rule = body.indexOf("if (!newWordsShown(isPhraseProtected(storage, accountIdB64),");
    expect(rule).toBeGreaterThan(0);
    expect(body.indexOf("clearSignupPhraseOwed(storage, accountIdB64);", rule)).toBeGreaterThan(rule);
    // Decided before anything reads the phrase off the device.
    expect(rule).toBeLessThan(body.indexOf("readDevicePhraseWith("));
  });

  it("a recall takes whether the account had a saved phrase before anything is sent", () => {
    const start = ctx.indexOf("function runRecall(");
    const body = ctx.slice(start, ctx.indexOf("\n  }\n", start));
    // Taken from the anchor check before the request leaves, and kept with
    // the attempt so a lost answer found later reads the same.
    const taken = body.indexOf("phraseArm = check.status.phraseArm;");
    expect(taken).toBeGreaterThan(0);
    expect(taken).toBeLessThan(body.indexOf("await client.recallPrepare("));
    expect(body.indexOf("phraseArm = earlier.phraseArm;")).toBeGreaterThan(0);
    // Written once the recall landed and the staged wipe's seal was lifted
    // (`account-store-gate.ts`): a write made while the account is sealed is
    // dropped. Before the landing's own steps read it.
    const opened = body.indexOf("openStoresFor(accountIdB64);");
    const flag = body.indexOf("setPhraseProtected(plainStorage(), accountIdB64, phraseArm);");
    expect(opened).toBeGreaterThan(body.indexOf("await client.recallFinish(prepared);"));
    expect(flag).toBeGreaterThan(opened);
    expect(flag).toBeLessThan(body.indexOf("await adoptKeyResetOutcome("));
  });

  it("the reset / guest phrase screen lands by itself when no words are shown", () => {
    const prompt = read("../SaveFreshPhrasePrompt.tsx");
    expect(prompt.includes("newWordsShown(phraseProtectedHere(s.account.accountId)")).toBe(true);
    // A guest sign-up has no phrase screen any more.
    expect(prompt.includes("recovery_phrase.guest.lede")).toBe(false);
  });

  it("no rotation saves a phrase any more: the runtime sends no phrase_saved", () => {
    const runtime = read("../../devices/key-rotation-runtime.ts");
    expect(runtime.includes("phraseSaved")).toBe(false);
    expect(runtime.includes("savePhrase")).toBe(false);
    const panel = read("./SavePhrasePanel.tsx");
    expect(panel.includes("useKeyRotation")).toBe(false);
    expect(panel.includes("auth.savePhraseSeal()")).toBe(true);
  });
});

describe("the email vault is gone (ADR-0075, second amendment)", () => {
  it("no surface of the protect area seals, re-seals or opens a vault", () => {
    for (const file of [
      "./RecoveryEmailForm.tsx",
      "./AccountNotices.tsx",
      "./ProtectionCard.tsx",
      "./recovery-email-client.ts",
      "./email-flow.ts",
      "../recover/EmailRecover.tsx",
      "../rename-runtime.tsx",
      "../username-change.ts",
    ]) {
      const code = read(file);
      expect(/emailVault|runEmailVault|putVault|recoverOpen|sealed_entropy|vault_salt|sealedEntropy|vaultSalt/.test(code), file).toBe(
        false,
      );
      expect(/recovery-email\/vault|recover\/open|account\/phrase/.test(code), file).toBe(false);
    }
  });
});

describe("the landing waits for the factor read (UX 19.1c, item G)", () => {
  const none = { email: false, totp: false, passkeys: 0, mailAvailable: true };
  const base = { hasUsername: true, guest: false, markerOwed: false, factors: null, waitOver: false };

  it("waits while the factors are not known and the wait is not over", () => {
    expect(landingWaitsForFactors(base)).toBe(true);
    expect(landingWaitsForFactors({ ...base, waitOver: true })).toBe(false);
    expect(landingWaitsForFactors({ ...base, factors: none })).toBe(false);
    expect(LANDING_FACTORS_WAIT_MS).toBe(3_000);
  });

  it("never for an account the step cannot apply to, or one whose sign-up marker already shows it", () => {
    expect(landingWaitsForFactors({ ...base, hasUsername: false })).toBe(false);
    expect(landingWaitsForFactors({ ...base, guest: true })).toBe(false);
    expect(landingWaitsForFactors({ ...base, markerOwed: true })).toBe(false);
  });
});
