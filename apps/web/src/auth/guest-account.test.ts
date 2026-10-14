// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  GUEST_LIFETIME_MS,
  clearGuestAccount,
  guestAccountState,
  guestDeletePlan,
  guestExpired,
  guestPhraseKey,
  guestRestoreAction,
  guestScreen,
  markGuestAccount,
  noteGuestUsernameClaimed,
  type GuestMarkerStorage,
} from "./guest-account";

const T0 = 1_790_000_000_000;

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
    markGuestAccount(storage, "acct", T0);
    expect(guestAccountState(storage, "acct")).toEqual({ markedAtMs: T0, claimedUsername: null });
  });

  it("is kept per account", () => {
    const storage = memoryStorage();
    markGuestAccount(storage, "one", T0);
    expect(guestAccountState(storage, "two")).toBeNull();
  });

  it("remembers the username a half-finished Create account already took", () => {
    // A username cannot be changed, so the second attempt must go on with it.
    const storage = memoryStorage();
    markGuestAccount(storage, "acct", T0);
    noteGuestUsernameClaimed(storage, "acct", "mira");
    // The age is kept: taking a name does not buy the account another day.
    expect(guestAccountState(storage, "acct")).toEqual({ markedAtMs: T0, claimedUsername: "mira" });
  });

  it("does not turn an ordinary account into a guest by noting a name", () => {
    const storage = memoryStorage();
    noteGuestUsernameClaimed(storage, "acct", "mira");
    expect(guestAccountState(storage, "acct")).toBeNull();
  });

  it("stops being a guest once cleared", () => {
    const storage = memoryStorage();
    markGuestAccount(storage, "acct", T0);
    noteGuestUsernameClaimed(storage, "acct", "mira");
    clearGuestAccount(storage, "acct");
    expect(guestAccountState(storage, "acct")).toBeNull();
    expect(storage.rows.size).toBe(0);
  });

  it("does not treat a value this build did not write as a guest", () => {
    // The offer includes deleting the account: never on the strength of
    // unknown bytes.
    const storage = memoryStorage();
    for (const value of ["1", "guest", "guest@", "guest@soon", "named@12", "named@x:mira", "named:mira"]) {
      storage.setItem("lakko:guest-account:acct", value);
      expect(guestAccountState(storage, "acct")).toBeNull();
    }
  });

  it("never throws when storage is blocked", () => {
    expect(() => markGuestAccount(throwingStorage, "acct", T0)).not.toThrow();
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

describe("a guest account lives less than a day on a device", () => {
  const guest = { markedAtMs: T0, claimedUsername: null };

  it("is not expired while younger than the lifetime", () => {
    expect(guestExpired(guest, T0)).toBe(false);
    expect(guestExpired(guest, T0 + GUEST_LIFETIME_MS - 1)).toBe(false);
  });

  it("is expired from the lifetime on", () => {
    expect(guestExpired(guest, T0 + GUEST_LIFETIME_MS)).toBe(true);
    expect(guestExpired(guest, T0 + 10 * GUEST_LIFETIME_MS)).toBe(true);
  });

  it("stops before the servers' own day, never after it", () => {
    // The servers delete an unkept account 24 hours after it was made. The
    // device must stop offering Create account before that.
    expect(GUEST_LIFETIME_MS).toBeLessThan(24 * 60 * 60 * 1000);
  });

  it("does not read a clock set back as an old account", () => {
    expect(guestExpired(guest, T0 - 5 * GUEST_LIFETIME_MS)).toBe(false);
  });
});

describe("guestRestoreAction", () => {
  const young = { markedAtMs: T0, claimedUsername: null };
  const later = T0 + GUEST_LIFETIME_MS;

  it("leaves an ordinary account to the ordinary restore", () => {
    expect(guestRestoreAction({ guest: null, restorable: true, nowMs: later })).toBe("none");
    expect(guestRestoreAction({ guest: null, restorable: false, nowMs: later })).toBe("none");
  });

  it("restores a young guest", () => {
    expect(guestRestoreAction({ guest: young, restorable: true, nowMs: T0 + 1 })).toBe("none");
  });

  it("deletes a guest past its lifetime", () => {
    expect(guestRestoreAction({ guest: young, restorable: true, nowMs: later })).toBe("discard");
  });

  it("wipes a guest whose session cannot be restored, whatever its age", () => {
    // No password exists that anyone could type: a signed-out guest account
    // is unreachable, and its phrase must not stay on the device.
    expect(guestRestoreAction({ guest: young, restorable: false, nowMs: T0 + 1 })).toBe("wipe");
    expect(guestRestoreAction({ guest: young, restorable: false, nowMs: later })).toBe("wipe");
  });
});

describe("guestDeletePlan", () => {
  it("lets the auth service decide for an unnamed guest", () => {
    expect(guestDeletePlan({ markedAtMs: T0, claimedUsername: null })).toEqual([
      { service: "auth", required: true },
      { service: "directory", required: false },
    ]);
  });

  it("frees a taken username first, and requires it", () => {
    // The directory never sweeps an entry that has a username: skipping this
    // delete would leave the name taken for good.
    expect(guestDeletePlan({ markedAtMs: T0, claimedUsername: "mira" })).toEqual([
      { service: "directory", required: true },
      { service: "auth", required: true },
    ]);
  });
});
