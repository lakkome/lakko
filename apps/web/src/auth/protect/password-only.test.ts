// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { t } from "../../l10n";
import type { OwnSealFacts } from "../seal/own-seals";
import type { AccountFactors } from "./factors";
import {
  passwordOnlyNoticeApplies,
  passwordOnlyOffersPasskey,
  passwordOnlyShownIn,
} from "./password-only";

const factors = (over: Partial<AccountFactors> = {}): AccountFactors => ({
  email: true,
  totp: false,
  passkeys: 0,
  mailAvailable: true,
  ...over,
});
const seals = (over: Partial<OwnSealFacts> = {}): OwnSealFacts => ({
  password: true,
  phrase: false,
  passkeyTagsHex: [],
  seq: "1",
  ...over,
});
const read = (rel: string): string => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

describe("when the chats hang on the password alone (UX 19.1d)", () => {
  it("applies to an account with a second way in, no saved phrase and no passkey that keeps the chats", () => {
    expect(passwordOnlyNoticeApplies(factors(), seals())).toBe(true);
    expect(passwordOnlyNoticeApplies(factors({ email: false, totp: true }), seals())).toBe(true);
  });

  it("a passkey that signs in but keeps nothing does not end it", () => {
    expect(passwordOnlyNoticeApplies(factors({ passkeys: 1 }), seals())).toBe(true);
  });

  it("goes once a phrase is saved", () => {
    expect(passwordOnlyNoticeApplies(factors(), seals({ phrase: true }))).toBe(false);
  });

  it("goes once a passkey keeps the chats, and comes back when that passkey is deleted", () => {
    expect(passwordOnlyNoticeApplies(factors({ passkeys: 1 }), seals({ passkeyTagsHex: ["ab".repeat(16)] }))).toBe(
      false,
    );
    expect(passwordOnlyNoticeApplies(factors({ passkeys: 0 }), seals({ passkeyTagsHex: [] }))).toBe(true);
  });

  it("is not shown before the account has a second way in", () => {
    expect(passwordOnlyNoticeApplies(factors({ email: false }), seals())).toBe(false);
  });

  it("unknown facts show nothing, in either direction", () => {
    expect(passwordOnlyNoticeApplies(null, seals())).toBe(false);
    expect(passwordOnlyNoticeApplies(factors(), null)).toBe(false);
    expect(passwordOnlyNoticeApplies(null, null)).toBe(false);
  });
});

describe("its three places", () => {
  it("the step and Settings > Security > Recovery show it for as long as it applies", () => {
    expect(passwordOnlyShownIn("step", { applies: true })).toBe(true);
    expect(passwordOnlyShownIn("settings-recovery", { applies: true })).toBe(true);
    // A seen dialog does not hide the two standing places.
    expect(passwordOnlyShownIn("step", { applies: true, dialogSeen: true })).toBe(true);
    expect(passwordOnlyShownIn("settings-recovery", { applies: true, dialogSeen: true })).toBe(true);
  });

  it("nowhere when it does not apply", () => {
    for (const place of ["step", "settings-recovery", "dialog"] as const) {
      expect(passwordOnlyShownIn(place, { applies: false })).toBe(false);
    }
  });

  it("the dialog opens once on a device", () => {
    expect(passwordOnlyShownIn("dialog", { applies: true, dialogSeen: false, guardStepUp: false })).toBe(true);
    expect(passwordOnlyShownIn("dialog", { applies: true, dialogSeen: true, guardStepUp: false })).toBe(false);
  });

  it("the dialog waits while a step that comes before every route is up", () => {
    expect(passwordOnlyShownIn("dialog", { applies: true, dialogSeen: false, guardStepUp: true })).toBe(false);
  });

  it("Add a passkey is offered only where passkeys can be created, with a username", () => {
    expect(passwordOnlyOffersPasskey(true, true)).toBe(true);
    expect(passwordOnlyOffersPasskey(false, true)).toBe(false);
    expect(passwordOnlyOffersPasskey(true, false)).toBe(false);
  });

  it("is drawn by exactly the three surfaces and nowhere else", () => {
    // The one component, and the three files that mount it.
    const users = [
      "./ProtectAccountPrompt.tsx",
      "./AccountNotices.tsx",
      "../../settings/RecoveryPage.tsx",
    ];
    for (const file of users) expect(read(file).includes("<PasswordOnlyNotice"), file).toBe(true);
    for (const file of [
      "./ProtectionCard.tsx",
      "../PasskeysSection.tsx",
      "../../routes/Chat.tsx",
      "../../ui/AppShell.tsx",
    ]) {
      expect(read(file).includes("PasswordOnlyNotice"), file).toBe(false);
      expect(read(file).includes("protect.password_only.title"), file).toBe(false);
    }
  });

  it("the dialog closes for good on this device by either button, and does not open after a key reset here", () => {
    const notices = read("./AccountNotices.tsx");
    expect(notices.includes("markPasswordOnlyDialogSeen(plainStore(), l.accountIdB64);")).toBe(true);
    expect(notices.includes('t("common.not_now")')).toBe(true);
    expect(notices.includes("if (l !== null && auth.keyResetNote()) markPasswordOnlyDialogSeen(")).toBe(true);
    // The sign-up step already showed the notice: its Continue marks it seen.
    const step = read("./ProtectAccountPrompt.tsx");
    expect(step.indexOf("markPasswordOnlyDialogSeen(plainStore(), id);")).toBeLessThan(
      step.indexOf("auth.finishProtectStep();"),
    );
  });

  it("the copy is in the catalog", () => {
    expect(t("protect.password_only.title")).toBe("Your chats depend on your password");
    expect(t("protect.password_only.body")).toContain("Save a recovery phrase or add a passkey");
    expect(t("protect.password_only.passkey_no_seal")).toBe(
      "This passkey signs you in but does not keep your chats.",
    );
  });
});
