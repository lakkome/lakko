// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  clearGuestAccount,
  guestAccountState,
  guestPhraseKey,
  guestScreen,
  markGuestAccount,
  noteGuestUsernameClaimed,
  type GuestMarkerStorage,
} from "./guest-account";

function memoryStorage(): GuestMarkerStorage & { readonly rows: Map<string, string> } {
  const rows = new Map<string, string>();
  return {
    rows,
    getItem: (key) => rows.get(key) ?? null,
    setItem: (key, value) => {
      rows.set(key, value);
    },
    removeItem: (key) => {
      rows.delete(key);
    },
  };
}

const throwingStorage: GuestMarkerStorage = {
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

describe("the guest marker", () => {
  it("reads an account nobody marked as not a guest", () => {
    expect(guestAccountState(memoryStorage(), "acct")).toBeNull();
    expect(guestAccountState(undefined, "acct")).toBeNull();
  });

  it("marks an account as a guest with no username yet", () => {
    const storage = memoryStorage();
    markGuestAccount(storage, "acct");
    expect(guestAccountState(storage, "acct")).toEqual({ claimedUsername: null });
  });

  it("is kept per account", () => {
    const storage = memoryStorage();
    markGuestAccount(storage, "one");
    expect(guestAccountState(storage, "two")).toBeNull();
  });

  it("remembers the username a half-finished Create account already took", () => {
    // A username cannot be changed, so the second attempt must go on with it.
    const storage = memoryStorage();
    markGuestAccount(storage, "acct");
    noteGuestUsernameClaimed(storage, "acct", "mira");
    expect(guestAccountState(storage, "acct")).toEqual({ claimedUsername: "mira" });
  });

  it("stops being a guest once cleared", () => {
    const storage = memoryStorage();
    markGuestAccount(storage, "acct");
    noteGuestUsernameClaimed(storage, "acct", "mira");
    clearGuestAccount(storage, "acct");
    expect(guestAccountState(storage, "acct")).toBeNull();
    expect(storage.rows.size).toBe(0);
  });

  it("does not treat a value this build did not write as a guest", () => {
    // The offer includes deleting the account: never on the strength of
    // unknown bytes.
    const storage = memoryStorage();
    storage.setItem("lakko:guest-account:acct", "1");
    expect(guestAccountState(storage, "acct")).toBeNull();
  });

  it("never throws when storage is blocked", () => {
    expect(() => markGuestAccount(throwingStorage, "acct")).not.toThrow();
    expect(() => noteGuestUsernameClaimed(throwingStorage, "acct", "mira")).not.toThrow();
    expect(() => clearGuestAccount(throwingStorage, "acct")).not.toThrow();
    expect(guestAccountState(throwingStorage, "acct")).toBeNull();
  });
});

describe("guestPhraseKey", () => {
  it("is never the account's own key in the phrase store", () => {
    // The backup and the three-word enrolment read the phrase by the account
    // id. A guest's phrase under that key would upload a backup and enrol
    // recovery for an account the person has not decided to keep.
    expect(guestPhraseKey("acct")).not.toBe("acct");
    expect(guestPhraseKey("one")).not.toBe(guestPhraseKey("two"));
  });
});

describe("guestScreen", () => {
  it("leaves an ordinary account alone", () => {
    expect(guestScreen({ guest: false, callInProgress: false })).toBe("none");
    expect(guestScreen({ guest: false, callInProgress: true })).toBe("none");
  });

  it("sends a guest back to the call while one runs", () => {
    // Never the offer here: one of its two buttons deletes the account.
    expect(guestScreen({ guest: true, callInProgress: true })).toBe("in-call");
  });

  it("asks the question once there is no call", () => {
    expect(guestScreen({ guest: true, callInProgress: false })).toBe("offer");
  });
});
