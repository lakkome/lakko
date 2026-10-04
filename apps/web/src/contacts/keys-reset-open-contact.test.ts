// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { base64Decode, base64UrlEncodeNoPad } from "../auth/account-id";
import { paths } from "../routes/paths";
import { KeysResetUnverified } from "./client";
import {
  keysResetOpenContactHref,
  keysResetRefusalAccount,
  openContactFor,
} from "./keys-reset-open-contact";
import { accountIdB64OfHex } from "./local-store";

const HEX = "000102030405060708090a0b0c0d0e0f";
const ACCOUNT_B64 = accountIdB64OfHex(HEX) ?? "";
const KEY_B64 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const ROWS = [
  { accountIdHex: "ffffffffffffffffffffffffffffffff", signaturePubkeyB64: "BBBB" },
  { accountIdHex: HEX, signaturePubkeyB64: KEY_B64 },
  { signaturePubkeyB64: "CCCC" },
];

describe("Open contact beside the keys-reset refusal (UX 16.1)", () => {
  it("names the account of a refusal, also through a cause chain", () => {
    const refusal = new KeysResetUnverified(ACCOUNT_B64);
    expect(keysResetRefusalAccount(refusal)).toBe(ACCOUNT_B64);
    expect(keysResetRefusalAccount(new Error("wrapped", { cause: refusal }))).toBe(ACCOUNT_B64);
    expect(keysResetRefusalAccount({ variant: { cause: refusal } })).toBe(ACCOUNT_B64);
  });

  it("is nothing for any other failure", () => {
    expect(keysResetRefusalAccount(new Error("offline"))).toBeNull();
    expect(keysResetRefusalAccount("text")).toBeNull();
    expect(keysResetRefusalAccount(null)).toBeNull();
    expect(keysResetOpenContactHref(new Error("offline"), ROWS)).toBeNull();
  });

  it("opens the contact screen of the row holding that account id", () => {
    expect(keysResetOpenContactHref(new KeysResetUnverified(ACCOUNT_B64), ROWS)).toBe(
      paths.contact(base64UrlEncodeNoPad(base64Decode(KEY_B64))),
    );
  });

  it("offers nothing when the person is not a contact here", () => {
    expect(keysResetOpenContactHref(new KeysResetUnverified(ACCOUNT_B64), [])).toBeNull();
    expect(
      keysResetOpenContactHref(new KeysResetUnverified(ACCOUNT_B64), [ROWS[0]!, ROWS[2]!]),
    ).toBeNull();
  });

  it("the action belongs to its own line only", () => {
    const kept = { text: () => "Verify Anna's new safety number first.", href: "/contacts/x" };
    expect(openContactFor("Verify Anna's new safety number first.", kept)).toBe("/contacts/x");
    expect(openContactFor("Could not start the chat.", kept)).toBeNull();
    expect(openContactFor(null, kept)).toBeNull();
    expect(openContactFor("Verify Anna's new safety number first.", null)).toBeNull();
  });
});
