// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the TOTP code-shape validator (ADR-0018 factor
 * (c)). Mirrors the acceptance set of the server's `parse_code`
 * (`services/auth/src/totp.rs::tests::parse_code_rejects_bad_shapes`)
 * so the client-side gate never accepts something the server would
 * refuse (which would burn a PoW solve for a guaranteed 400).
 */
import { describe, expect, it } from "vitest";
import { normalizeTotpCode, TOTP_CODE_DIGITS } from "./totp-code";

describe("normalizeTotpCode", () => {
  it("accepts exactly six ASCII digits", () => {
    expect(normalizeTotpCode("123456")).toBe("123456");
    expect(normalizeTotpCode("000000")).toBe("000000");
    expect(normalizeTotpCode("999999")).toBe("999999");
  });

  it("preserves leading zeros", () => {
    expect(normalizeTotpCode("000123")).toBe("000123");
  });

  it("trims surrounding whitespace (server parity)", () => {
    expect(normalizeTotpCode(" 123456 ")).toBe("123456");
    expect(normalizeTotpCode("\t123456\n")).toBe("123456");
  });

  it("rejects the wrong length", () => {
    expect(normalizeTotpCode("12345")).toBeNull(); // too short
    expect(normalizeTotpCode("1234567")).toBeNull(); // too long
    expect(normalizeTotpCode("")).toBeNull();
  });

  it("rejects non-digits", () => {
    expect(normalizeTotpCode("12345a")).toBeNull();
    expect(normalizeTotpCode("12 456")).toBeNull(); // inner whitespace
    expect(normalizeTotpCode("12345.")).toBeNull();
    expect(normalizeTotpCode("-12345")).toBeNull();
  });

  it("rejects non-ASCII numerals (server is_ascii_digit parity)", () => {
    // Arabic-Indic digits — `\d`-style regexes can be tricked by
    // these under the `u` flag with certain classes; the explicit
    // char-code loop must refuse them like the server does.
    expect(normalizeTotpCode("١٢٣٤٥٦")).toBeNull();
    // Full-width digits.
    expect(normalizeTotpCode("１２３４５６")).toBeNull();
  });

  it("digit count constant matches the server's DIGITS", () => {
    expect(TOTP_CODE_DIGITS).toBe(6);
  });
});
