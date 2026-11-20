// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { base64Encode, base64UrlEncodeNoPad } from "./account-id";
import { accountIdFromWire, accountIdToWire } from "./webauthn-client";

// An id whose encodings differ in every way that matters: it carries the
// characters the two alphabets disagree on, and 16 bytes pad to "==".
const ID = new Uint8Array([0xfb, 0xff, 0xbf, 0x3e, 0x3f, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0xfe, 0xff]);
const STANDARD = base64Encode(ID);
const WIRE = base64UrlEncodeNoPad(ID);

describe("passkey sign-in account id", () => {
  it("the fixture differs between the two forms", () => {
    expect(STANDARD).toMatch(/[+/]/);
    expect(STANDARD.endsWith("==")).toBe(true);
    expect(WIRE).toMatch(/[-_]/);
  });

  it("the server's form reads back as the client's form", () => {
    expect(accountIdFromWire(WIRE)).toBe(STANDARD);
  });

  it("the client's form goes to the server in the server's form", () => {
    expect(accountIdToWire(STANDARD)).toBe(WIRE);
    expect(accountIdToWire(WIRE)).toBe(WIRE);
  });
});
