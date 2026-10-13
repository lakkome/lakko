// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  freshPhraseDisposition,
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
