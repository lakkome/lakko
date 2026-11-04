// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.7 - a fetched bundle's two-way conversation link, after
 * the gate passed: the key it proves joins the proven chain of the key the
 * gate left trusted, on the pin or on a verified binding, and every reader of
 * that chain (the re-admission standing, the KeyPackage add filter, the
 * address resolver, the contact row's aliases) then follows the person. A
 * link that does not verify changes nothing.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { lookupStanding } from "../chat/device-readmission";
import { MemoryStorage } from "../test/fake-storage";
import { base64Decode, base64Encode } from "../auth/account-id";
import { acceptableBoundKeys, trustedBoundKeys } from "./add-member-key";
import { resolveAccountKey } from "./address-resolver";
import { carryBindingConversationKey, recordConversationLink } from "./conversation-link";
import {
  acceptReset,
  accountKeysOf,
  bindContactsAccount,
  loadAccepted,
  loadKeyAliases,
  loadSupersededKeys,
  readPin,
  setPin,
  upsertAccepted,
} from "./local-store";
import {
  accountKeysOfBinding,
  memberHeldKey,
  readVerifiedBinding,
  recordVerifiedBinding,
} from "./verified-binding";

const OWNER = "b3duZXItYWNjb3VudC0wMDAwMA==";
const ACCOUNT = "AAECAwQFBgcICQoLDA0ODw==";
const key = (fill: number): string => base64Encode(new Uint8Array(32).fill(fill));
const OLDEST = key(1);
const MIDDLE = key(2);
const CURRENT = key(3);
const LINK = base64Encode(new Uint8Array(201).fill(9));

/** The core's verifier: it names `OLDEST` under `CURRENT`, and refuses else. */
const verifies = (link: Uint8Array, trusted: Uint8Array): Uint8Array => {
  expect(link).toHaveLength(201);
  if (base64Encode(trusted) !== CURRENT) throw new Error("names another current key");
  return base64Decode(OLDEST);
};
const refuses = (): Uint8Array => {
  throw new Error("bad signature");
};

const record = (verify = verifies, fetchedKeyB64 = CURRENT): boolean =>
  recordConversationLink({ accountIdB64: ACCOUNT, fetchedKeyB64, linkB64: LINK, verify });

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount(OWNER);
});
afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
  vi.restoreAllMocks();
});

describe("on a pin", () => {
  it("makes the conversation key a proven predecessor of the pin", () => {
    setPin(ACCOUNT, CURRENT, 0);
    expect(record()).toBe(true);
    expect(accountKeysOf(ACCOUNT)).toEqual({ currentKeyB64: CURRENT, provenPredecessorsB64: [OLDEST] });
  });

  it("every reader of the chain follows the person", () => {
    setPin(ACCOUNT, CURRENT, 0);
    upsertAccepted({ signaturePubkeyB64: CURRENT, displayName: "Mira", acceptedAtMs: 1 });
    record();
    // The re-admission check.
    expect(lookupStanding({ rosterKeyB64: OLDEST, pin: readPin(ACCOUNT) })).toBe("same-person");
    // The KeyPackage add filter, with a caller's key and without one.
    const keys = accountKeysOf(ACCOUNT);
    expect(acceptableBoundKeys(CURRENT, keys)).toEqual(new Set([CURRENT, OLDEST]));
    expect(trustedBoundKeys(keys, undefined)).toEqual(new Set([CURRENT, OLDEST]));
    // The address of a leaf under the old key, and the name on it.
    expect(base64Encode(resolveAccountKey(base64Decode(OLDEST)))).toBe(CURRENT);
    expect(loadKeyAliases().get(OLDEST)).toBe(CURRENT);
    expect(loadAccepted()[0]?.previousSignaturePubkeysB64).toEqual([OLDEST]);
  });

  it("goes first: it is the oldest key the account has", () => {
    setPin(ACCOUNT, MIDDLE, 0);
    setPin(ACCOUNT, CURRENT, 1, undefined, { proven: true });
    record();
    expect(accountKeysOf(ACCOUNT)?.provenPredecessorsB64).toEqual([OLDEST, MIDDLE]);
  });

  it("is written once", () => {
    setPin(ACCOUNT, CURRENT, 0);
    expect(record()).toBe(true);
    expect(record()).toBe(false);
    expect(accountKeysOf(ACCOUNT)?.provenPredecessorsB64).toEqual([OLDEST]);
  });

  it("a contact that already followed the rotation by proof gains nothing", () => {
    setPin(ACCOUNT, OLDEST, 0);
    setPin(ACCOUNT, CURRENT, 1, undefined, { proven: true });
    expect(record()).toBe(false);
  });
});

describe("what records nothing", () => {
  it("a link that does not verify, and it never throws", () => {
    setPin(ACCOUNT, CURRENT, 0);
    upsertAccepted({ signaturePubkeyB64: CURRENT, displayName: "Mira", acceptedAtMs: 1 });
    const info = vi.spyOn(console, "info").mockImplementation(() => undefined);
    expect(record(refuses)).toBe(false);
    expect(accountKeysOf(ACCOUNT)?.provenPredecessorsB64).toEqual([]);
    expect(loadAccepted()[0]?.previousSignaturePubkeysB64).toBeUndefined();
    // A plain line with no identifier.
    expect(info).toHaveBeenCalledWith("a conversation link did not verify; ignored");
  });

  it("no link", () => {
    setPin(ACCOUNT, CURRENT, 0);
    const verify = vi.fn(verifies);
    for (const linkB64 of [undefined, ""]) {
      expect(recordConversationLink({ accountIdB64: ACCOUNT, fetchedKeyB64: CURRENT, linkB64, verify })).toBe(false);
    }
    expect(verify).not.toHaveBeenCalled();
  });

  it("a fetched key that is not what the gate left trusted, and the link is not even read", () => {
    setPin(ACCOUNT, MIDDLE, 0);
    const verify = vi.fn(verifies);
    expect(record(verify)).toBe(false);
    expect(verify).not.toHaveBeenCalled();
    expect(accountKeysOf(ACCOUNT)?.provenPredecessorsB64).toEqual([]);
  });

  it("an account this device neither pinned nor verified", () => {
    const verify = vi.fn(verifies);
    expect(record(verify)).toBe(false);
    expect(verify).not.toHaveBeenCalled();
  });

  it("a key an accepted reset retired: old keys do not come back", () => {
    setPin(ACCOUNT, OLDEST, 0);
    acceptReset({
      accountIdB64: ACCOUNT,
      heldKeyB64: OLDEST,
      newKeyB64: CURRENT,
      keyGeneration: 2,
      signedAtMs: 5,
      nowMs: 6,
    });
    expect(record()).toBe(false);
    expect(accountKeysOf(ACCOUNT)?.provenPredecessorsB64).toEqual([]);
  });
});

describe("on a verified binding (an account this device never pinned)", () => {
  const binding = { keyB64: CURRENT, keyGeneration: 3, verifiedAtMs: 1 };

  it("records the conversation key beside the binding, and pins nothing", () => {
    recordVerifiedBinding(ACCOUNT, binding);
    expect(record()).toBe(true);
    expect(readPin(ACCOUNT)).toBeUndefined();
    expect(readVerifiedBinding(ACCOUNT)?.conversationKeyB64).toBe(OLDEST);
  });

  it("the re-admission check, the add filter and the resolver follow it", () => {
    recordVerifiedBinding(ACCOUNT, binding);
    record();
    const held = readVerifiedBinding(ACCOUNT);
    expect(lookupStanding({ rosterKeyB64: OLDEST, pin: undefined, binding: held })).toBe("same-person");
    expect(lookupStanding({ rosterKeyB64: MIDDLE, pin: undefined, binding: held })).toBe("skip");
    expect(trustedBoundKeys(accountKeysOfBinding(held), held?.keyB64)).toEqual(new Set([CURRENT, OLDEST]));
    expect(base64Encode(resolveAccountKey(base64Decode(OLDEST)))).toBe(CURRENT);
    expect(
      memberHeldKey({ pinned: false, binding: held, rosterKeyB64: OLDEST, fetchedKeyB64: CURRENT }).kind,
    ).toBe("confirmed");
    // Only while the directory still serves the binding's key.
    expect(
      memberHeldKey({ pinned: false, binding: held, rosterKeyB64: OLDEST, fetchedKeyB64: MIDDLE }).kind,
    ).toBe("none");
  });

  it("a link that does not verify changes nothing", () => {
    recordVerifiedBinding(ACCOUNT, binding);
    vi.spyOn(console, "info").mockImplementation(() => undefined);
    expect(record(refuses)).toBe(false);
    expect(readVerifiedBinding(ACCOUNT)?.conversationKeyB64).toBeUndefined();
  });

  it("moves to the pin when the binding becomes one", () => {
    recordVerifiedBinding(ACCOUNT, binding);
    record();
    setPin(ACCOUNT, CURRENT, 0);
    carryBindingConversationKey(ACCOUNT, readVerifiedBinding(ACCOUNT));
    expect(accountKeysOf(ACCOUNT)?.provenPredecessorsB64).toEqual([OLDEST]);
  });

  it("a reset judged from the binding retires the conversation key too", () => {
    acceptReset({
      accountIdB64: ACCOUNT,
      heldKeyB64: CURRENT,
      heldPredecessorsB64: [OLDEST],
      newKeyB64: MIDDLE,
      keyGeneration: 4,
      signedAtMs: 5,
      nowMs: 6,
    });
    expect(loadSupersededKeys().has(OLDEST)).toBe(true);
  });
});
