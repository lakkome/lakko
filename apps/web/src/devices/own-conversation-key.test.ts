// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  bindingVerdict,
  boundAccountKeyB64,
  ownDeviceKeyPackageUsable,
  ownKeyPackageUsable,
  rebindsAfterStateBundle,
  type KeyPackageReaders,
} from "./own-conversation-key";

describe("ownKeyPackageUsable (ADR-0066, the sender rule)", () => {
  it("admits an own device bound to the key this device's leaves are bound to", () => {
    expect(
      ownKeyPackageUsable({ ownAccount: true, boundKeyB64: "old", conversationKeyB64: "old" }),
    ).toBe(true);
  });

  it("refuses an own device bound to another of the account's keys", () => {
    // The paired device minted under the CURRENT key; this device's leaves
    // are under the key of before the rotation.
    expect(
      ownKeyPackageUsable({ ownAccount: true, boundKeyB64: "new", conversationKeyB64: "old" }),
    ).toBe(false);
    // And the mirror: this device kept a current-key binding, the other is
    // bound to the earlier key.
    expect(
      ownKeyPackageUsable({ ownAccount: true, boundKeyB64: "old", conversationKeyB64: "new" }),
    ).toBe(false);
  });

  it("leaves a foreign account's device alone, whatever it is bound to", () => {
    expect(
      ownKeyPackageUsable({ ownAccount: false, boundKeyB64: "bob", conversationKeyB64: "old" }),
    ).toBe(true);
  });

  it("changes nothing for an unbound KeyPackage or an unknown conversation key", () => {
    expect(
      ownKeyPackageUsable({ ownAccount: true, boundKeyB64: null, conversationKeyB64: "old" }),
    ).toBe(true);
    expect(
      ownKeyPackageUsable({ ownAccount: true, boundKeyB64: "new", conversationKeyB64: null }),
    ).toBe(true);
  });
});

describe("boundAccountKeyB64", () => {
  it("reads an unbound KeyPackage (account key = leaf key) as no binding", () => {
    expect(boundAccountKeyB64("leaf", "leaf")).toBeNull();
  });
  it("reads a binding that did not verify as no binding", () => {
    expect(boundAccountKeyB64(undefined, "leaf")).toBeNull();
    expect(boundAccountKeyB64(null, "leaf")).toBeNull();
  });
  it("returns the bound account key", () => {
    expect(boundAccountKeyB64("account", "leaf")).toBe("account");
  });
});

describe("ownDeviceKeyPackageUsable (over the wasm readers)", () => {
  const bytes = (n: number): Uint8Array => new Uint8Array(32).fill(n);
  const b64 = (n: number): string => btoa(String.fromCharCode(...bytes(n)));
  const readers = (account: number | undefined, leaf: number): KeyPackageReaders => ({
    keyPackageAccountKey: () => (account === undefined ? undefined : bytes(account)),
    keyPackageSignatureKey: () => bytes(leaf),
  });

  it("is usable when the binding names the conversation key", () => {
    expect(ownDeviceKeyPackageUsable(readers(1, 9), new Uint8Array(), b64(1))).toBe(true);
  });
  it("is not usable when the binding names another key", () => {
    expect(ownDeviceKeyPackageUsable(readers(2, 9), new Uint8Array(), b64(1))).toBe(false);
  });
  it("is usable when unbound", () => {
    expect(ownDeviceKeyPackageUsable(readers(9, 9), new Uint8Array(), b64(1))).toBe(true);
  });
  it("leaves an unreadable KeyPackage to the admission path", () => {
    const throwing: KeyPackageReaders = {
      keyPackageAccountKey: () => {
        throw new Error("bad");
      },
      keyPackageSignatureKey: () => bytes(9),
    };
    expect(ownDeviceKeyPackageUsable(throwing, new Uint8Array(), b64(1))).toBe(true);
  });
});

describe("bindingVerdict (ADR-0066, the receiver rule)", () => {
  const base = { currentKeyB64: "new", predecessorKeysB64: ["old"] as readonly string[] };

  it("mints when there is no usable binding", () => {
    expect(bindingVerdict({ ...base, boundKeyB64: null, holdsConversations: false })).toBe("mint");
  });

  it("keeps a binding under an own predecessor, conversations or not", () => {
    expect(bindingVerdict({ ...base, boundKeyB64: "old", holdsConversations: false })).toBe("keep");
    expect(bindingVerdict({ ...base, boundKeyB64: "old", holdsConversations: true })).toBe("keep");
    expect(
      bindingVerdict({
        currentKeyB64: "newest",
        predecessorKeysB64: ["old", "new"],
        boundKeyB64: "new",
        holdsConversations: true,
      }),
    ).toBe("keep");
  });

  it("keeps a current-key binding on an account that never rotated", () => {
    expect(
      bindingVerdict({
        currentKeyB64: "new",
        predecessorKeysB64: [],
        boundKeyB64: "new",
        holdsConversations: false,
      }),
    ).toBe("keep");
  });

  it("mints again under the conversation key while the device holds no conversation", () => {
    expect(bindingVerdict({ ...base, boundKeyB64: "new", holdsConversations: false })).toBe("remint");
  });

  it("keeps a current-key binding once the device holds conversations", () => {
    expect(bindingVerdict({ ...base, boundKeyB64: "new", holdsConversations: true })).toBe(
      "keep-under-current-key",
    );
  });

  it("mints when the binding names a key that is not this account's (a reset)", () => {
    expect(
      bindingVerdict({
        currentKeyB64: "new",
        predecessorKeysB64: [],
        boundKeyB64: "gone",
        holdsConversations: true,
      }),
    ).toBe("mint");
  });

  it("only `remint` changes the binding of a live session", () => {
    expect(rebindsAfterStateBundle("remint")).toBe(true);
    expect(rebindsAfterStateBundle("keep")).toBe(false);
    expect(rebindsAfterStateBundle("keep-under-current-key")).toBe(false);
    expect(rebindsAfterStateBundle("mint")).toBe(false);
  });
});
