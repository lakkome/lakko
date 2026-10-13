// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a 401 means (`session-401.ts`, ADR-0073 sections 3.2-3.5): the one
 * code that signs a device out and wipes it, the one that sends it to the
 * sign-in screen, and the plain 401 that costs one refresh and one retry.
 */
import { describe, expect, it } from "vitest";

import {
  bearerRefusal,
  CODE_DEVICE_REVOKED,
  CODE_KEY_GENERATION_SUPERSEDED,
  CODE_REFRESH_INVALID,
  errorCodeOf,
  refreshRefusal,
} from "./session-401";

describe("errorCodeOf", () => {
  it("reads the error code of a JSON body and nothing else", () => {
    expect(errorCodeOf('{"error":"key-generation-superseded"}')).toBe("key-generation-superseded");
    expect(errorCodeOf('{"error":"refresh-invalid","retry_after_secs":3}')).toBe("refresh-invalid");
    expect(errorCodeOf('{"error":5}')).toBeNull();
    expect(errorCodeOf("")).toBeNull();
    expect(errorCodeOf(null)).toBeNull();
    expect(errorCodeOf("not json")).toBeNull();
  });
});

describe("bearerRefusal", () => {
  const base = { status: 401, errorCode: null, retried: false, refreshAvailable: true };

  it("passes anything that is not a 401", () => {
    for (const status of [200, 204, 400, 403, 404, 409, 429, 500, 503]) {
      expect(bearerRefusal({ ...base, status })).toBe("pass");
    }
  });

  it("signs out on the superseded code, whatever else is true", () => {
    for (const retried of [false, true]) {
      for (const refreshAvailable of [false, true]) {
        expect(
          bearerRefusal({
            status: 401,
            errorCode: CODE_KEY_GENERATION_SUPERSEDED,
            retried,
            refreshAvailable,
          }),
        ).toBe("superseded");
      }
    }
  });

  it("refreshes and retries once on a plain 401 while a credential is held", () => {
    expect(bearerRefusal(base)).toBe("refresh-and-retry");
    // A body with some other code (a wrong TOTP code, a device request
    // that is not ready) is still a 401 of a bearer route: one refresh.
    expect(bearerRefusal({ ...base, errorCode: "something-else" })).toBe("refresh-and-retry");
  });

  it("reports the second 401, and any 401 with nothing to refresh with", () => {
    expect(bearerRefusal({ ...base, retried: true })).toBe("report");
    expect(bearerRefusal({ ...base, refreshAvailable: false })).toBe("report");
    expect(bearerRefusal({ ...base, retried: true, refreshAvailable: false })).toBe("report");
  });
});

describe("refreshRefusal", () => {
  it("is superseded only on the one code; everything else is invalid and wipes nothing", () => {
    expect(refreshRefusal(CODE_KEY_GENERATION_SUPERSEDED)).toBe("superseded");
    expect(refreshRefusal(CODE_REFRESH_INVALID)).toBe("invalid");
    expect(refreshRefusal(null)).toBe("invalid");
    expect(refreshRefusal("unknown-code")).toBe("invalid");
  });
});

// ADR-0074 section 0.4: the removed device, against a logout and a reset.
describe("the two wiping codes do not depend on an order", () => {
  // ADR-0074 "T3 as built", item 1: the server reads the generation before
  // the device floor. Each code alone decides; the staged wipe and the
  // recall record follow `superseded`, the full wipe `device-revoked`.
  it("maps each code to its own refusal on both routes", () => {
    const base = { status: 401, retried: false, refreshAvailable: true };
    expect(bearerRefusal({ ...base, errorCode: CODE_KEY_GENERATION_SUPERSEDED })).toBe("superseded");
    expect(bearerRefusal({ ...base, errorCode: CODE_DEVICE_REVOKED })).toBe("device-revoked");
    expect(refreshRefusal(CODE_KEY_GENERATION_SUPERSEDED)).toBe("superseded");
    expect(refreshRefusal(CODE_DEVICE_REVOKED)).toBe("device-revoked");
  });
});

describe("device-revoked", () => {
  const base = { status: 401, errorCode: CODE_DEVICE_REVOKED, retried: false, refreshAvailable: true };

  it("signs a removed device out from any bearer route, whatever else is true", () => {
    for (const retried of [false, true]) {
      for (const refreshAvailable of [false, true]) {
        expect(bearerRefusal({ ...base, retried, refreshAvailable })).toBe("device-revoked");
      }
    }
  });

  it("is told apart from a reset and from an expired token", () => {
    expect(bearerRefusal({ ...base, errorCode: CODE_KEY_GENERATION_SUPERSEDED })).toBe("superseded");
    expect(bearerRefusal({ ...base, errorCode: null })).toBe("refresh-and-retry");
    expect(bearerRefusal({ ...base, errorCode: CODE_REFRESH_INVALID })).toBe("refresh-and-retry");
  });

  it("is only ever a 401: discovery's 403 device-revoked is not proof of removal", () => {
    expect(bearerRefusal({ ...base, status: 403 })).toBe("pass");
  });

  it("wipes on the refresh route's device-revoked and on nothing a logout answers", () => {
    expect(refreshRefusal(CODE_DEVICE_REVOKED)).toBe("device-revoked");
    // A logout answers the refresh with `refresh-invalid`: the sign-in
    // screen, nothing wiped.
    expect(refreshRefusal(CODE_REFRESH_INVALID)).toBe("invalid");
    expect(refreshRefusal(CODE_KEY_GENERATION_SUPERSEDED)).toBe("superseded");
    expect(refreshRefusal(null)).toBe("invalid");
    expect(refreshRefusal("something-else")).toBe("invalid");
  });
});
