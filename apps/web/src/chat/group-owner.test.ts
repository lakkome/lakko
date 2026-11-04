// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { ownsGroup } from "./group-owner";

const key = (b: number): Uint8Array => new Uint8Array(32).fill(b);
const group = (owner: Uint8Array | undefined | "throws") => ({
  roleTableOwner: () => {
    if (owner === "throws") throw new Error("torn");
    return owner;
  },
});

describe("ownsGroup", () => {
  it("recognises the owner by the account key", () => {
    expect(ownsGroup(group(key(1)), [key(1), key(2)])).toBe(true);
  });
  it("recognises the owner by the conversation key after a rotation", () => {
    expect(ownsGroup(group(key(2)), [key(1), key(2)])).toBe(true);
  });
  it("is not the owner otherwise, with no table, or on a torn read", () => {
    expect(ownsGroup(group(key(3)), [key(1), null, undefined])).toBe(false);
    expect(ownsGroup(group(undefined), [key(1)])).toBe(false);
    expect(ownsGroup(group("throws"), [key(1)])).toBe(false);
  });
});
