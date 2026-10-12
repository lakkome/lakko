// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The fresh-auth proof (`fresh-auth.ts`; ADR-0065 as amended 2026-10-06,
 * identity-recovery UX sections 17 and 15.1a): the wire read, the in-memory
 * holder, the shared block's decision, each surface's "what is shown", the
 * failure mapping and the password-change gate.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

/** `apps/web/src`. */
const SRC_ROOT = fileURLToPath(new URL("..", import.meta.url));
import { describe, expect, it } from "vitest";

import {
  createFreshGrantHolder,
  deleteAccountSteps,
  deleteSurfaceView,
  errorCodeOfBody,
  freshGrantOf,
  isFreshAuthRequired,
  passwordChangeAllowed,
  PHRASE_CHECK_TTL_MS,
  PROOF_COPY,
  proofBlockView,
  proofFailureLine,
  proofFailureOf,
  proofLoginFailure,
  resetCanSubmit,
  resetSurfaceView,
  type ProofBlockInput,
  type ProofErrorFacts,
} from "./fresh-auth";

describe("freshGrantOf", () => {
  it("reads the two fields of a minting route's finish", () => {
    expect(freshGrantOf({ fresh_grant: "g1.opaque", fresh_grant_expires_at_ms: 1_000 })).toEqual({
      grant: "g1.opaque",
      expiresAtMs: 1_000,
    });
  });

  it("answers null, never throws, for a response without a usable grant", () => {
    for (const wire of [
      null,
      undefined,
      {},
      { fresh_grant: "g" },
      { fresh_grant_expires_at_ms: 5 },
      { fresh_grant: "", fresh_grant_expires_at_ms: 5 },
      { fresh_grant: 7, fresh_grant_expires_at_ms: 5 },
      { fresh_grant: "g", fresh_grant_expires_at_ms: "5" },
      { fresh_grant: "g", fresh_grant_expires_at_ms: Number.NaN },
      { fresh_grant: "g", fresh_grant_expires_at_ms: 0 },
      // The retired field names are not read (flag day).
      { reset_grant: "g", reset_grant_expires_at_ms: 5 },
    ]) {
      expect(freshGrantOf(wire as never)).toBeNull();
    }
  });
});

describe("createFreshGrantHolder", () => {
  const grant = { grant: "g-a", expiresAtMs: 10_000 };

  it("holds nothing at first", () => {
    expect(createFreshGrantHolder().live("acct-a", 0)).toBeNull();
  });

  it("hands the grant out until its expiry, and not at or after it", () => {
    const holder = createFreshGrantHolder();
    holder.put("acct-a", grant);
    expect(holder.live("acct-a", 0)).toBe("g-a");
    expect(holder.live("acct-a", 9_999)).toBe("g-a");
    expect(holder.live("acct-a", 10_000)).toBeNull();
  });

  it("is reusable within its lifetime: reading it does not spend it", () => {
    const holder = createFreshGrantHolder();
    holder.put("acct-a", grant);
    for (let i = 0; i < 5; i += 1) expect(holder.live("acct-a", 100 + i)).toBe("g-a");
  });

  it("drops an expired grant for good, even if the clock steps back", () => {
    const holder = createFreshGrantHolder();
    holder.put("acct-a", grant);
    expect(holder.live("acct-a", 10_001)).toBeNull();
    expect(holder.live("acct-a", 0)).toBeNull();
  });

  it("is bound to the account it was issued for", () => {
    const holder = createFreshGrantHolder();
    holder.put("acct-a", grant);
    expect(holder.live("acct-b", 0)).toBeNull();
    // Asking for another account drops it: an account change holds nothing.
    expect(holder.live("acct-a", 0)).toBeNull();
  });

  it("a refused grant is dropped; a newer one put meanwhile is kept", () => {
    const holder = createFreshGrantHolder();
    holder.put("acct-a", grant);
    holder.spend("g-a");
    expect(holder.live("acct-a", 0)).toBeNull();
    holder.put("acct-a", { grant: "g-new", expiresAtMs: 10_000 });
    holder.spend("g-a");
    expect(holder.live("acct-a", 0)).toBe("g-new");
  });

  it("clear drops it (sign-out, account wipe, after a key reset)", () => {
    const holder = createFreshGrantHolder();
    holder.put("acct-a", grant);
    holder.clear();
    expect(holder.live("acct-a", 0)).toBeNull();
  });
});

describe("proofBlockView (UX 17.1)", () => {
  const base: ProofBlockInput = {
    proofLive: false,
    revealed: true,
    collapsed: false,
    passkeyUsable: false,
    passkeyFirst: false,
    passwordChosen: false,
    usernameKnown: true,
    password: "",
    username: "",
    phase: "idle",
    waiting: false,
  };

  it("with a live proof the block is not drawn and the surface goes straight on", () => {
    const v = proofBlockView({ ...base, proofLive: true, revealed: false });
    expect(v.visible).toBe(false);
    expect(v.route).toBe("none");
    expect(v.ready).toBe(true);
  });

  it("without a proof shows the field; the main button waits for a password", () => {
    const empty = proofBlockView(base);
    expect(empty).toMatchObject({
      visible: true,
      showPassword: true,
      showUsername: false,
      showUsePasskey: false,
      showUsePassword: false,
      showPasskeyNotice: false,
      route: "password",
      ready: false,
    });
    expect(proofBlockView({ ...base, password: "pw" }).ready).toBe(true);
  });

  it("asks for the username too when the device does not know it", () => {
    const v = proofBlockView({ ...base, usernameKnown: false, password: "pw" });
    expect(v.showUsername).toBe(true);
    expect(v.ready).toBe(false);
    expect(proofBlockView({ ...base, usernameKnown: false, password: "pw", username: " a " }).ready).toBe(true);
  });

  it("offers `Use a passkey instead` only when a passkey is usable: absent, not disabled", () => {
    expect(proofBlockView(base).showUsePasskey).toBe(false);
    const v = proofBlockView({ ...base, passkeyUsable: true });
    expect(v.showUsePasskey).toBe(true);
    expect(v.passkeyButtonDisabled).toBe(false);
  });

  it("a passkey-first surface shows the notice, no field, and its button opens the sheet", () => {
    const v = proofBlockView({ ...base, passkeyFirst: true, passkeyUsable: true });
    expect(v).toMatchObject({
      showPasskeyNotice: true,
      showPassword: false,
      showUsePasskey: false,
      showUsePassword: true,
      route: "passkey",
      ready: true,
    });
  });

  it("`Use your password instead` swaps the notice for the field", () => {
    const v = proofBlockView({
      ...base,
      passkeyFirst: true,
      passkeyUsable: true,
      passwordChosen: true,
    });
    expect(v).toMatchObject({ showPasskeyNotice: false, showPassword: true, route: "password", ready: false });
    expect(v.showUsePasskey).toBe(true);
  });

  it("a passkey-first surface without a usable passkey is the plain field", () => {
    const v = proofBlockView({ ...base, passkeyFirst: true, passkeyUsable: false });
    expect(v).toMatchObject({ showPasskeyNotice: false, showPassword: true, route: "password" });
  });

  it("checking and the passkey sheet disable the field and both proof buttons", () => {
    for (const phase of ["checking", "passkey"] as const) {
      const v = proofBlockView({ ...base, passkeyUsable: true, password: "pw", phase });
      expect(v.fieldDisabled).toBe(true);
      expect(v.passkeyButtonDisabled).toBe(true);
      expect(v.ready).toBe(false);
    }
  });

  it("a rate limit disables the field and the main button, but not the passkey button", () => {
    const v = proofBlockView({ ...base, passkeyUsable: true, password: "pw", waiting: true });
    expect(v.fieldDisabled).toBe(true);
    expect(v.ready).toBe(false);
    expect(v.passkeyButtonDisabled).toBe(false);
    // A passkey-led surface is not held back by the password's limit.
    expect(
      proofBlockView({ ...base, passkeyFirst: true, passkeyUsable: true, waiting: true }).ready,
    ).toBe(true);
  });

  it("keeps the block once revealed, until a proof completes on this surface", () => {
    // Revealed and a proof went live elsewhere: still drawn, nothing more needed.
    const v = proofBlockView({ ...base, proofLive: true, revealed: true });
    expect(v.visible).toBe(true);
    expect(v.route).toBe("none");
    expect(v.ready).toBe(true);
  });

  it("a collapsed surface draws nothing until its button is pressed, and the button is enabled", () => {
    const folded = proofBlockView({ ...base, collapsed: true, revealed: false });
    expect(folded.visible).toBe(false);
    expect(folded.ready).toBe(true);
    const opened = proofBlockView({ ...base, collapsed: true, revealed: true });
    expect(opened.visible).toBe(true);
    expect(opened.ready).toBe(false);
  });
});

describe("failure mapping", () => {
  const none: ProofErrorFacts = {
    required: false,
    wrongPassword: false,
    cancelled: false,
    otherAccount: false,
    passkeyFailed: false,
    status: null,
    retryAfterSecs: null,
    network: false,
  };

  it("isFreshAuthRequired is the 403 with the fresh-auth-required code and nothing else", () => {
    expect(isFreshAuthRequired({ status: 403, errorCode: "fresh-auth-required" })).toBe(true);
    expect(isFreshAuthRequired({ status: 403, errorCode: "password-required" })).toBe(false);
    expect(isFreshAuthRequired({ status: 403, errorCode: null })).toBe(false);
    expect(isFreshAuthRequired({ status: 401, errorCode: "fresh-auth-required" })).toBe(false);
    expect(isFreshAuthRequired({ status: null, errorCode: "fresh-auth-required" })).toBe(false);
  });

  it("errorCodeOfBody reads a JSON refusal leniently", () => {
    expect(errorCodeOfBody('{"error":"fresh-auth-required"}')).toBe("fresh-auth-required");
    expect(errorCodeOfBody("ActionRequired")).toBeNull();
    expect(errorCodeOfBody('{"error":5}')).toBeNull();
    expect(errorCodeOfBody("")).toBeNull();
  });

  it("proofLoginFailure reads a 401, a 404 and a local exchange failure as a wrong password", () => {
    expect(proofLoginFailure({ status: 401, network: false })).toBe("wrong-password");
    expect(proofLoginFailure({ status: 404, network: false })).toBe("wrong-password");
    expect(proofLoginFailure({ status: null, network: false })).toBe("wrong-password");
  });

  it("proofLoginFailure never reads a limit, an outage or the network as a wrong password", () => {
    expect(proofLoginFailure({ status: 429, network: false })).toBe("other");
    expect(proofLoginFailure({ status: 503, network: false })).toBe("other");
    expect(proofLoginFailure({ status: 500, network: false })).toBe("other");
    expect(proofLoginFailure({ status: null, network: true })).toBe("other");
  });

  it("classifies each row of the shared states table", () => {
    expect(proofFailureOf({ ...none, required: true }).kind).toBe("required");
    expect(proofFailureOf({ ...none, wrongPassword: true }).kind).toBe("wrong-password");
    expect(proofFailureOf({ ...none, cancelled: true }).kind).toBe("cancelled");
    expect(proofFailureOf({ ...none, otherAccount: true }).kind).toBe("other-account");
    expect(proofFailureOf({ ...none, passkeyFailed: true }).kind).toBe("passkey-failed");
    expect(proofFailureOf({ ...none, network: true }).kind).toBe("offline");
    expect(proofFailureOf({ ...none, status: 429, retryAfterSecs: 30 })).toEqual({
      kind: "rate-limited",
      retryAfterSecs: 30,
    });
    expect(proofFailureOf({ ...none, status: 429 })).toEqual({ kind: "rate-limited", retryAfterSecs: null });
    expect(proofFailureOf({ ...none, status: 429, retryAfterSecs: 0 })).toEqual({
      kind: "rate-limited",
      retryAfterSecs: null,
    });
    expect(proofFailureOf({ ...none, status: 500 }).kind).toBe("other");
    expect(proofFailureOf(none).kind).toBe("other");
  });

  it("a refused proof wins over every other reading of the same error", () => {
    expect(proofFailureOf({ ...none, required: true, status: 403 }).kind).toBe("required");
  });

  it("words each failure per surface, and a refused proof with no line at all", () => {
    const copy = PROOF_COPY.addDeviceRequest;
    expect(proofFailureLine({ kind: "wrong-password" }, copy)).toEqual({
      key: "proof.add_device.request.wrong_password",
      error: true,
    });
    // Cancelling is not an error.
    expect(proofFailureLine({ kind: "cancelled" }, copy)).toEqual({
      key: "proof.add_device.request.cancelled",
      error: false,
    });
    expect(proofFailureLine({ kind: "other-account" }, copy)?.key).toBe("proof.passkey.other_account");
    expect(proofFailureLine({ kind: "passkey-failed" }, copy)?.key).toBe("proof.passkey.failed");
    expect(proofFailureLine({ kind: "offline" }, copy)?.key).toBe("common.error.offline");
    expect(proofFailureLine({ kind: "rate-limited", retryAfterSecs: null }, copy)?.key).toBe(
      "common.error.too_many_attempts",
    );
    // With a retry time the countdown notice says it.
    expect(proofFailureLine({ kind: "rate-limited", retryAfterSecs: 9 }, copy)).toBeNull();
    expect(proofFailureLine({ kind: "required" }, copy)).toBeNull();
    expect(proofFailureLine({ kind: "other" }, copy)?.key).toBe("device_link.request.error.generic");
    expect(proofFailureLine({ kind: "wrong-password" }, PROOF_COPY.reset)?.key).toBe(
      "restore_identity.reset.error.wrong_password",
    );
    expect(proofFailureLine({ kind: "cancelled" }, PROOF_COPY.reset)?.key).toBe("proof.reset.cancelled");
    expect(proofFailureLine({ kind: "wrong-password" }, PROOF_COPY.deleteAccount)?.key).toBe(
      "proof.delete.wrong_password",
    );
    expect(proofFailureLine({ kind: "wrong-password" }, PROOF_COPY.addDeviceScan)?.key).toBe(
      "proof.add_device.scan.wrong_password",
    );
    expect(proofFailureLine({ kind: "cancelled" }, PROOF_COPY.addDeviceScan)?.key).toBe(
      "proof.add_device.scan.cancelled",
    );
  });

  it("add a passkey and change password have their own helper and cancelled line", () => {
    expect(PROOF_COPY.addPasskey.helpKey).toBe("proof.add_passkey.help");
    expect(proofFailureLine({ kind: "cancelled" }, PROOF_COPY.addPasskey)).toEqual({
      key: "proof.add_passkey.cancelled",
      error: false,
    });
    expect(PROOF_COPY.changePassword.helpKey).toBe("proof.change_password.help");
    expect(proofFailureLine({ kind: "cancelled" }, PROOF_COPY.changePassword)).toEqual({
      key: "proof.change_password.cancelled",
      error: false,
    });
    // A dismissed sheet on a surface without a passkey route has no line.
    expect(proofFailureLine({ kind: "cancelled" }, PROOF_COPY.deleteAccount)).toBeNull();
  });

  it("every key the block can show exists in the catalog", () => {
    const catalog = JSON.parse(
      readFileSync(fileURLToPath(new URL("../l10n/catalog/source.json", import.meta.url)), "utf8"),
    ) as { messages: Record<string, unknown> };
    const keys = new Set<string>([
      "auth.password.label",
      "proof.passkey.use",
      "proof.password.use",
      "proof.passkey.notice",
      "proof.passkey.other_account",
      "proof.passkey.failed",
      "proof.no_password_link",
      "proof.know_password_link",
      "proof.delete.cannot",
      "proof.change_password.use_words",
      "settings.account.delete.working",
      "settings.account.delete.error.cancelled",
      "recovery.passkey.waiting",
      "common.error.offline",
      "common.error.too_many_attempts",
      "recovery.error.rate_limited",
      "device_link.request.expired",
    ]);
    for (const copy of Object.values(PROOF_COPY)) {
      for (const key of [copy.helpKey, copy.wrongPasswordKey, copy.cancelledKey, copy.genericKey]) {
        if (key !== null) keys.add(key);
      }
    }
    for (const key of keys) expect(catalog.messages[key], key).toBeDefined();
  });
});

describe("resetSurfaceView (UX 17.2)", () => {
  const base = {
    proofLive: false,
    passkeyUsable: false,
    landing: "other" as const,
    knowsPassword: false,
    noPassword: false,
  };

  it("a live proof, or a usable passkey, is the form with no link", () => {
    expect(resetSurfaceView({ ...base, proofLive: true })).toEqual({ cannot: false, showNoPasswordLink: false });
    expect(resetSurfaceView({ ...base, passkeyUsable: true, landing: "authenticator" })).toEqual({
      cannot: false,
      showNoPasswordLink: false,
    });
  });

  it("no proof and no usable passkey: the block with `I do not know my password`", () => {
    expect(resetSurfaceView(base)).toEqual({ cannot: false, showNoPasswordLink: true });
    expect(resetSurfaceView({ ...base, landing: "password" })).toEqual({
      cannot: false,
      showNoPasswordLink: true,
    });
  });

  it("an authenticator sign-in without a passkey opens on the cannot-reset panel", () => {
    expect(resetSurfaceView({ ...base, landing: "authenticator" }).cannot).toBe(true);
    // `I know my password` switches to the block.
    expect(resetSurfaceView({ ...base, landing: "authenticator", knowsPassword: true })).toEqual({
      cannot: false,
      showNoPasswordLink: true,
    });
  });

  it("`I do not know my password` leads to the cannot-reset panel", () => {
    expect(resetSurfaceView({ ...base, noPassword: true }).cannot).toBe(true);
    // A proof that is live anyway is not undone by the link.
    expect(resetSurfaceView({ ...base, noPassword: true, proofLive: true }).cannot).toBe(false);
  });

  it("the reset button needs the word AND the proof side, and never shows on the cannot panel", () => {
    const ok = { confirmed: true, proofReady: true, cannot: false, blocked: false };
    expect(resetCanSubmit(ok)).toBe(true);
    expect(resetCanSubmit({ ...ok, confirmed: false })).toBe(false);
    expect(resetCanSubmit({ ...ok, proofReady: false })).toBe(false);
    expect(resetCanSubmit({ ...ok, cannot: true })).toBe(false);
    expect(resetCanSubmit({ ...ok, blocked: true })).toBe(false);
  });
});

describe("deleteSurfaceView (UX 17.3)", () => {
  const base = { hasPasskey: false, proofLive: false, revealed: false, noPassword: false };

  it("an account with a passkey is always asked for the gesture, proof or not", () => {
    expect(deleteSurfaceView({ ...base, hasPasskey: true }).kind).toBe("passkey");
    expect(deleteSurfaceView({ ...base, hasPasskey: true, proofLive: true }).kind).toBe("passkey");
  });

  it("no passkey and a live proof: the dialog as it was", () => {
    expect(deleteSurfaceView({ ...base, proofLive: true }).kind).toBe("plain");
  });

  it("no passkey and no proof: the block", () => {
    expect(deleteSurfaceView(base).kind).toBe("block");
    // A block already drawn stays drawn.
    expect(deleteSurfaceView({ ...base, proofLive: true, revealed: true }).kind).toBe("block");
  });

  it("`I do not know my password` replaces the field and the button", () => {
    expect(deleteSurfaceView({ ...base, noPassword: true }).kind).toBe("cannot");
  });
});

describe("deleteAccountSteps (UX 17.3)", () => {
  it("an account with a passkey runs one gesture and no sign-in before it", () => {
    expect(deleteAccountSteps("passkey")).toEqual({ ensureProof: false, actionGesture: true });
  });

  it("an account without a passkey proves the password and runs no gesture", () => {
    expect(deleteAccountSteps("plain")).toEqual({ ensureProof: true, actionGesture: false });
    expect(deleteAccountSteps("block")).toEqual({ ensureProof: true, actionGesture: false });
  });

  it("the cannot panel deletes nothing", () => {
    expect(deleteAccountSteps("cannot")).toBeNull();
  });
});

describe("passwordChangeAllowed (UX 17.8)", () => {
  it("is refused on the bare session", () => {
    expect(passwordChangeAllowed({ proofLive: false, phraseCheckedAtMs: null, nowMs: 1_000 })).toBe(false);
  });

  it("opens with a live proof", () => {
    expect(passwordChangeAllowed({ proofLive: true, phraseCheckedAtMs: null, nowMs: 1_000 })).toBe(true);
  });

  it("opens for ten minutes after a local phrase check, and not after", () => {
    const at = 50_000;
    expect(passwordChangeAllowed({ proofLive: false, phraseCheckedAtMs: at, nowMs: at })).toBe(true);
    expect(
      passwordChangeAllowed({ proofLive: false, phraseCheckedAtMs: at, nowMs: at + PHRASE_CHECK_TTL_MS - 1 }),
    ).toBe(true);
    expect(
      passwordChangeAllowed({ proofLive: false, phraseCheckedAtMs: at, nowMs: at + PHRASE_CHECK_TTL_MS }),
    ).toBe(false);
    // A check "in the future" (the clock stepped back) does not count.
    expect(passwordChangeAllowed({ proofLive: false, phraseCheckedAtMs: at, nowMs: at - 1 })).toBe(false);
  });
});

describe("the grant stays in memory and reaches every gated route", () => {
  const read = (rel: string): string =>
    readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

  it("the holder module touches no storage and no log", () => {
    const src = read("./fresh-auth.ts");
    for (const needle of ["localStorage", "sessionStorage", "indexedDB", "console.", "plainStorage"]) {
      expect(src.includes(needle)).toBe(false);
    }
  });

  it("the retired wire names are gone from the client", () => {
    for (const file of ["./client.ts", "./wire.ts", "./webauthn-client.ts", "./AuthContext.tsx"]) {
      const src = read(file);
      expect(src.includes("reset_grant"), file).toBe(false);
      expect(src.includes("password-required"), file).toBe(false);
    }
  });

  it("the reset sends the grant on both calls", () => {
    const client = read("./client.ts");
    const start = client.indexOf("async resetAccountKeys(");
    const body = client.slice(start, client.indexOf("\n  }\n", start));
    expect(body.match(/fresh_grant: freshGrant/g)?.length).toBe(2);
  });

  it("both add-device answers and the authorize start carry it", () => {
    const client = read("./client.ts");
    for (const fn of ["async deviceAuthorizeStart(", "async deviceRequestAnswer(", "async deviceRequestResult("]) {
      const start = client.indexOf(fn);
      expect(start, fn).toBeGreaterThan(0);
      const body = client.slice(start, client.indexOf("\n  }\n", start));
      expect(body.includes("fresh_grant: freshGrant"), fn).toBe(true);
    }
  });

  it("both passkey-registration calls carry it, and the account deletion sends the header", () => {
    const webauthn = read("./webauthn-client.ts");
    const start = webauthn.indexOf("async registerPasskey(");
    const body = webauthn.slice(start, webauthn.indexOf("\n  }\n", start));
    // The start posts `req` (which carries `fresh_grant`); the finish names it.
    expect(body.includes('"/webauthn/register/start",\n      req,')).toBe(true);
    expect(body.includes("fresh_grant: req.fresh_grant")).toBe(true);
    const del = webauthn.indexOf("async deleteAccount(");
    expect(webauthn.slice(del, webauthn.indexOf("\n  }\n", del)).includes("FRESH_GRANT_HEADER")).toBe(true);
  });

  it("the three minting routes feed the holder; the other landings do not", () => {
    const ctx = read("./AuthContext.tsx");
    expect(ctx.match(/freshGrants\.put\(/g)?.length).toBe(6);
    // login, signup, passkey sign-in, the two proof routes, and the recall
    // (ADR-0066: `recall/finish` answers a grant; the old password proved it).
    for (const landing of ["async recoverWithTotp(", "async recoverWithThreeWords("]) {
      const start = ctx.indexOf(landing);
      const end = ctx.indexOf("\n    async ", start + 10);
      expect(ctx.slice(start, end).includes("freshGrants.put("), landing).toBe(false);
    }
  });

  it("the password replace refuses the bare session", () => {
    const ctx = read("./AuthContext.tsx");
    const start = ctx.indexOf("async replacePassword(");
    const end = ctx.indexOf("\n    async ", start + 10);
    const body = ctx.slice(start, end);
    // The one exception is the "It was not me" screen of an account notice
    // (ADR-0067, UX 19.6), which asks nothing but the new password.
    expect(
      body.includes("if (options?.afterAccountNotice !== true && !api.passwordChangeAllowed()) {"),
    ).toBe(true);
    expect(body.includes("throw new FreshAuthRequiredError();")).toBe(true);
  });

  it("only the account notice's screen replaces the password without the gate", () => {
    const users: string[] = [];
    const walk = (dir: string): void => {
      for (const name of readdirSync(dir)) {
        const p = join(dir, name);
        if (statSync(p).isDirectory()) walk(p);
        else if (/\.tsx?$/.test(name) && !/\.test\./.test(name)) {
          if (readFileSync(p, "utf8").includes("afterAccountNotice")) users.push(relative(SRC_ROOT, p));
        }
      }
    };
    walk(SRC_ROOT);
    expect(users.sort()).toEqual([
      "auth/AuthContext.tsx",
      "auth/NewPasswordForm.tsx",
      "auth/protect/AccountNotices.tsx",
    ]);
  });

  it("the owed-publish retry asks for no grant", () => {
    const ctx = read("./AuthContext.tsx");
    const start = ctx.indexOf("async function retryKeyResetPublish(");
    expect(start).toBeGreaterThan(0);
    const end = ctx.indexOf("\n  }\n", start);
    expect(ctx.slice(start, end).includes("freshGrants")).toBe(false);
  });
});
