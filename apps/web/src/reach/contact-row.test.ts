// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { contactRowAction, isCurrentContactKey, type ContactRowAction } from "./contact-row";

/** The core's rule, modelled: key 1 is current, key 2 a retired contact key. */
const verdict = (pk: Uint8Array, contact: boolean): ContactRowAction =>
  pk[0] === 2 ? (contact ? "regrant" : "drop") : "process";

describe("contactRowAction (the receive path)", () => {
  it("a proofless row and a current-key row are processed, without asking who sent it", () => {
    let asked = false;
    const ask = (): boolean => {
      asked = true;
      return false;
    };
    expect(contactRowAction({ reachPk: undefined, verdict, senderIsContact: ask })).toBe("process");
    expect(contactRowAction({ reachPk: new Uint8Array([1]), verdict, senderIsContact: ask })).toBe("process");
    expect(asked).toBe(false);
  });

  it("a retired contact key from a current contact is processed and regranted", () => {
    expect(contactRowAction({ reachPk: new Uint8Array([2]), verdict, senderIsContact: () => true })).toBe("regrant");
  });

  it("a retired contact key from anyone else is dropped", () => {
    expect(contactRowAction({ reachPk: new Uint8Array([2]), verdict, senderIsContact: () => false })).toBe("drop");
  });
});

describe("isCurrentContactKey", () => {
  const rows = new Map([["A", { signaturePubkeyB64: "A" }], ["OLD", { signaturePubkeyB64: "A" }]]);
  it("is a listed contact through any of its keys, never a removed one", () => {
    expect(isCurrentContactKey("A", (k) => rows.get(k), () => false)).toBe(true);
    expect(isCurrentContactKey("OLD", (k) => rows.get(k), () => false)).toBe(true);
    expect(isCurrentContactKey("A", (k) => rows.get(k), (k) => k === "A")).toBe(false);
    expect(isCurrentContactKey("B", (k) => rows.get(k), () => false)).toBe(false);
  });
});
