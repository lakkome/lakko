// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Lakko is full for now" (`docs/ux/beta-account-cap.md`):
 *
 *   - `GET /signup/open` reads as open on every failure;
 *   - auth's `403 registration-closed` at the first bind becomes
 *     `RegistrationClosedError` and discovery is never asked;
 *   - the sign-up screen and the guest question decide by the pure rules.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { AuthClient, AuthHttpError, signupOpenFromBody } from "./client";
import {
  claimUsername,
  isRegistrationClosed,
  RegistrationClosedError,
  UsernameTakenError,
} from "./login-handle";
import {
  guestQuestionFor,
  SIGNUP_GATE_START,
  signupFailureView,
  signupGateMayRetry,
  signupGateReduce,
} from "./signup-closed";

function closedBind(): AuthHttpError {
  return new AuthHttpError(403, "/auth/login-handle", JSON.stringify({ error: "registration-closed" }));
}

describe("AuthClient.signupOpen", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function stubFetch(impl: () => Promise<Response>): ReturnType<typeof vi.fn> {
    const fn = vi.fn(impl);
    vi.stubGlobal("fetch", fn);
    return fn;
  }

  it("asks GET /signup/open on the auth base", async () => {
    const fn = stubFetch(async () => new Response(JSON.stringify({ open: true }), { status: 200 }));
    expect(await new AuthClient("/api/auth").signupOpen()).toBe(true);
    expect(fn).toHaveBeenCalledWith("/api/auth/signup/open", { method: "GET" });
  });

  it("is closed only on an explicit {open: false}", async () => {
    stubFetch(async () => new Response(JSON.stringify({ open: false }), { status: 200 }));
    expect(await new AuthClient().signupOpen()).toBe(false);
  });

  it("a network failure reads as open", async () => {
    stubFetch(async () => {
      throw new TypeError("Failed to fetch");
    });
    expect(await new AuthClient().signupOpen()).toBe(true);
  });

  it("an older server's 404 and any other non-2xx read as open", async () => {
    for (const status of [404, 429, 500, 503]) {
      stubFetch(async () => new Response(JSON.stringify({ open: false }), { status }));
      expect(await new AuthClient().signupOpen()).toBe(true);
    }
  });

  it("a body that is not JSON reads as open", async () => {
    stubFetch(async () => new Response("<html>", { status: 200 }));
    expect(await new AuthClient().signupOpen()).toBe(true);
  });

  it("a JSON body of another shape reads as open", () => {
    expect(signupOpenFromBody(null)).toBe(true);
    expect(signupOpenFromBody("false")).toBe(true);
    expect(signupOpenFromBody({})).toBe(true);
    expect(signupOpenFromBody({ open: "false" })).toBe(true);
    expect(signupOpenFromBody({ open: 0 })).toBe(true);
    expect(signupOpenFromBody({ open: false })).toBe(false);
  });
});

describe("registration-closed at the bind", () => {
  it("is recognised only as auth's 403 with that code", () => {
    expect(isRegistrationClosed(closedBind())).toBe(true);
    expect(isRegistrationClosed(new RegistrationClosedError())).toBe(true);
    expect(
      isRegistrationClosed(
        new AuthHttpError(403, "/auth/login-handle", JSON.stringify({ error: "fresh-auth-required" })),
      ),
    ).toBe(false);
    expect(isRegistrationClosed(new AuthHttpError(409, "/auth/login-handle", "handle-taken"))).toBe(false);
    expect(isRegistrationClosed(new AuthHttpError(403, "/auth/login-handle", "registration-closed"))).toBe(
      false,
    );
    expect(isRegistrationClosed(new Error("registration closed"))).toBe(false);
  });

  it("claimUsername throws RegistrationClosedError and asks discovery nothing", async () => {
    const register = vi.fn(async () => "registered");
    const release = vi.fn(async () => undefined);
    await expect(
      claimUsername({
        username: "alice",
        bind: async () => {
          throw closedBind();
        },
        register,
        release,
      }),
    ).rejects.toBeInstanceOf(RegistrationClosedError);
    expect(register).not.toHaveBeenCalled();
    // Nothing was bound, so nothing is released.
    expect(release).not.toHaveBeenCalled();
  });

  it("a taken handle is still UsernameTakenError, not closed", async () => {
    await expect(
      claimUsername({
        username: "alice",
        bind: async () => {
          throw new AuthHttpError(409, "/auth/login-handle", JSON.stringify({ error: "handle-taken" }));
        },
        register: async () => "x",
        release: async () => undefined,
      }),
    ).rejects.toBeInstanceOf(UsernameTakenError);
  });
});

describe("sign-up screen gate", () => {
  it("starts on the form and follows the answer", () => {
    expect(SIGNUP_GATE_START).toEqual({ view: "form" });
    const closed = signupGateReduce(SIGNUP_GATE_START, { type: "answer", open: false });
    expect(closed).toEqual({ view: "closed", checking: false });
    expect(signupGateReduce(SIGNUP_GATE_START, { type: "answer", open: true })).toEqual({ view: "form" });
  });

  it("Try again checks, then returns to the form or stays closed", () => {
    const closed = signupGateReduce(SIGNUP_GATE_START, { type: "answer", open: false });
    expect(signupGateMayRetry(closed)).toBe(true);
    const checking = signupGateReduce(closed, { type: "retry" });
    expect(checking).toEqual({ view: "closed", checking: true });
    // A second press while asking does nothing.
    expect(signupGateMayRetry(checking)).toBe(false);
    expect(signupGateReduce(checking, { type: "answer", open: true })).toEqual({ view: "form" });
    expect(signupGateReduce(checking, { type: "answer", open: false })).toEqual({
      view: "closed",
      checking: false,
    });
  });

  it("Try again has no meaning on the form", () => {
    expect(signupGateMayRetry(SIGNUP_GATE_START)).toBe(false);
    expect(signupGateReduce(SIGNUP_GATE_START, { type: "retry" })).toEqual({ view: "form" });
  });

  it("a refusal at the bind shows the full state", () => {
    expect(signupGateReduce(SIGNUP_GATE_START, { type: "refused" })).toEqual({
      view: "closed",
      checking: false,
    });
  });

  it("only the bind's refusal turns a failed sign-up into the full state", () => {
    expect(signupFailureView(new RegistrationClosedError())).toBe("closed");
    expect(signupFailureView(closedBind())).toBe("closed");
    expect(signupFailureView(new UsernameTakenError("alice"))).toBe("error");
    expect(signupFailureView(new Error("network"))).toBe("error");
  });
});

describe("guest question after a call", () => {
  it("closed when the server said so or the bind refused", () => {
    expect(guestQuestionFor({ signupOpen: true, refusedAtBind: false, reservedUsername: null })).toBe("ask");
    expect(guestQuestionFor({ signupOpen: false, refusedAtBind: false, reservedUsername: null })).toBe("closed");
    expect(guestQuestionFor({ signupOpen: true, refusedAtBind: true, reservedUsername: null })).toBe("closed");
    expect(guestQuestionFor({ signupOpen: false, refusedAtBind: true, reservedUsername: null })).toBe("closed");
  });

  it("a guest whose earlier Create account reserved a username is never shown the closed question", () => {
    for (const signupOpen of [true, false]) {
      for (const refusedAtBind of [true, false]) {
        expect(guestQuestionFor({ signupOpen, refusedAtBind, reservedUsername: "alice" })).toBe("ask");
      }
    }
    // An empty name is not a reservation.
    expect(guestQuestionFor({ signupOpen: false, refusedAtBind: false, reservedUsername: "" })).toBe("closed");
  });
});
