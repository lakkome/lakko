// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `PUT /auth/login-handle` answering `429 {"error":"handle-bind-limited",
 * "retry_after_secs":N}` (docs/ux/anti-abuse.md section 2, added
 * 2026-09-28):
 *
 *   - the copy is in minutes, rounded up, and "later" over an hour;
 *   - it reaches the signup form through `signupErrorMessage`, not as the
 *     generic attempts copy;
 *   - in the signup claim order it stops before discovery, like a 409, but
 *     keeps its own copy and releases nothing (nothing was bound);
 *   - the signup form keeps its contents on a failure.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";

import {
  authLimitMessage,
  handleBindLimitedMessage,
  isHandleBindLimited,
  signupErrorMessage,
} from "./auth-error-copy";
import { AuthHttpError } from "./client";
import { claimUsername, UsernameTakenError } from "./login-handle";

function limited(retryAfterSecs: number | null, header: number | null = null): AuthHttpError {
  const body: Record<string, unknown> = { error: "handle-bind-limited" };
  if (retryAfterSecs !== null) body.retry_after_secs = retryAfterSecs;
  return new AuthHttpError(429, "/auth/login-handle", JSON.stringify(body), header);
}

describe("handleBindLimitedMessage", () => {
  it("rounds the wait up to whole minutes", () => {
    expect(handleBindLimitedMessage(61)).toBe(
      "You have changed your username too often. Try again in 2 minutes.",
    );
    expect(handleBindLimitedMessage(120)).toBe(
      "You have changed your username too often. Try again in 2 minutes.",
    );
    expect(handleBindLimitedMessage(3599)).toBe(
      "You have changed your username too often. Try again in 60 minutes.",
    );
    expect(handleBindLimitedMessage(3600)).toBe(
      "You have changed your username too often. Try again in 60 minutes.",
    );
  });

  it("a wait under a minute reads as one minute, singular", () => {
    expect(handleBindLimitedMessage(1)).toBe(
      "You have changed your username too often. Try again in 1 minute.",
    );
    expect(handleBindLimitedMessage(0)).toBe(
      "You have changed your username too often. Try again in 1 minute.",
    );
  });

  it("over an hour, or with no wait given, says later", () => {
    expect(handleBindLimitedMessage(3601)).toBe(
      "You have changed your username too often. Try again later.",
    );
    expect(handleBindLimitedMessage(86_400)).toBe(
      "You have changed your username too often. Try again later.",
    );
    expect(handleBindLimitedMessage(null)).toBe(
      "You have changed your username too often. Try again later.",
    );
  });
});

describe("the 429 reaches the screens with its own copy", () => {
  it("from the body's retry_after_secs, else the Retry-After header", () => {
    expect(authLimitMessage(limited(150))).toBe(
      "You have changed your username too often. Try again in 3 minutes.",
    );
    expect(authLimitMessage(limited(null, 300))).toBe(
      "You have changed your username too often. Try again in 5 minutes.",
    );
    expect(isHandleBindLimited(limited(10))).toBe(true);
    expect(isHandleBindLimited(new AuthHttpError(429, "/x", JSON.stringify({ error: "too-many-attempts" })))).toBe(
      false,
    );
  });

  it("the signup form shows it, not the attempts or taken copy", () => {
    expect(signupErrorMessage(limited(600))).toBe(
      "You have changed your username too often. Try again in 10 minutes.",
    );
  });
});

describe("the signup claim order", () => {
  function steps() {
    const order: string[] = [];
    return {
      order,
      bind: vi.fn(async () => {
        order.push("bind");
        throw limited(240);
      }),
      register: vi.fn(async () => {
        order.push("register");
        return { normalisedUsername: "alice" };
      }),
      release: vi.fn(async () => {
        order.push("release");
      }),
    };
  }

  it("stops before discovery, like a 409, but keeps the limit copy", async () => {
    const s = steps();
    const err = await claimUsername({ username: "alice", ...s }).catch((e: unknown) => e);
    expect(err).not.toBeInstanceOf(UsernameTakenError);
    expect(isHandleBindLimited(err)).toBe(true);
    expect(signupErrorMessage(err)).toBe(
      "You have changed your username too often. Try again in 4 minutes.",
    );
    expect(s.order).toEqual(["bind"]);
    expect(s.register).not.toHaveBeenCalled();
    // Nothing was bound, so nothing is released.
    expect(s.release).not.toHaveBeenCalled();
  });

  it("the signup form clears its fields only on success, never in the failure path", () => {
    const src = readFileSync(
      fileURLToPath(new URL("../routes/Signup.tsx", import.meta.url)),
      "utf8",
    );
    const shown = src.indexOf("setError(signupErrorMessage(err))");
    const failure = src.slice(src.lastIndexOf("} catch (err) {", shown), src.indexOf("} finally {", shown));
    expect(failure).toContain("setError(signupErrorMessage(err))");
    expect(failure).not.toMatch(/set(Username|Password|Confirm)\(/);
  });
});
