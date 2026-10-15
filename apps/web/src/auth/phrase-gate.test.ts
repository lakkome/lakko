// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  freshPhraseDisposition,
  owedPhraseCompletesPark,
  anonymousGuardView,
  clearSignupPhraseOwed,
  isSignupPhraseOwed,
  markSignupPhraseOwed,
  phraseSourceClearsOwed,
  sessionGuardView,
  type GuardAuthKind,
  type MarkerStorage,
} from "./phrase-gate";

const ALL_KINDS: GuardAuthKind[] = [
  "loading",
  "anon",
  "session",
  "needs-recovery-phrase",
  "save-fresh-phrase",
];

describe("the sign-up's protect step (ADR-0075, UX 19.1a)", () => {
  it("keeps /signup on the step when signup lands the session", () => {
    // `/signup` sits in the anonymous guard, which would otherwise navigate
    // home in the same tick the session landed.
    expect(anonymousGuardView("session", false, false, true)).toBe("protect-account");
  });

  it("shows the step instead of a protected route while it is owed", () => {
    expect(sessionGuardView("session", false, false, true)).toBe("protect-account");
  });

  it("releases both guards once the person chose", () => {
    expect(sessionGuardView("session", false, false, false)).toBe("children");
    expect(anonymousGuardView("session", false, false, false)).toBe("redirect-home");
  });

  it("comes after owed new words and before the new-password step", () => {
    // "Save a recovery phrase" on the step ends in new words: they are
    // shown first, and never the step again in between.
    expect(sessionGuardView("session", true, false, true)).toBe("save-signup-phrase");
    expect(sessionGuardView("session", false, true, true)).toBe("protect-account");
  });

  it("is never shown outside a live session", () => {
    for (const kind of ALL_KINDS.filter((k) => k !== "session")) {
      expect(sessionGuardView(kind, false, false, true)).not.toBe("protect-account");
      expect(anonymousGuardView(kind, false, false, true)).not.toBe("protect-account");
    }
  });
});

describe("the new-words gate", () => {
  it("keeps a public auth page on the phrase screen while new words are owed", () => {
    expect(anonymousGuardView("session", true)).toBe("save-signup-phrase");
  });

  it("shows the phrase screen instead of a protected route while it is owed", () => {
    // Back button, a typed URL, or a reload that lands on `/`.
    expect(sessionGuardView("session", true)).toBe("save-signup-phrase");
  });

  it("releases both guards once the phrase is acknowledged", () => {
    expect(sessionGuardView("session", false)).toBe("children");
    // On a public auth page the release is a redirect (honouring ?return=).
    expect(anonymousGuardView("session", false)).toBe("redirect-home");
  });

  it("never shows new words outside a live session", () => {
    for (const kind of ALL_KINDS.filter((k) => k !== "session")) {
      expect(sessionGuardView(kind, true)).not.toBe("save-signup-phrase");
      expect(anonymousGuardView(kind, true)).not.toBe("save-signup-phrase");
    }
  });

  it("keeps the ADR-0017 identity screens on both guards", () => {
    for (const pending of [false, true]) {
      expect(sessionGuardView("needs-recovery-phrase", pending)).toBe("restore-identity");
      expect(anonymousGuardView("needs-recovery-phrase", pending)).toBe("restore-identity");
      // Also the ADR-0032 guest signup, whose phrase is parked before a session.
      expect(sessionGuardView("save-fresh-phrase", pending)).toBe("save-fresh-phrase");
      expect(anonymousGuardView("save-fresh-phrase", pending)).toBe("save-fresh-phrase");
    }
  });

  it("offers a new password after a forgot-password route, in a live session only", () => {
    expect(sessionGuardView("session", false, true)).toBe("choose-new-password");
    // The recovery screen sits in the anonymous guard: the step is shown
    // there instead of the redirect home.
    expect(anonymousGuardView("session", false, true)).toBe("choose-new-password");
    for (const kind of ALL_KINDS.filter((k) => k !== "session")) {
      expect(sessionGuardView(kind, false, true)).not.toBe("choose-new-password");
      expect(anonymousGuardView(kind, false, true)).not.toBe("choose-new-password");
    }
  });

  it("shows an owed signup phrase before the new-password step", () => {
    expect(sessionGuardView("session", true, true)).toBe("save-signup-phrase");
  });

  it("keeps a device without the keys on the restore prompt, not the password step", () => {
    expect(sessionGuardView("needs-recovery-phrase", false, true)).toBe("restore-identity");
    expect(anonymousGuardView("needs-recovery-phrase", false, true)).toBe("restore-identity");
  });

  it("releases the guards once the step is saved or skipped", () => {
    expect(sessionGuardView("session", false, false)).toBe("children");
    expect(anonymousGuardView("session", false, false)).toBe("redirect-home");
  });

  it("keeps the plain signed-in / signed-out routing", () => {
    expect(sessionGuardView("anon", false)).toBe("redirect-login");
    expect(anonymousGuardView("anon", false)).toBe("children");
    expect(sessionGuardView("loading", false)).toBe("loading");
    expect(anonymousGuardView("loading", false)).toBe("loading");
  });
});

function memoryStorage(): MarkerStorage & { size: () => number } {
  const map = new Map<string, string>();
  return {
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => void map.set(k, v),
    removeItem: (k) => void map.delete(k),
    size: () => map.size,
  };
}

describe("a guest account never lands in the app", () => {
  it("shows the guest screen in place of a protected route", () => {
    expect(sessionGuardView("session", false, false, false, true)).toBe("guest");
  });

  it("leaves an ordinary session alone", () => {
    expect(sessionGuardView("session", false, false, false, false)).toBe("children");
  });

  it("puts owed recovery words first", () => {
    expect(sessionGuardView("session", true, false, false, true)).toBe("save-signup-phrase");
  });

  it("comes before the steps of a kept account", () => {
    // While the marker is there the person is asked the guest question,
    // whatever else is owed.
    expect(sessionGuardView("session", false, true, true, true)).toBe("guest");
  });

  it("a guest that kept its account lands on Protect your account, shown no phrase", () => {
    // `keepGuestAccount` marks the step owed and then clears the marker.
    expect(sessionGuardView("session", false, false, true, false)).toBe("protect-account");
  });

  it("means nothing without a session", () => {
    expect(sessionGuardView("anon", false, false, false, true)).toBe("redirect-login");
    expect(sessionGuardView("loading", false, false, false, true)).toBe("loading");
  });
});

describe("the owed-phrase marker (surviving a reload)", () => {
  it("is set at signup and cleared on acknowledgement", () => {
    const storage = memoryStorage();
    expect(isSignupPhraseOwed(storage, "acct")).toBe(false);
    markSignupPhraseOwed(storage, "acct");
    expect(isSignupPhraseOwed(storage, "acct")).toBe(true);
    clearSignupPhraseOwed(storage, "acct");
    expect(isSignupPhraseOwed(storage, "acct")).toBe(false);
  });

  it("is per account", () => {
    const storage = memoryStorage();
    markSignupPhraseOwed(storage, "alice");
    expect(isSignupPhraseOwed(storage, "bob")).toBe(false);
  });

  it("stores only a flag, never the phrase", () => {
    const storage = memoryStorage();
    markSignupPhraseOwed(storage, "acct");
    expect(storage.getItem("lakko:signup-phrase-unacknowledged:acct")).toBe("1");
    expect(storage.size()).toBe(1);
  });

  it("degrades to 'not owed' with no storage or blocked storage", () => {
    expect(isSignupPhraseOwed(undefined, "acct")).toBe(false);
    const blocked: MarkerStorage = {
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
    expect(() => markSignupPhraseOwed(blocked, "acct")).not.toThrow();
    expect(isSignupPhraseOwed(blocked, "acct")).toBe(false);
  });
});

describe("a freshly minted phrase after its identity was published", () => {
  it("is shown while the browser is still the minting account's", () => {
    // Binding unchanged (still parked), or the same account parked again.
    expect(freshPhraseDisposition({ mintedFor: "alice", boundAccount: "alice" })).toBe("show");
  });

  it("is shown after a mere sign-out", () => {
    expect(freshPhraseDisposition({ mintedFor: "alice", boundAccount: null })).toBe("show");
  });

  it("is not shown over another account, and stays owed to the minting one", () => {
    expect(freshPhraseDisposition({ mintedFor: "alice", boundAccount: "bob" })).toBe("owe");
    // What `mintFreshIdentity` does with "owe": the marker is the minting
    // account's own, so bob's sign-ins never see it and alice's next one does.
    const storage = memoryStorage();
    markSignupPhraseOwed(storage, "alice");
    expect(isSignupPhraseOwed(storage, "bob")).toBe(false);
    expect(isSignupPhraseOwed(storage, "alice")).toBe(true);
    // Shown as the unacknowledged-phrase screen once alice's session lands.
    expect(sessionGuardView("session", true)).toBe("save-signup-phrase");
    clearSignupPhraseOwed(storage, "alice");
    expect(isSignupPhraseOwed(storage, "alice")).toBe(false);
  });
});

describe("a sign-in that would park while its phrase is still owed", () => {
  it("is completed from the kept phrase only when it is owed and a device key holds it", () => {
    expect(owedPhraseCompletesPark({ owed: true, deviceKeyPresent: true })).toBe(true);
    expect(owedPhraseCompletesPark({ owed: false, deviceKeyPresent: true })).toBe(false);
    expect(owedPhraseCompletesPark({ owed: true, deviceKeyPresent: false })).toBe(false);
  });

  // `AuthContext.tsx` needs a Solid runtime this suite does not have.
  const src = readFileSync(fileURLToPath(new URL("./AuthContext.tsx", import.meta.url)), "utf8");

  it("every park with a device key goes through the owed-phrase completion", () => {
    // The password sign-in and the access-recovery landing, which every
    // ACCESS-only route shares (a passkey, an authenticator code, three
    // words, a mailed code).
    expect(src.match(/await parkForRestore\(/g)?.length).toBe(2);
    // No direct park is left: the restore prompt is put up in one place.
    expect(src.match(/kind: "needs-recovery-phrase",\n/g)?.length).toBe(1);
    // The owed phrase is tried first, then the passkey route's own hook.
    expect(src).toMatch(
      /if \(await completeParkFromOwedPhrase\(parked, flow\)\) return;\s*if \(restoreKeys !== undefined\) \{/,
    );
    // Through the ordinary, fail-closed phrase path - never a second one.
    expect(src).toMatch(/await withParkRouteWipes\(flow\.wipes, \(\) => api\.provideRecoveryPhrase\(phrase, "device"\)\);\s*return true;/);
  });

  it("a plain sign-out keeps the owed marker and the cached phrase", () => {
    const start = src.indexOf("\n    logout() {");
    const end = src.indexOf("\n    async forgetDevice() {");
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    const logout = src.slice(start, end);
    expect(logout).not.toContain("clearSignupPhraseOwed");
    expect(logout).not.toContain("deletePhrase");
    expect(logout).not.toContain("phraseOwedInMemory.delete");
    expect(logout).not.toContain("clearOwedPhraseScreen");
    // Forget-device wipes both (`wipe-account.ts`).
    const wipe = readFileSync(fileURLToPath(new URL("./wipe-account.ts", import.meta.url)), "utf8");
    expect(wipe).toContain("deletePhrase(accountIdB64)");
    expect(wipe).toContain("clearSignupPhraseOwed(storage, accountIdB64)");
  });
});

describe("a phrase the person typed clears the owed screen (ADR-0077 decision 3 rule 6)", () => {
  it("only a typed phrase clears it", () => {
    expect(phraseSourceClearsOwed("typed")).toBe(true);
    expect(phraseSourceClearsOwed("device")).toBe(false);
    expect(phraseSourceClearsOwed("vault")).toBe(false);
  });

  const read = (rel: string): string =>
    readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");
  const src = read("./AuthContext.tsx");
  const body = (name: string): string => {
    const start = src.indexOf(`\n    async ${name}(`);
    expect(start).toBeGreaterThan(-1);
    return src.slice(start, src.indexOf("\n    },\n", start));
  };

  it("the two phrase routes clear it after the phrase is stored, and default to leaving it owed", () => {
    for (const [name, label] of [
      ["provideRecoveryPhrase", '"provide-phrase"'],
      ["recover", '"recover"'],
    ] as const) {
      const b = body(name);
      expect(b).toMatch(/source = "vault"\) \{/);
      const stored = b.indexOf(label);
      const cleared = b.indexOf("if (phraseSourceClearsOwed(source)) clearOwedPhraseScreen(");
      expect(stored).toBeGreaterThan(-1);
      expect(cleared).toBeGreaterThan(stored);
    }
  });

  it("each call site says where its phrase came from", () => {
    // Typed by the person.
    expect(read("./RestoreIdentityPrompt.tsx")).toContain('await auth.provideRecoveryPhrase(phrase(), "typed");');
    expect(read("./recover/PhraseRecover.tsx")).toContain(
      'await auth.recover(phrase(), password(), canonicalUsername, "typed");',
    );
    // Opened from the email vault.
    expect(read("./recover/EmailRecover.tsx")).toContain(
      'await auth.recover(phrase, password(), canonicalUsername, "vault");',
    );
    // This device's kept row, and a passkey's vault: still shown.
    expect(src).toContain('api.provideRecoveryPhrase(phrase, "device")');
    expect(src).toContain('api.provideRecoveryPhrase(phrase, "vault")');
    expect(src.match(/api\.provideRecoveryPhrase\(/g)?.length).toBe(2);
  });
});
