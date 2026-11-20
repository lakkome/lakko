// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { MemoryStorage } from "../../test/fake-storage";
import {
  accountProtection,
  bannerDismissedAt,
  clearProtectStepOwed,
  clearProtectionFacts,
  dismissBanner,
  emailFormPrefill,
  emailNeedsAddingAgain,
  isPhraseProtected,
  newWordsShown,
  protectBannerKeys,
  isProtectStepOwed,
  markProtectStepOwed,
  parseRecoveryEmailStatus,
  PROTECT_BANNER_RETURN_MS,
  protectBannerShown,
  protectionOf,
  protectionRows,
  protectionStatusKey,
  setPhraseProtected,
  showPhraseOffered,
  threeWordsShown,
  type RecoveryEmailStatus,
} from "./account-protection";
import { t } from "../../l10n";

const status = (over: Partial<RecoveryEmailStatus> = {}): RecoveryEmailStatus => ({
  available: true,
  enabled: false,
  vault: false,
  phraseSaved: false,
  totp: false,
  ...over,
});

describe("the account's mode", () => {
  it("is the email while no phrase is saved and the vault exists", () => {
    expect(accountProtection(true, true, false)).toBe("email");
  });

  it("is the phrase once one is saved, whatever the email", () => {
    for (const enabled of [false, true]) {
      for (const vault of [false, true]) {
        expect(accountProtection(enabled, vault, true)).toBe("phrase");
      }
    }
  });

  it("is none without an email and without a phrase", () => {
    expect(accountProtection(false, false, false)).toBe("none");
  });

  it("is none after a key reset or a recall dropped the vault: the email alone brings no keys", () => {
    // `enabled` true, `vault` false, `phrase_saved` false.
    expect(accountProtection(true, false, false)).toBe("none");
    expect(protectionOf(status({ enabled: true }))).toBe("none");
  });

  it("words each mode in the app's terms", () => {
    expect(t(protectionStatusKey("email"))).toBe("Protected by your email");
    expect(t(protectionStatusKey("phrase"))).toBe("Protected by your recovery phrase");
    expect(t(protectionStatusKey("none"))).toBe("Not protected");
  });
});

describe("reading GET /recovery-email", () => {
  it("takes the four facts", () => {
    expect(
      parseRecoveryEmailStatus({ available: true, enabled: true, vault: false, phrase_saved: true }),
    ).toEqual({ available: true, enabled: true, vault: false, phraseSaved: true, totp: false });
    // The authenticator fact, when the server says it.
    expect(
      parseRecoveryEmailStatus({ available: true, enabled: true, vault: true, phrase_saved: false, totp: true })
        ?.totp,
    ).toBe(true);
  });

  it("refuses a body that is not one", () => {
    expect(parseRecoveryEmailStatus(null)).toBeNull();
    expect(parseRecoveryEmailStatus({ available: true, enabled: true, vault: false })).toBeNull();
    expect(parseRecoveryEmailStatus({ available: 1, enabled: true, vault: false, phrase_saved: false })).toBeNull();
  });
});

describe("the Settings > Account rows", () => {
  it("offer to add the email while there is none", () => {
    expect(protectionRows(status(), true)).toEqual({
      email: { on: false, actions: ["add"] },
      phrase: "save",
    });
  });

  it("offer Replace and Remove once it is on, never the address", () => {
    const rows = protectionRows(status({ enabled: true, vault: true }), true);
    expect(rows.email).toEqual({ on: true, actions: ["replace", "remove"] });
  });

  it("offer to add it again when the vault is gone and no phrase is saved", () => {
    const rows = protectionRows(status({ enabled: true, vault: false }), true);
    expect(rows.email).toEqual({ on: true, actions: ["addAgain", "remove"] });
  });

  it("a phrase-protected account with an email has no vault and nothing to add again", () => {
    const rows = protectionRows(status({ enabled: true, phraseSaved: true }), true);
    expect(rows.email).toEqual({ on: true, actions: ["replace", "remove"] });
    expect(rows.phrase).toBe("show");
  });

  it("have no email row when mail is not set up on this server", () => {
    expect(protectionRows(status({ available: false }), true).email).toBeNull();
  });

  it("keep Remove for an email that was added before mail was switched off", () => {
    expect(protectionRows(status({ available: false, enabled: true, vault: true }), true).email).toEqual({
      on: true,
      actions: ["remove"],
    });
  });

  it("tell a guest account to choose a username first", () => {
    expect(protectionRows(status(), false).email).toEqual({ on: false, actions: ["choose-username"] });
  });

  it("show the phrase only for a phrase-protected account; every other one may save one", () => {
    expect(protectionRows(status(), true).phrase).toBe("save");
    expect(protectionRows(status({ enabled: true, vault: true }), true).phrase).toBe("save");
    expect(protectionRows(status({ phraseSaved: true }), true).phrase).toBe("show");
  });
});

describe("the no-protection banner", () => {
  const NOW = 1_800_000_000_000;

  it("is shown for an unprotected account without a passkey that never dismissed it", () => {
    expect(protectBannerShown("none", null, NOW, false)).toBe(true);
  });

  it("is never shown for a protected account", () => {
    expect(protectBannerShown("email", null, NOW, false)).toBe(false);
    expect(protectBannerShown("phrase", null, NOW, false)).toBe(false);
  });

  it("is not shown for an account with a passkey, nor does it return after 30 days (alignment O)", () => {
    expect(protectBannerShown("none", null, NOW, true)).toBe(false);
    expect(protectBannerShown("none", NOW - PROTECT_BANNER_RETURN_MS, NOW, true)).toBe(false);
  });

  it("stays away while the passkey list is not known", () => {
    expect(protectBannerShown("none", null, NOW, null)).toBe(false);
  });

  it("stays away for 30 days after it was dismissed, then returns", () => {
    expect(protectBannerShown("none", NOW - PROTECT_BANNER_RETURN_MS + 1, NOW, false)).toBe(false);
    expect(protectBannerShown("none", NOW - PROTECT_BANNER_RETURN_MS, NOW, false)).toBe(true);
  });

  it("a clock that went backwards keeps it hidden", () => {
    expect(protectBannerShown("none", NOW + 1_000, NOW, false)).toBe(false);
  });

  it("remembers the dismissal per account", () => {
    const local = new MemoryStorage();
    expect(bannerDismissedAt(local, "alice")).toBeNull();
    dismissBanner(local, "alice", NOW);
    expect(bannerDismissedAt(local, "alice")).toBe(NOW);
    expect(bannerDismissedAt(local, "bob")).toBeNull();
  });
});

describe("the three-word surfaces and Show phrase", () => {
  it("exist only for a phrase-protected account", () => {
    expect(threeWordsShown(true)).toBe(true);
    expect(threeWordsShown(false)).toBe(false);
  });

  it("Show phrase needs the phrase on this device as well", () => {
    expect(showPhraseOffered(true, true)).toBe(true);
    expect(showPhraseOffered(true, false)).toBe(false);
    // The words of an account without a saved phrase were never shown.
    expect(showPhraseOffered(false, true)).toBe(false);
  });
});

describe("the per-device facts", () => {
  it("keep the phrase-protected flag per account, and clear it", () => {
    const local = new MemoryStorage();
    expect(isPhraseProtected(local, "alice")).toBe(false);
    setPhraseProtected(local, "alice", true);
    expect(isPhraseProtected(local, "alice")).toBe(true);
    expect(isPhraseProtected(local, "bob")).toBe(false);
    setPhraseProtected(local, "alice", false);
    expect(isPhraseProtected(local, "alice")).toBe(false);
  });

  it("keep the protect step owed across a reload until the person chose", () => {
    const local = new MemoryStorage();
    expect(isProtectStepOwed(local, "alice")).toBe(false);
    markProtectStepOwed(local, "alice");
    expect(isProtectStepOwed(local, "alice")).toBe(true);
    expect(isProtectStepOwed(local, "bob")).toBe(false);
    clearProtectStepOwed(local, "alice");
    expect(isProtectStepOwed(local, "alice")).toBe(false);
  });

  it("are all forgotten by the sign-out wipe", () => {
    const local = new MemoryStorage();
    setPhraseProtected(local, "alice", true);
    markProtectStepOwed(local, "alice");
    dismissBanner(local, "alice", 5);
    clearProtectionFacts(local, "alice");
    expect(local.entries()).toEqual([]);
  });

  it("read as unset without storage", () => {
    expect(isPhraseProtected(undefined, "alice")).toBe(false);
    expect(isProtectStepOwed(undefined, "alice")).toBe(false);
    expect(bannerDismissedAt(undefined, "alice")).toBeNull();
    expect(() => markProtectStepOwed(undefined, "alice")).not.toThrow();
  });
});

describe("new words after the keys changed (alignment I)", () => {
  it("are shown only to a phrase-protected account", () => {
    expect(newWordsShown(true, true)).toBe(true);
    // A rotation, a reset or a recall on an account whose owner never saved
    // a phrase completes with no words shown anywhere.
    expect(newWordsShown(false, true)).toBe(false);
  });

  it("a guest account is never shown any", () => {
    expect(newWordsShown(false, false)).toBe(false);
    expect(newWordsShown(true, false)).toBe(false);
  });

  it("the account then reads Not protected and asks for the email again", () => {
    const after = status({ enabled: true, vault: false });
    expect(protectionOf(after)).toBe("none");
    expect(emailNeedsAddingAgain(after)).toBe(true);
    expect(protectBannerKeys(after)).toEqual({
      titleKey: "recovery_email.add_again.title",
      bodyKey: "recovery_email.add_again.body",
    });
    expect(t("recovery_email.add_again.title")).toBe("Add your email again");
  });

  it("an account that never had an email gets the ordinary banner", () => {
    expect(emailNeedsAddingAgain(status())).toBe(false);
    expect(protectBannerKeys(status())).toEqual({ titleKey: "protect.banner.none", bodyKey: null });
    // With a phrase saved, an email without a vault is the normal state.
    expect(emailNeedsAddingAgain(status({ enabled: true, phraseSaved: true }))).toBe(false);
    expect(emailNeedsAddingAgain(status({ enabled: true, vault: true }))).toBe(false);
  });
});

describe("a device that holds the address (amended 2026-10-13)", () => {
  const gone = status({ enabled: true, vault: false });

  it("is not asked to add the email again: it puts the vault back by itself", () => {
    expect(emailNeedsAddingAgain(gone, true)).toBe(false);
    expect(emailNeedsAddingAgain(gone, false)).toBe(true);
    expect(protectionRows(gone, true, true).email).toEqual({ on: true, actions: ["replace", "remove"] });
    expect(protectionRows(gone, true, false).email).toEqual({ on: true, actions: ["addAgain", "remove"] });
    expect(protectBannerKeys(gone, true)).toEqual({ titleKey: "protect.banner.none", bodyKey: null });
    expect(protectBannerKeys(gone, false).titleKey).toBe("recovery_email.add_again.title");
  });

  it("after a recall the server holds no email: the ordinary add state, pre-filled with the held address", () => {
    const recalled = status({ enabled: false });
    expect(protectionRows(recalled, true, true).email).toEqual({ on: false, actions: ["add"] });
    expect(emailNeedsAddingAgain(recalled, true)).toBe(false);
    expect(emailFormPrefill("add", "name@example.com")).toBe("name@example.com");
    expect(emailFormPrefill("addAgain", "name@example.com")).toBe("name@example.com");
    expect(emailFormPrefill("add", null)).toBe("");
  });

  it("Replace starts empty: a new address is being typed", () => {
    expect(emailFormPrefill("replace", "name@example.com")).toBe("");
  });
});

describe("the banner while a re-seal is pending (ADR-0076 pass B, item 6)", () => {
  it("is not shown: the device holds the address and is putting the vault back", () => {
    const NOW = 1_800_000_000_000;
    expect(protectBannerShown("none", null, NOW, false, true)).toBe(false);
    // Without the address it is (the add-again state).
    expect(protectBannerShown("none", null, NOW, false, false)).toBe(true);
  });
});
