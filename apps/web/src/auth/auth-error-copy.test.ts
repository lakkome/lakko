// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  authLimitMessage,
  deviceRestoreErrorMessage,
  identityRestoreErrorMessage,
  isSignupRateLimited,
  loginErrorMessage,
  passkeyErrorMessage,
  recoverErrorMessage,
  screenText,
  signupErrorMessage,
  tooManyAttempts,
  tooManyAttemptsMessage,
  wrongCredentials,
} from "./auth-error-copy";
import { totpRecoverErrorMessage } from "./recover/TotpRecover";
import { wordVaultErrorMessage } from "../recovery/word-vault";
import { AuthHttpError, AuthNetworkError } from "./client";
import { DiscoveryHttpError, DiscoveryNetworkError } from "../discovery/client";

const BANNED = /OPAQUE|PAKE|OPRF|MLS|KeyPackage|epoch|pseudonym|→|\/auth\//i;

describe("loginErrorMessage", () => {
  it("collapses an unknown username and a wrong password into one sentence", () => {
    // `AuthContext.login` throws "invalid credentials" on a lookup miss and
    // the server answers 401 on a wrong password; the user must not be able
    // to tell them apart.
    expect(loginErrorMessage(new Error("invalid credentials"))).toBe(wrongCredentials());
    expect(loginErrorMessage(new AuthHttpError(401, "/login/finish", "unauthorized"))).toBe(
      wrongCredentials(),
    );
    expect(loginErrorMessage(new DiscoveryHttpError(404, "/lookup", ""))).toBe(wrongCredentials());
  });

  it("names the network, never the endpoint", () => {
    const m = loginErrorMessage(new AuthNetworkError("/login/start", new TypeError("fetch")));
    expect(m).toMatch(/connection/);
    expect(m).not.toMatch(BANNED);
    expect(loginErrorMessage(new DiscoveryNetworkError("/lookup", "x"))).toBe(m);
  });

  it("never shows a raw diagnostic", () => {
    const m = loginErrorMessage(new AuthHttpError(500, "/login/finish", "boom"));
    expect(m).not.toMatch(BANNED);
    expect(m.endsWith(".")).toBe(true);
    expect(loginErrorMessage("garbage")).toBe(wrongCredentials());
  });

  it("passes through a sentence the context already wrote for the user", () => {
    expect(loginErrorMessage(new Error("Enter a username."))).toBe("Enter a username.");
  });
});

describe("passkeyErrorMessage", () => {
  it("keeps the context's own sentences and replaces a diagnostic", () => {
    expect(passkeyErrorMessage(new Error("Passkey sign-in cancelled."))).toBe(
      "Passkey sign-in cancelled.",
    );
    expect(passkeyErrorMessage(new Error("auth /x → 500: y"))).toMatch(/passkey/i);
  });
});

describe("signupErrorMessage", () => {
  it("says a taken username is taken, from either shape the context throws", () => {
    expect(signupErrorMessage(new Error("username @bob is already taken — pick another"))).toBe(
      "That username is taken. Choose another one.",
    );
    expect(signupErrorMessage(new DiscoveryHttpError(409, "/register", ""))).toBe(
      "That username is taken. Choose another one.",
    );
  });
});

describe("recoverErrorMessage", () => {
  it("collapses a wrong phrase and an unknown username into one sentence", () => {
    const a = recoverErrorMessage(new Error("username not found"));
    const b = recoverErrorMessage(new AuthHttpError(401, "/password/reset/finish", ""));
    expect(a).toBe(b);
    expect(a).not.toMatch(BANNED);
  });

  it("maps a lockout to a wait", () => {
    expect(recoverErrorMessage(new AuthHttpError(429, "/x", ""))).toMatch(/Wait/);
  });
});

describe("DiscoveryNetworkError constructor shape", () => {
  it("is constructible the way the tests above assume", () => {
    // Guards the assumption, so a signature change fails here and not silently.
    expect(new DiscoveryNetworkError("/lookup", "x")).toBeInstanceOf(Error);
  });
});

describe("ADR-0054 S4 limits (docs/ux/anti-abuse.md section 2)", () => {
  function limited(code: string, retryAfterSecs: number | null, headerSecs: number | null = null): AuthHttpError {
    const body =
      retryAfterSecs === null
        ? JSON.stringify({ error: code })
        : JSON.stringify({ error: code, retry_after_secs: retryAfterSecs });
    return new AuthHttpError(429, "/login/start", body, headerSecs);
  }

  it("too-many-attempts: a moment, or minutes when the server asks for more than one", () => {
    expect(authLimitMessage(limited("too-many-attempts", 30))).toBe(
      "Too many attempts. Wait a moment, then try again.",
    );
    expect(authLimitMessage(limited("too-many-attempts", 60))).toBe(
      "Too many attempts. Wait a moment, then try again.",
    );
    expect(authLimitMessage(limited("too-many-attempts", 61))).toBe(
      "Too many attempts. Try again in 2 minutes.",
    );
    expect(authLimitMessage(limited("too-many-attempts", 900))).toBe(
      "Too many attempts. Try again in 15 minutes.",
    );
    // The header alone, when the body has no number.
    expect(authLimitMessage(limited("too-many-attempts", null, 300))).toBe(
      "Too many attempts. Try again in 5 minutes.",
    );
    expect(tooManyAttemptsMessage(null)).toBe(tooManyAttempts());
  });

  it("device-request-limited", () => {
    expect(authLimitMessage(limited("device-request-limited", 600))).toBe(
      "Too many requests to your other devices. Try again in a few minutes.",
    );
  });

  it("signup-rate-limited, with Try again on the form", () => {
    const err = limited("signup-rate-limited", 60);
    expect(authLimitMessage(err)).toBe(
      "Too many new accounts from this network right now. Try again later.",
    );
    expect(signupErrorMessage(err)).toBe(
      "Too many new accounts from this network right now. Try again later.",
    );
    expect(isSignupRateLimited(err)).toBe(true);
    expect(isSignupRateLimited(limited("too-many-attempts", 1))).toBe(false);
  });

  it("503 rate-limiter-unavailable is the generic failure, not a limit", () => {
    const err = new AuthHttpError(503, "/totp/recover", JSON.stringify({ error: "rate-limiter-unavailable" }));
    expect(authLimitMessage(err)).toBe("That is not available right now. Try again later.");
  });

  it("every screen routes the codes through the one mapping", () => {
    const err = limited("too-many-attempts", 120);
    const expected = "Too many attempts. Try again in 2 minutes.";
    expect(loginErrorMessage(err)).toBe(expected);
    expect(recoverErrorMessage(err)).toBe(expected);
    expect(totpRecoverErrorMessage(err)).toBe(expected);
    expect(wordVaultErrorMessage(err, 3)).toBe(expected);
  });

  it("a 429 is never a sign-in failure: no copy mentions the password or a sign-out", () => {
    for (const code of [
      "too-many-attempts",
      "device-request-limited",
      "signup-rate-limited",
      "something-new",
    ]) {
      const m = authLimitMessage(limited(code, 10)) ?? "";
      expect(m).not.toMatch(/password|sign in|signed out|invalid/i);
      expect(m).not.toBe(wrongCredentials());
    }
  });

  it("a non-limit error is not claimed", () => {
    expect(authLimitMessage(new AuthHttpError(401, "/x", "{}"))).toBeNull();
    expect(authLimitMessage(new Error("x"))).toBeNull();
  });
});

describe("restore identity checks render from their kind (ADR-0061 section 6)", () => {
  // Shaped like `AuthContext`'s classes (matched on `name`), with a `message`
  // that is NOT the screen sentence, so a pass-through of the English would
  // fail these assertions.
  const mismatch = (source?: "phrase" | "device"): Error =>
    Object.assign(new Error("Diagnostic for the logs."), {
      name: "SupersessionPhraseMismatch",
      ...(source === undefined ? {} : { source }),
    });
  const unavailable = (kind: "missing" | "unreachable"): Error =>
    Object.assign(new Error("Diagnostic for the logs."), {
      name: "IdentityVerificationUnavailable",
      kind,
    });
  const PHRASE = "That recovery phrase doesn't match this account's identity. Check the phrase, or pair an existing device instead.";
  const DEVICE =
    "The identity received from your other device doesn't match this account's published identity — refusing to use it.";

  it("follows the source the mismatch carries, whichever form shows it", () => {
    expect(screenText(mismatch("device"))).toBe(DEVICE);
    expect(screenText(mismatch("phrase"))).toBe(PHRASE);
    expect(identityRestoreErrorMessage(mismatch("device"))).toBe(DEVICE);
    expect(deviceRestoreErrorMessage(mismatch("phrase"))).toBe(PHRASE);
    // The three-word form reaches the same check through screenText.
    expect(wordVaultErrorMessage(mismatch("phrase"), null)).toBe(PHRASE);
    expect(recoverErrorMessage(mismatch("device"))).toBe(DEVICE);
  });

  it("falls back to the form's own source when the error carries none", () => {
    expect(identityRestoreErrorMessage(mismatch())).toBe(PHRASE);
    expect(deviceRestoreErrorMessage(mismatch())).toBe(DEVICE);
  });

  it("says why the identity could not be checked", () => {
    expect(screenText(unavailable("missing"))).toBe(
      "No published identity was found to verify the recovered identity. Try again, or restore from a device that still has your account.",
    );
    expect(wordVaultErrorMessage(unavailable("unreachable"), null)).toBe(
      "Could not reach the directory to verify the recovered identity. Check your connection and try again.",
    );
  });
});
