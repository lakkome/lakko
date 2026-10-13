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

describe("the signup phrase gate", () => {
  it("keeps /signup on the phrase screen when signup lands the session", () => {
    // The defect: the anonymous guard saw `session` and redirected home in the
    // same tick signup produced the phrase, so the phrase was never rendered.
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

  it("never shows a signup phrase outside a live session", () => {
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
    expect(sessionGuardView("session", false, true)).toBe("guest");
  });

  it("leaves an ordinary session alone", () => {
    expect(sessionGuardView("session", false, false)).toBe("children");
  });

  it("puts a pending recovery phrase first", () => {
    // The guest who has just chosen to keep the account: the phrase must be
    // seen before anything else.
    expect(sessionGuardView("session", true, true)).toBe("save-signup-phrase");
  });

  it("means nothing without a session", () => {
    expect(sessionGuardView("anon", false, true)).toBe("redirect-login");
    expect(sessionGuardView("loading", false, true)).toBe("loading");
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
    // login, passkey (same device), and the access-recovery landing.
    expect(src.match(/await parkForRestore\(/g)?.length).toBe(3);
    // The one direct park left is the passkey fresh-device branch, which has
    // no device key and so no readable phrase cache.
    expect(src.match(/kind: "needs-recovery-phrase",\n/g)?.length).toBe(2);
    expect(src).toMatch(
      /if \(await completeParkFromOwedPhrase\(parked, flow\)\) return;\s*setState\(\{\s*kind: "needs-recovery-phrase"/,
    );
    // Through the ordinary, fail-closed phrase path - never a second one.
    expect(src).toMatch(/await api\.provideRecoveryPhrase\(phrase\);\s*return true;/);
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
    // Forget-device wipes both (`wipe-account.ts`).
    const wipe = readFileSync(fileURLToPath(new URL("./wipe-account.ts", import.meta.url)), "utf8");
    expect(wipe).toContain("deletePhrase(accountIdB64)");
    expect(wipe).toContain("clearSignupPhraseOwed(storage, accountIdB64)");
  });
});
