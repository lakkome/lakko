// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { contactRequestGate, refusalIsRecordedReset } from "./keys-reset-send-gate";

const KEY_A = "QUFBQQ==";
const KEY_B = "QkJCQg==";

describe("contactRequestGate", () => {
  it("lets a request to a stranger through: no pin, first-ever trust", () => {
    expect(contactRequestGate(undefined, KEY_A)).toBe("send");
  });

  it("lets a request to a contact under the pinned key through", () => {
    expect(contactRequestGate({ pinnedSignaturePubkeyB64: KEY_A }, KEY_A)).toBe("send");
  });

  it("refuses a contact whose keys were reset and not verified", () => {
    const pin = { pinnedSignaturePubkeyB64: KEY_A, keyReset: { pendingKeyB64: KEY_B } };
    expect(contactRequestGate(pin, KEY_B)).toBe("keys-reset");
  });

  it("refuses a flagged contact even under the OLD pinned key", () => {
    const pin = { pinnedSignaturePubkeyB64: KEY_A, keyReset: { pendingKeyB64: KEY_B } };
    expect(contactRequestGate(pin, KEY_A)).toBe("keys-reset");
  });

  it("sends a pinned account under another key through the identity gate", () => {
    expect(contactRequestGate({ pinnedSignaturePubkeyB64: KEY_A }, KEY_B)).toBe("check-identity");
  });
});

describe("refusalIsRecordedReset", () => {
  it("is a reset only when the log confirmed it and it was recorded", () => {
    expect(refusalIsRecordedReset(true, true)).toBe(true);
  });

  it("is never a reset when the log did not confirm one", () => {
    expect(refusalIsRecordedReset(false, true)).toBe(false);
    expect(refusalIsRecordedReset(null, true)).toBe(false);
  });

  it("is never a reset when nothing was recorded (an invalid proof)", () => {
    expect(refusalIsRecordedReset(true, false)).toBe(false);
  });
});
