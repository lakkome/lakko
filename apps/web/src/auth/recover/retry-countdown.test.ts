// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { AuthHttpError } from "../client";
import { attemptsRetryAfterSecs } from "./retry-countdown";

const limited = (body: string, header: number | null = null): AuthHttpError =>
  new AuthHttpError(429, "/totp/recover", body, header);

describe("the wait a server limit asks for", () => {
  it("is the body's retry time for an attempts limit", () => {
    expect(attemptsRetryAfterSecs(limited('{"error":"too-many-attempts","retry_after_secs":899}'))).toBe(899);
  });

  it("falls back to the Retry-After header", () => {
    expect(attemptsRetryAfterSecs(limited("too many", 30))).toBe(30);
  });

  it("is absent without a retry time", () => {
    expect(attemptsRetryAfterSecs(limited('{"error":"too-many-attempts"}'))).toBeNull();
    expect(attemptsRetryAfterSecs(limited('{"error":"too-many-attempts","retry_after_secs":0}'))).toBeNull();
  });

  it("leaves the limits that have their own copy alone", () => {
    for (const code of ["device-request-limited", "signup-rate-limited", "handle-bind-limited"]) {
      expect(attemptsRetryAfterSecs(limited(`{"error":"${code}","retry_after_secs":60}`))).toBeNull();
    }
  });

  it("is absent for anything that is not a 429", () => {
    expect(attemptsRetryAfterSecs(new AuthHttpError(401, "/totp/recover", "", null))).toBeNull();
    expect(attemptsRetryAfterSecs(new AuthHttpError(503, "/totp/recover", '{"error":"rate-limiter-unavailable"}', null))).toBeNull();
    expect(attemptsRetryAfterSecs(new Error("x"))).toBeNull();
  });
});
