// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  OWN_LINK_ABSENT,
  OWN_LINK_UNKNOWN,
  bindingVerdict,
  ownLinkOfBundle,
  type OwnLink,
  boundAccountKeyB64,
  ownDeviceKeyPackageUsable,
  ownKeyPackageUsable,
  rebindsAfterStateBundle,
  type KeyPackageReaders,
} from "./own-conversation-key";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

describe("ownKeyPackageUsable (ADR-0074, the sender rule)", () => {
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

// The 17 shared vectors of `lakko_contacts::own_binding` (contract G-56
// section 2.1), through the wasm core: every client's suite carries them.
describe("bindingVerdict (ADR-0074 'Joining after a rotation', the shared truth table)", () => {
  const key = (n: number): Uint8Array => new Uint8Array(32).fill(n);
  const CUR = key(0xc0);
  const K0 = key(0x10);
  const K1 = key(0x11);
  const FOREIGN = key(0xee);
  type Link = "absent" | "k0" | "unknown";
  const link = (l: Link): OwnLink =>
    l === "absent" ? OWN_LINK_ABSENT : l === "unknown" ? OWN_LINK_UNKNOWN : { kind: "key", key: K0 };
  const cases: readonly [Uint8Array | null, readonly Uint8Array[], Link, boolean, string][] = [
    [null, [], "absent", false, "mint:cur"],
    [null, [K0], "absent", false, "mint:k0"],
    [null, [K0], "k0", false, "mint:k0"],
    [null, [], "k0", false, "wait"],
    [null, [K1], "k0", false, "wait"],
    [null, [K0, K1], "k0", false, "mint:k0"],
    [null, [], "unknown", false, "wait"],
    [CUR, [], "absent", false, "keep"],
    [CUR, [], "k0", false, "wait"],
    [CUR, [K0], "k0", false, "mint:k0"],
    [CUR, [K0], "k0", true, "keep-under-current-key"],
    [CUR, [], "k0", true, "keep-under-current-key"],
    [K0, [K0], "k0", true, "keep"],
    [K1, [K0, K1], "k0", true, "keep"],
    [FOREIGN, [], "absent", false, "mint:cur"],
    [FOREIGN, [], "k0", false, "wait"],
    [FOREIGN, [K0], "unknown", false, "wait"],
  ];
  const named = (k: Uint8Array | null): string =>
    k === null ? "cur" : k[0] === 0xc0 ? "cur" : k[0] === 0x10 ? "k0" : k[0] === 0x11 ? "k1" : "?";

  it.each(cases.map((c, i) => [i + 1, ...c] as const))(
    "vector %i",
    (_i, bound, preds, l, conv, expected) => {
      const d = bindingVerdict(wasm, {
        boundKey: bound,
        currentKey: CUR,
        predecessorKeys: preds,
        link: link(l),
        holdsConversations: conv,
      });
      const got = d.kind === "mint" ? `mint:${named(d.mintKey)}` : d.kind;
      expect(got).toBe(expected);
      expect(d.isKept).toBe(d.kind === "keep" || d.kind === "keep-under-current-key");
    },
  );

  it("re-mints a live session only over a binding under the current key", () => {
    const remint = bindingVerdict(wasm, {
      boundKey: CUR,
      currentKey: CUR,
      predecessorKeys: [K0],
      link: OWN_LINK_ABSENT,
      holdsConversations: false,
    });
    expect(rebindsAfterStateBundle(remint, CUR, CUR)).toBe(true);
    const fresh = bindingVerdict(wasm, {
      boundKey: null,
      currentKey: CUR,
      predecessorKeys: [K0],
      link: OWN_LINK_ABSENT,
      holdsConversations: false,
    });
    expect(rebindsAfterStateBundle(fresh, null, CUR)).toBe(false);
    const kept = bindingVerdict(wasm, {
      boundKey: K0,
      currentKey: CUR,
      predecessorKeys: [K0],
      link: OWN_LINK_ABSENT,
      holdsConversations: true,
    });
    expect(rebindsAfterStateBundle(kept, K0, CUR)).toBe(false);
  });
});

describe("ownLinkOfBundle (the own conversation link, verified against THIS device's key)", () => {
  const b64 = (bytes: Uint8Array): string => btoa(String.fromCharCode(...bytes));
  const conversation = (): InstanceType<typeof wasm.Identity> =>
    wasm.Identity.fromSeed(new Uint8Array(64).fill(0x21));
  const current = (): InstanceType<typeof wasm.Identity> =>
    wasm.Identity.fromSeed(new Uint8Array(64).fill(0x22));

  it("is absent for no link and an empty link", () => {
    const cur = current();
    expect(ownLinkOfBundle(wasm, undefined, cur.signaturePublicKey)).toEqual(OWN_LINK_ABSENT);
    expect(ownLinkOfBundle(wasm, "", cur.signaturePublicKey)).toEqual(OWN_LINK_ABSENT);
    cur.free();
  });

  it("names the conversation key when the link verifies against the current key", () => {
    const conv = conversation();
    const cur = current();
    const link = wasm.signConversationLink(conv.ed25519Seed, cur.ed25519Seed, BigInt(1_700_000_000));
    const answer = ownLinkOfBundle(wasm, b64(link), cur.signaturePublicKey);
    expect(answer.kind).toBe("key");
    expect(answer.kind === "key" ? Array.from(answer.key) : []).toEqual(Array.from(conv.signaturePublicKey));
    conv.free();
    cur.free();
  });

  it("is absent when the link names another current key (minted before a reset)", () => {
    const conv = conversation();
    const cur = current();
    const other = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x23));
    const link = wasm.signConversationLink(conv.ed25519Seed, cur.ed25519Seed, BigInt(1_700_000_000));
    expect(ownLinkOfBundle(wasm, b64(link), other.signaturePublicKey)).toEqual(OWN_LINK_ABSENT);
    conv.free();
    cur.free();
    other.free();
  });

  it("is absent for a link that does not decode", () => {
    const cur = current();
    expect(ownLinkOfBundle(wasm, "not base64 !", cur.signaturePublicKey)).toEqual(OWN_LINK_ABSENT);
    expect(ownLinkOfBundle(wasm, b64(new Uint8Array(201)), cur.signaturePublicKey)).toEqual(OWN_LINK_ABSENT);
    cur.free();
  });

  it("is absent for a link naming the current key itself", () => {
    const cur = current();
    // The core never signs one (two seeds, one key); a hand-built 201-byte
    // blob naming the current key twice is not a valid link either way.
    const bogus = new Uint8Array(201);
    bogus.set(cur.signaturePublicKey, 1);
    bogus.set(cur.signaturePublicKey, 33);
    expect(ownLinkOfBundle(wasm, b64(bogus), cur.signaturePublicKey)).toEqual(OWN_LINK_ABSENT);
    cur.free();
  });
});
