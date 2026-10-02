// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
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
