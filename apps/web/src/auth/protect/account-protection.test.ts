// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { MemoryStorage } from "../../test/fake-storage";
import {
  chatsKeptStatusKey,
  clearProtectStepOwed,
  clearProtectionFacts,
  emailFormPrefill,
  isPhraseProtected,
  isProtectStepOwed,
  markPasswordOnlyDialogSeen,
  markProtectionLooked,
  markProtectStepOwed,
  newWordsShown,
  parseRecoveryEmailStatus,
  passwordOnlyDialogSeen,
  protectionLookedBefore,
  protectionRows,
  setPhraseProtected,
  showPhraseOffered,
  threeWordsShown,
  type RecoveryEmailStatus,
} from "./account-protection";
import { t } from "../../l10n";

const status = (over: Partial<RecoveryEmailStatus> = {}): RecoveryEmailStatus => ({
  available: true,
  enabled: false,
  totp: false,
  ...over,
});

describe("the Settings > Account status line (UX 19.1c)", () => {
  it("says what keeps the chats, in the app's terms", () => {
    expect(t(chatsKeptStatusKey("phrase") ?? "")).toBe("Your chats are kept by your recovery phrase");
    expect(t(chatsKeptStatusKey("passkey") ?? "")).toBe("Your chats are kept by a passkey");
    expect(t(chatsKeptStatusKey("password") ?? "")).toBe("Your chats are kept by your password only");
  });

  it("draws no line while that is not known", () => {
    expect(chatsKeptStatusKey(null)).toBeNull();
  });
});

describe("reading GET /recovery-email", () => {
  it("takes the three facts", () => {
    expect(parseRecoveryEmailStatus({ available: true, enabled: true, totp: false })).toEqual({
      available: true,
      enabled: true,
      totp: false,
    });
    expect(parseRecoveryEmailStatus({ available: false, enabled: false, totp: true })?.totp).toBe(true);
  });

  it("reads a missing authenticator fact as none", () => {
    expect(parseRecoveryEmailStatus({ available: true, enabled: true })?.totp).toBe(false);
  });

  it("no longer needs, or carries, a vault or a saved-phrase flag", () => {
    const read = parseRecoveryEmailStatus({
      available: true,
      enabled: true,
      totp: false,
      vault: true,
      phrase_saved: true,
    });
    expect(read).toEqual({ available: true, enabled: true, totp: false });
  });

  it("refuses a body that is not one", () => {
    expect(parseRecoveryEmailStatus(null)).toBeNull();
    expect(parseRecoveryEmailStatus({ available: true })).toBeNull();
    expect(parseRecoveryEmailStatus({ available: 1, enabled: true })).toBeNull();
  });
});

describe("the Settings > Account rows", () => {
  it("offer to add the email while there is none", () => {
    expect(protectionRows(status(), true, false)).toEqual({
      email: { on: false, actions: ["add"] },
      phrase: "save",
    });
  });

  it("offer Replace and Remove once it is on, never the address", () => {
    expect(protectionRows(status({ enabled: true }), true, false).email).toEqual({
      on: true,
      actions: ["replace", "remove"],
    });
  });

  it("never ask to add the email again: nothing is sealed under it any more", () => {
    for (const phraseSaved of [false, true]) {
      const actions = protectionRows(status({ enabled: true }), true, phraseSaved).email?.actions ?? [];
      expect(actions).toEqual(["replace", "remove"]);
    }
  });

  it("have no email row when mail is not set up on this server", () => {
    expect(protectionRows(status({ available: false }), true, false).email).toBeNull();
  });

  it("keep Remove for an email that was added before mail was switched off", () => {
    expect(protectionRows(status({ available: false, enabled: true }), true, false).email).toEqual({
      on: true,
      actions: ["remove"],
    });
  });

  it("tell a guest account to choose a username first", () => {
    expect(protectionRows(status(), false, false).email).toEqual({ on: false, actions: ["choose-username"] });
  });

  it("show the phrase once it was saved; every other account may save one", () => {
    expect(protectionRows(status(), true, false).phrase).toBe("save");
    expect(protectionRows(status({ enabled: true }), true, false).phrase).toBe("save");
    expect(protectionRows(status(), true, true).phrase).toBe("show");
  });
});

describe("the three-word surfaces and Show phrase", () => {
  it("exist only once the phrase was saved", () => {
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
  it("keep the phrase-saved flag per account, and clear it", () => {
    const local = new MemoryStorage();
    expect(isPhraseProtected(local, "alice")).toBe(false);
    setPhraseProtected(local, "alice", true);
    expect(isPhraseProtected(local, "alice")).toBe(true);
    expect(isPhraseProtected(local, "bob")).toBe(false);
    setPhraseProtected(local, "alice", false);
    expect(isPhraseProtected(local, "alice")).toBe(false);
  });

  it("keep the protect step owed across a reload until Continue", () => {
    const local = new MemoryStorage();
    expect(isProtectStepOwed(local, "alice")).toBe(false);
    markProtectStepOwed(local, "alice");
    expect(isProtectStepOwed(local, "alice")).toBe(true);
    expect(isProtectStepOwed(local, "bob")).toBe(false);
    clearProtectStepOwed(local, "alice");
    expect(isProtectStepOwed(local, "alice")).toBe(false);
  });

  it("remember that the password-only dialog was seen, per account", () => {
    const local = new MemoryStorage();
    expect(passwordOnlyDialogSeen(local, "alice")).toBe(false);
    markPasswordOnlyDialogSeen(local, "alice");
    expect(passwordOnlyDialogSeen(local, "alice")).toBe(true);
    expect(passwordOnlyDialogSeen(local, "bob")).toBe(false);
  });

  it("remember that this device read the account's protection, per account", () => {
    const local = new MemoryStorage();
    expect(protectionLookedBefore(local, "alice")).toBe(false);
    markProtectionLooked(local, "alice");
    expect(protectionLookedBefore(local, "alice")).toBe(true);
    expect(protectionLookedBefore(local, "bob")).toBe(false);
  });

  it("are all forgotten by the sign-out wipe", () => {
    const local = new MemoryStorage();
    setPhraseProtected(local, "alice", true);
    markProtectStepOwed(local, "alice");
    markPasswordOnlyDialogSeen(local, "alice");
    markProtectionLooked(local, "alice");
    clearProtectionFacts(local, "alice");
    expect(local.entries()).toEqual([]);
  });

  it("read as unset without storage, and the dialog never nags where nothing can be remembered", () => {
    expect(isPhraseProtected(undefined, "alice")).toBe(false);
    expect(isProtectStepOwed(undefined, "alice")).toBe(false);
    expect(protectionLookedBefore(undefined, "alice")).toBe(false);
    expect(() => markProtectStepOwed(undefined, "alice")).not.toThrow();
    const blocked = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
      removeItem: () => {
        throw new Error("blocked");
      },
    };
    expect(passwordOnlyDialogSeen(blocked, "alice")).toBe(true);
  });
});

describe("new words after the keys changed (alignment I)", () => {
  it("are shown only to an account whose phrase had been saved", () => {
    expect(newWordsShown(true, true)).toBe(true);
    // A rotation, a reset or a recall on an account whose owner never saved
    // a phrase completes with no words shown anywhere.
    expect(newWordsShown(false, true)).toBe(false);
  });

  it("a guest account is never shown any", () => {
    expect(newWordsShown(false, false)).toBe(false);
    expect(newWordsShown(true, false)).toBe(false);
  });
});

describe("what the email form starts with", () => {
  it("adding pre-fills the address this device holds", () => {
    expect(emailFormPrefill("add", "name@example.com")).toBe("name@example.com");
    expect(emailFormPrefill("add", null)).toBe("");
  });

  it("Replace starts empty: a new address is being typed", () => {
    expect(emailFormPrefill("replace", "name@example.com")).toBe("");
  });
});
