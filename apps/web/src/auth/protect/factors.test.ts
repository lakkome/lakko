// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { t } from "../../l10n";
import {
  accountFactors,
  CODE_LAST_FACTOR,
  factorsAfterRemoving,
  factorsOf,
  hasSecondFactor,
  isLastFactorRefusal,
  notePasskeyCount,
  refreshFactors,
  removeNoticeKey,
  removingLeavesNone,
  removingLeavesTotpOnly,
  totpIsOnlyFactor,
  totpOnlyWarningKey,
  type AccountFactors,
} from "./factors";
import { adoptProtectionStatus, dropProtectionStatus } from "./protection-state";

const f = (over: Partial<AccountFactors> = {}): AccountFactors => ({
  email: false,
  totp: false,
  passkeys: 0,
  mailAvailable: true,
  ...over,
});

describe("the account's second ways in", () => {
  it("any one of the three is enough", () => {
    expect(hasSecondFactor(f())).toBe(false);
    expect(hasSecondFactor(f({ email: true }))).toBe(true);
    expect(hasSecondFactor(f({ totp: true }))).toBe(true);
    expect(hasSecondFactor(f({ passkeys: 1 }))).toBe(true);
  });

  it("whether mail is set up is not a factor", () => {
    expect(hasSecondFactor(f({ mailAvailable: true }))).toBe(false);
  });

  it("the authenticator is the only one when there is no email and no passkey", () => {
    expect(totpIsOnlyFactor(f({ totp: true }))).toBe(true);
    expect(totpIsOnlyFactor(f({ totp: true, email: true }))).toBe(false);
    expect(totpIsOnlyFactor(f({ totp: true, passkeys: 1 }))).toBe(false);
    expect(totpIsOnlyFactor(f())).toBe(false);
  });

  it("puts the two reads together", () => {
    expect(factorsOf({ available: false, enabled: true, totp: true }, 2)).toEqual({
      email: true,
      totp: true,
      passkeys: 2,
      mailAvailable: false,
    });
    expect(factorsOf({ available: true, enabled: false, totp: false }, -3).passkeys).toBe(0);
  });
});

describe("the last one cannot be removed (UX 19.1c)", () => {
  it("the email, when it is the only one", () => {
    expect(removingLeavesNone(f({ email: true }), "email")).toBe(true);
  });

  it("the last passkey, when nothing else is there", () => {
    expect(removingLeavesNone(f({ passkeys: 1 }), "passkey")).toBe(true);
  });

  it("not when something else stays", () => {
    expect(removingLeavesNone(f({ email: true, totp: true }), "email")).toBe(false);
    expect(removingLeavesNone(f({ email: true, passkeys: 1 }), "email")).toBe(false);
    expect(removingLeavesNone(f({ passkeys: 2 }), "passkey")).toBe(false);
    expect(removingLeavesNone(f({ passkeys: 1, email: true }), "passkey")).toBe(false);
    expect(removingLeavesNone(f({ passkeys: 1, totp: true }), "passkey")).toBe(false);
  });

  it("removing something the account does not have is never the last", () => {
    expect(removingLeavesNone(f(), "email")).toBe(false);
    expect(removingLeavesNone(f(), "passkey")).toBe(false);
    expect(removingLeavesNone(f({ passkeys: 1 }), "email")).toBe(false);
  });

  it("after a removal one passkey, or the email, is gone", () => {
    expect(factorsAfterRemoving(f({ passkeys: 2 }), "passkey").passkeys).toBe(1);
    expect(factorsAfterRemoving(f({ passkeys: 0 }), "passkey").passkeys).toBe(0);
    expect(factorsAfterRemoving(f({ email: true }), "email").email).toBe(false);
  });

  it("reads the server's refusal", () => {
    expect(isLastFactorRefusal({ status: 409, errorCode: CODE_LAST_FACTOR })).toBe(true);
    expect(isLastFactorRefusal({ status: 409, errorCode: "handle-taken" })).toBe(false);
    expect(isLastFactorRefusal({ status: 403, errorCode: CODE_LAST_FACTOR })).toBe(false);
    expect(isLastFactorRefusal({ status: null, errorCode: null })).toBe(false);
  });
});

describe("a removal that would leave only the authenticator", () => {
  it("says so before its buttons", () => {
    expect(removingLeavesTotpOnly(f({ totp: true, email: true }), "email")).toBe(true);
    expect(removingLeavesTotpOnly(f({ totp: true, passkeys: 1 }), "passkey")).toBe(true);
    expect(removeNoticeKey(f({ totp: true, email: true }), "email", false)).toBe("protect.totp_only.remove_notice");
    expect(removeNoticeKey(f({ totp: true, passkeys: 1 }), "passkey", true)).toBe(
      "protect.totp_only.remove_notice_phrase",
    );
  });

  it("says nothing when another way stays, or there is no authenticator", () => {
    expect(removingLeavesTotpOnly(f({ totp: true, email: true, passkeys: 1 }), "email")).toBe(false);
    expect(removingLeavesTotpOnly(f({ totp: true, passkeys: 2 }), "passkey")).toBe(false);
    expect(removingLeavesTotpOnly(f({ email: true, passkeys: 1 }), "email")).toBe(false);
    expect(removeNoticeKey(f({ email: true, passkeys: 1 }), "email", false)).toBeNull();
  });

  it("says nothing when the authenticator was already the only one, or the factor is not there", () => {
    expect(removingLeavesTotpOnly(f({ totp: true }), "email")).toBe(false);
    expect(removingLeavesTotpOnly(f({ totp: true }), "passkey")).toBe(false);
  });

  it("says nothing while the factors are not known", () => {
    expect(removeNoticeKey(null, "email", false)).toBeNull();
  });

  it("the lines are in the catalog", () => {
    expect(t("protect.totp_only.remove_notice")).toContain("authenticator app");
    expect(t("protect.totp_only.remove_notice_phrase")).toContain("recovery phrase");
  });
});

describe("the authenticator-only warning (UX 19.1a, 19.1c)", () => {
  it("is shown while the authenticator is the only one, worded by whether a phrase is saved", () => {
    expect(totpOnlyWarningKey(f({ totp: true }), false)).toBe("protect.totp_only.warning");
    expect(totpOnlyWarningKey(f({ totp: true }), true)).toBe("protect.totp_only.warning_phrase");
  });

  it("is not shown otherwise, nor while the factors are not known", () => {
    expect(totpOnlyWarningKey(f({ totp: true, email: true }), false)).toBeNull();
    expect(totpOnlyWarningKey(f({ passkeys: 1 }), false)).toBeNull();
    expect(totpOnlyWarningKey(null, false)).toBeNull();
  });
});

describe("the holder", () => {
  it("is unknown until both reads are known for the account", () => {
    dropProtectionStatus();
    expect(accountFactors("alice")).toBeNull();
    adoptProtectionStatus("alice", { available: true, enabled: true, totp: false });
    // The passkey list was not read yet.
    expect(accountFactors("alice")).toBeNull();
    notePasskeyCount("alice", 1);
    expect(accountFactors("alice")).toEqual({ email: true, totp: false, passkeys: 1, mailAvailable: true });
    // Another account's facts are not this one's.
    expect(accountFactors("bob")).toBeNull();
    expect(accountFactors(null)).toBeNull();
    dropProtectionStatus();
    expect(accountFactors("alice")).toBeNull();
  });

  it("follows a change of either read", () => {
    adoptProtectionStatus("carol", { available: true, enabled: false, totp: true });
    notePasskeyCount("carol", 0);
    expect(accountFactors("carol")?.totp).toBe(true);
    adoptProtectionStatus("carol", { available: true, enabled: true, totp: true });
    expect(accountFactors("carol")?.email).toBe(true);
    notePasskeyCount("carol", 3);
    expect(accountFactors("carol")?.passkeys).toBe(3);
    dropProtectionStatus();
  });
});

describe("reading the factors from the server", () => {
  it("resolves both facts and records the passkey count", async () => {
    const got = await refreshFactors("dave", "token", {
      status: async () => ({ available: true, enabled: true, totp: false }),
      passkeyCount: async () => 2,
    });
    expect(got).toEqual({ email: true, totp: false, passkeys: 2, mailAvailable: true });
  });

  it("resolves null when either read failed, and never throws", async () => {
    expect(
      await refreshFactors("erin", "token", {
        status: async () => null,
        passkeyCount: async () => 1,
      }),
    ).toBeNull();
    expect(
      await refreshFactors("erin", "token", {
        status: async () => ({ available: true, enabled: false, totp: false }),
        passkeyCount: async () => {
          throw new Error("offline");
        },
      }),
    ).toBeNull();
    expect(
      await refreshFactors("erin", "token", {
        status: async () => {
          throw new Error("offline");
        },
        passkeyCount: async () => 0,
      }),
    ).toBeNull();
  });
});
