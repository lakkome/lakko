// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0099 step 3: the pure rules of suspended mode. The `403
 * account-suspended` body, the suspension details, the reason name to its
 * catalog key, the one transition rule (enter on a landing, enter through a
 * refresh, leave on a normal refresh), and the appeal (sent once).
 */
import { describe, expect, it, vi } from "vitest";

import {
  appealFailureKey,
  appealLineKey,
  appealOffered,
  appealSendable,
  nextSuspension,
  reasonCatalogKey,
  sendAppeal,
  suspendedLandingOf,
  suspendedScreenActions,
  suspensionStatusOf,
  type AppealPorts,
  type SuspensionMode,
} from "./suspension";
import { AccountSuspendedError, authHttpErrorFrom, suspendedLandingFrom } from "./client";
import en from "../l10n/catalog/source.json";

const ACCOUNT_URL = "AAECAwQFBgcICQoLDA0ODw"; // bytes 0..15, URL-safe, no padding
const ACCOUNT_STD = "AAECAwQFBgcICQoLDA0ODw==";

function body(extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    error: "account-suspended",
    account_id_b64: ACCOUNT_URL,
    restricted_token: `s1.${"a".repeat(140)}`,
    restricted_expires_at_ms: 1_800_000_900_000,
    refresh_token: `r2.${"b".repeat(140)}`,
    refresh_expires_at_ms: 1_802_000_000_000,
    key_generation: 3,
    ...extra,
  };
}

describe("suspendedLandingOf", () => {
  it("reads the credentials and keys the account like every store does", () => {
    const landing = suspendedLandingOf(JSON.stringify(body()));
    expect(landing).not.toBeNull();
    expect(landing?.accountIdB64).toBe(ACCOUNT_STD);
    expect(landing?.session.restrictedToken.startsWith("s1.")).toBe(true);
    expect(landing?.session.keyGeneration).toBe(3n);
    expect(landing?.freshGrant).toBeNull();
  });

  it("carries the fresh-auth grant of a password or passkey sign-in", () => {
    const landing = suspendedLandingOf(
      body({ fresh_grant: "k1.grant", fresh_grant_expires_at_ms: 1_800_000_600_000 }),
    );
    expect(landing?.freshGrant).toEqual({ grant: "k1.grant", expiresAtMs: 1_800_000_600_000 });
  });

  it("is null for a bearer route's refusal and for any malformed field", () => {
    expect(suspendedLandingOf(JSON.stringify({ error: "account-suspended" }))).toBeNull();
    expect(suspendedLandingOf(body({ error: "fresh-auth-required" }))).toBeNull();
    expect(suspendedLandingOf(body({ restricted_token: "v3.not-restricted" }))).toBeNull();
    expect(suspendedLandingOf(body({ account_id_b64: "AAEC" }))).toBeNull();
    expect(suspendedLandingOf(body({ refresh_expires_at_ms: "soon" }))).toBeNull();
    expect(suspendedLandingOf(body({ key_generation: 0 }))).toBeNull();
    expect(suspendedLandingOf("not json")).toBeNull();
  });
});

describe("authHttpErrorFrom", () => {
  it("reads a 403 account-suspended whole and never puts the credentials in the message", async () => {
    const text = JSON.stringify(body({ fresh_grant: `k1.${"c".repeat(120)}`, fresh_grant_expires_at_ms: 1 }));
    expect(text.length).toBeGreaterThan(512);
    const err = await authHttpErrorFrom(new Response(text, { status: 403 }), "/login/finish");
    expect(err).toBeInstanceOf(AccountSuspendedError);
    expect(err.status).toBe(403);
    expect(err.errorCode).toBe("account-suspended");
    expect(err.message).not.toContain("s1.");
    expect(err.message).not.toContain("r2.");
    expect(suspendedLandingFrom(err)?.accountIdB64).toBe(ACCOUNT_STD);
  });

  it("gives a bearer route's refusal no landing, and leaves any other 403 alone", async () => {
    const bare = await authHttpErrorFrom(
      new Response(JSON.stringify({ error: "account-suspended" }), { status: 403 }),
      "/auth/tokens",
    );
    expect(bare).toBeInstanceOf(AccountSuspendedError);
    expect(suspendedLandingFrom(bare)).toBeNull();
    const other = await authHttpErrorFrom(
      new Response(JSON.stringify({ error: "fresh-auth-required" }), { status: 403 }),
      "/account",
    );
    expect(other).not.toBeInstanceOf(AccountSuspendedError);
    expect(other.errorCode).toBe("fresh-auth-required");
  });
});

describe("suspensionStatusOf", () => {
  it("reads both answers", () => {
    expect(suspensionStatusOf({ suspended: false })).toEqual({ suspended: false });
    expect(
      suspensionStatusOf({
        suspended: true,
        reason: 6,
        reason_name: "child-safety",
        until_ms: null,
        appeal: "sent",
      }),
    ).toEqual({
      suspended: true,
      details: { reason: 6, reasonName: "child-safety", untilMs: null, appeal: "sent" },
    });
    expect(
      suspensionStatusOf({ suspended: true, reason: 1, reason_name: "spam", until_ms: 1_900_000_000_000, appeal: "none" }),
    ).toMatchObject({ details: { untilMs: 1_900_000_000_000, appeal: "none" } });
  });

  it("is null for a body of the wrong shape", () => {
    expect(suspensionStatusOf({})).toBeNull();
    expect(suspensionStatusOf({ suspended: true, until_ms: "tomorrow" })).toBeNull();
    expect(suspensionStatusOf("nope")).toBeNull();
  });
});

describe("reasonCatalogKey", () => {
  it("maps every reason the server names to the report sheet's key, which exists", () => {
    const messages = (en as { messages?: Record<string, unknown> }).messages ?? (en as Record<string, unknown>);
    const pairs: [string, string][] = [
      ["spam", "report.reason.spam"],
      ["harassment", "report.reason.harassment"],
      ["hate", "report.reason.hate"],
      ["violence", "report.reason.violence"],
      ["sexual", "report.reason.sexual"],
      ["child-safety", "report.reason.child_safety"],
      ["impersonation", "report.reason.impersonation"],
      ["other", "report.reason.other"],
    ];
    for (const [name, key] of pairs) {
      expect(reasonCatalogKey(name)).toBe(key);
      expect(Object.prototype.hasOwnProperty.call(messages, key)).toBe(true);
    }
  });

  it("reads a name it does not know as Something else, never as an identifier", () => {
    expect(reasonCatalogKey("unknown")).toBe("report.reason.other");
    expect(reasonCatalogKey("new-reason")).toBe("report.reason.other");
    expect(reasonCatalogKey("")).toBe("report.reason.other");
  });
});

describe("nextSuspension", () => {
  const A = "QUFBQQ==";
  const B = "QkJCQg==";
  const reading: SuspensionMode = { accountIdB64: A, username: "ann", canRead: true, reading: true };

  it("enters on a landing route's 403, on the screen", () => {
    expect(
      nextSuspension(null, { kind: "landed", accountIdB64: A, username: "ann", canRead: true }),
    ).toEqual({ accountIdB64: A, username: "ann", canRead: true, reading: false });
  });

  it("enters through a refresh (a bearer route's 403), and a person reading keeps reading", () => {
    const entered = nextSuspension(null, {
      kind: "refresh-suspended",
      accountIdB64: A,
      username: "ann",
      canRead: true,
    });
    expect(entered).toEqual({ accountIdB64: A, username: "ann", canRead: true, reading: false });
    expect(
      nextSuspension(reading, { kind: "refresh-suspended", accountIdB64: A, username: "ann", canRead: true }),
    ).toBe(reading);
  });

  it("leaves silently on a normal refresh of the same account, not of another", () => {
    expect(nextSuspension(reading, { kind: "refresh-pair", accountIdB64: A })).toBeNull();
    expect(nextSuspension(reading, { kind: "refresh-pair", accountIdB64: B })).toBe(reading);
  });

  it("opens the chats only where there are chats to read", () => {
    const screen = { ...reading, reading: false };
    expect(nextSuspension(screen, { kind: "read" })).toEqual(reading);
    const alone = { ...screen, canRead: false };
    expect(nextSuspension(alone, { kind: "read" })).toBe(alone);
  });

  it("shows the screen again, and ends on sign-out", () => {
    expect(nextSuspension(reading, { kind: "show-screen" })).toEqual({ ...reading, reading: false });
    expect(nextSuspension(reading, { kind: "ended" })).toBeNull();
    expect(nextSuspension(null, { kind: "read" })).toBeNull();
  });
});

describe("the appeal", () => {
  function ports(overrides: Partial<AppealPorts> = {}): AppealPorts & { posted: Uint8Array[] } {
    const posted: Uint8Array[] = [];
    return {
      posted,
      rootKey: () => new Uint8Array(32),
      fetchList: async () => new Uint8Array([1, 2, 3]),
      seal: () => new Uint8Array([9]),
      post: async (sealed) => {
        posted.push(sealed);
        return { status: 204, errorCode: null };
      },
      ...overrides,
    };
  }

  it("is offered once: not after it was sent, not after a decline", () => {
    expect(appealOffered("none")).toBe(true);
    expect(appealOffered("sent")).toBe(false);
    expect(appealOffered("declined")).toBe(false);
    expect(appealLineKey("none")).toBeNull();
    expect(appealLineKey("sent")).toBe("suspended.appeal.sent");
    expect(appealLineKey("declined")).toBe("suspended.appeal.declined");
  });

  it("takes 1 to 1,000 characters once trimmed", () => {
    expect(appealSendable("   ")).toBe(false);
    expect(appealSendable("x")).toBe(true);
    expect(appealSendable("x".repeat(1000))).toBe(true);
    expect(appealSendable("x".repeat(1001))).toBe(false);
    // Characters, not UTF-16 units.
    expect(appealSendable("\u{1F600}".repeat(1000))).toBe(true);
  });

  it("seals to the list and posts it", async () => {
    const p = ports();
    expect(await sendAppeal("It was a mistake.", p)).toBe("sent");
    expect(p.posted).toEqual([new Uint8Array([9])]);
  });

  it("counts an appeal the server already holds as sent", async () => {
    const p = ports({ post: async () => ({ status: 409, errorCode: "appeal-already-sent" }) });
    expect(await sendAppeal("again", p)).toBe("sent");
  });

  it("knows a suspension that ended", async () => {
    const p = ports({ post: async () => ({ status: 409, errorCode: "not-suspended" }) });
    expect(await sendAppeal("late", p)).toBe("not-suspended");
  });

  it("is unavailable without a pinned root, without a list, or with a list that does not verify", async () => {
    const post = vi.fn();
    expect(await sendAppeal("x", ports({ rootKey: () => null, post }))).toBe("unavailable");
    expect(await sendAppeal("x", ports({ fetchList: async () => null, post }))).toBe("unavailable");
    expect(
      await sendAppeal(
        "x",
        ports({
          seal: () => {
            throw new Error("bad-signature: the list does not verify");
          },
          post,
        }),
      ),
    ).toBe("unavailable");
    expect(post).not.toHaveBeenCalled();
    expect(appealFailureKey("unavailable")).toBe("suspended.appeal.unavailable");
  });

  it("fails, to be tried again, on no answer or another refusal", async () => {
    expect(
      await sendAppeal(
        "x",
        ports({
          fetchList: async () => {
            throw new Error("offline");
          },
        }),
      ),
    ).toBe("failed");
    expect(
      await sendAppeal(
        "x",
        ports({
          post: async () => {
            throw new Error("offline");
          },
        }),
      ),
    ).toBe("failed");
    expect(await sendAppeal("x", ports({ post: async () => ({ status: 500, errorCode: null }) }))).toBe("failed");
    expect(appealFailureKey("failed")).toBe("suspended.appeal.failed");
    expect(appealFailureKey("sent")).toBeNull();
  });
});

describe("suspendedScreenActions", () => {
  it("offers Delete account exactly when this device holds none of the account's keys", () => {
    for (const appeal of [null, "none", "sent", "declined"] as const) {
      const withKeys = suspendedScreenActions(true, appeal);
      const without = suspendedScreenActions(false, appeal);
      expect(withKeys).toContain("read");
      expect(withKeys).not.toContain("delete");
      expect(without).toContain("delete");
      expect(without).not.toContain("read");
    }
  });

  it("orders Appeal, then Read or Delete, then Sign out, and drops Appeal once used", () => {
    expect(suspendedScreenActions(true, "none")).toEqual(["appeal", "read", "sign-out"]);
    expect(suspendedScreenActions(false, "none")).toEqual(["appeal", "delete", "sign-out"]);
    expect(suspendedScreenActions(false, "sent")).toEqual(["delete", "sign-out"]);
    expect(suspendedScreenActions(true, null)).toEqual(["read", "sign-out"]);
  });
});
