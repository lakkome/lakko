// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a 401 means (`session-401.ts`, ADR-0065 sections 3.2-3.5): the one
 * code that signs a device out and wipes it, the one that sends it to the
 * sign-in screen, and the plain 401 that costs one refresh and one retry.
 */
import { describe, expect, it } from "vitest";

import {
  bearerRefusal,
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
