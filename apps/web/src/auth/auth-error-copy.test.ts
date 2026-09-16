// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  loginErrorMessage,
  passkeyErrorMessage,
  recoverErrorMessage,
  signupErrorMessage,
  WRONG_CREDENTIALS,
} from "./auth-error-copy";
import { AuthHttpError, AuthNetworkError } from "./client";
import { DiscoveryHttpError, DiscoveryNetworkError } from "../discovery/client";

const BANNED = /OPAQUE|PAKE|OPRF|MLS|KeyPackage|epoch|pseudonym|→|\/auth\//i;

describe("loginErrorMessage", () => {
  it("collapses an unknown username and a wrong password into one sentence", () => {
    // `AuthContext.login` throws "invalid credentials" on a lookup miss and
    // the server answers 401 on a wrong password; the user must not be able
    // to tell them apart.
    expect(loginErrorMessage(new Error("invalid credentials"))).toBe(WRONG_CREDENTIALS);
    expect(loginErrorMessage(new AuthHttpError(401, "/login/finish", "unauthorized"))).toBe(
      WRONG_CREDENTIALS,
    );
    expect(loginErrorMessage(new DiscoveryHttpError(404, "/lookup", ""))).toBe(WRONG_CREDENTIALS);
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
    expect(loginErrorMessage("garbage")).toBe(WRONG_CREDENTIALS);
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
