// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { AuthHttpError, AuthNetworkError } from "../client";
import {
  recoveryLoginFailureOf,
  storedPhraseVerdict,
  type RecoveryLoginFailure,
} from "./stale-phrase";

const ALL: RecoveryLoginFailure[] = [
  "invalid-credentials",
  "rate-limited",
  "unavailable",
  "network",
  "other",
];

describe("what a refused recovery sign-in says about the stored phrase", () => {
  it("suspends three words on invalid credentials", () => {
    expect(storedPhraseVerdict({ failure: "invalid-credentials" })).toBe("suspend-three-words");
  });

  it("never suspends on a limit, an outage, a network error or anything else", () => {
    for (const failure of ALL.filter((f) => f !== "invalid-credentials")) {
      expect(storedPhraseVerdict({ failure })).toBe("keep");
    }
  });
});

describe("classifying the failure", () => {
  const http = (status: number): AuthHttpError =>
    new AuthHttpError(status, "/recovery/login/finish", "{}");

  it("reads 401 as invalid credentials", () => {
    expect(recoveryLoginFailureOf(http(401))).toBe("invalid-credentials");
  });

  it("reads 429 and 503 as saying nothing about the credential", () => {
    expect(recoveryLoginFailureOf(http(429))).toBe("rate-limited");
    expect(recoveryLoginFailureOf(http(503))).toBe("unavailable");
  });

  it("reads a network failure as network", () => {
    expect(recoveryLoginFailureOf(new AuthNetworkError("/recovery/login/start", "down"))).toBe(
      "network",
    );
  });

  it("reads every other status and error as other", () => {
    for (const status of [200, 400, 404, 409, 500, 502]) {
      expect(recoveryLoginFailureOf(http(status))).toBe("other");
    }
    expect(recoveryLoginFailureOf(new Error("x"))).toBe("other");
    expect(recoveryLoginFailureOf(null)).toBe("other");
  });
});
