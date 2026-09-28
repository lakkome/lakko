// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0054 decision 1 — the `x-lakko-wake` header value. The DS answers 400
 * to a malformed header, which fails the publish itself, so the one helper
 * that builds it is pinned here: a 32-byte `push_gid` as 43 base64url chars
 * without padding, a `;`, and `message` or `call`.
 */
import { describe, expect, it } from "vitest";

import { DS_WAKE_HEADER, DsWakeArgError, dsWakeHeaderValue, type DsWakeKind } from "./wake";

describe("dsWakeHeaderValue", () => {
  it("is named `x-lakko-wake`", () => {
    expect(DS_WAKE_HEADER).toBe("x-lakko-wake");
  });

  it("encodes the push_gid as 43 base64url chars with no padding", () => {
    // 0xfb 0xff bytes exercise both substitutions (`+` -> `-`, `/` -> `_`).
    const pushGid = new Uint8Array(32);
    for (let i = 0; i < 32; i += 1) pushGid[i] = i % 2 === 0 ? 0xfb : 0xff;
    const value = dsWakeHeaderValue({ pushGid, kind: "message" });
    const [id, kind] = value.split(";");
    expect(id).toHaveLength(43);
    expect(id).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(id).not.toContain("=");
    expect(id).toContain("-");
    expect(id).toContain("_");
    expect(kind).toBe("message");
  });

  it("pins a known vector", () => {
    const pushGid = Uint8Array.from({ length: 32 }, (_, i) => i);
    expect(dsWakeHeaderValue({ pushGid, kind: "call" })).toBe(
      "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8;call",
    );
  });

  it("carries both kinds verbatim", () => {
    const pushGid = new Uint8Array(32);
    for (const kind of ["message", "call"] as const) {
      expect(dsWakeHeaderValue({ pushGid, kind }).endsWith(`;${kind}`)).toBe(true);
    }
  });

  it("refuses a push_gid that is not 32 bytes", () => {
    for (const len of [0, 16, 31, 33, 64]) {
      expect(() => dsWakeHeaderValue({ pushGid: new Uint8Array(len), kind: "message" })).toThrow(
        DsWakeArgError,
      );
    }
  });

  it("refuses a kind the DS does not accept", () => {
    expect(() =>
      dsWakeHeaderValue({ pushGid: new Uint8Array(32), kind: "typing" as unknown as DsWakeKind }),
    ).toThrow(DsWakeArgError);
  });
});
