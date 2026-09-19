// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { base64Encode } from "../auth/account-id";
import { isOwnKeyB64 } from "./own-key";

const device = new Uint8Array(32).fill(1);
const account = new Uint8Array(32).fill(2);
const other = new Uint8Array(32).fill(3);

describe("isOwnKeyB64", () => {
  it("matches the device signature key", () => {
    expect(isOwnKeyB64({ signaturePublicKey: device }, base64Encode(device))).toBe(true);
  });

  it("matches the account key a roster names the person by (ADR-0050)", () => {
    const own = { signaturePublicKey: device, accountPublicKey: account };
    expect(isOwnKeyB64(own, base64Encode(account))).toBe(true);
    expect(isOwnKeyB64(own, base64Encode(device))).toBe(true);
  });

  it("rejects anyone else, and a missing account key never matches", () => {
    expect(isOwnKeyB64({ signaturePublicKey: device, accountPublicKey: account }, base64Encode(other))).toBe(false);
    expect(isOwnKeyB64({ signaturePublicKey: device, accountPublicKey: null }, base64Encode(account))).toBe(false);
  });
});
