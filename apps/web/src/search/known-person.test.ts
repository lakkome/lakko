// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What search knows about a person, answered from the signed-in account's
 * contact list: a contact nothing names is a contact with a stand-in name
 * (their fingerprint), not a named one, so a name they published can beat
 * it; and another account's contacts on the same browser are not contacts.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { bindContactsAccount, upsertAccepted } from "../contacts/local-store";
import type { ProfileApi } from "../profile/ProfileContext";
import type { ResolvedName } from "../profile/resolver";
import { EMPTY_RECEIVED_GRANTS, fileContactGrant, fileFoundGrant, fileScopedGrant } from "../reach/received-grants";
import { base64Decode } from "../auth/account-id";
import { toHex } from "./people";
import { knownPerson, localCandidates } from "./wiring";

const session = vi.hoisted(() => ({ current: null as null | { receivedGrants: () => unknown } }));
vi.mock("../reach/reach-service", async (original) => ({
  ...(await original<typeof import("../reach/reach-service")>()),
  reachSession: () => session.current,
}));

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, String(value));
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return Array.from(this.map.keys())[i] ?? null;
  }
}

const DAN_KEY = "RGFuLWtleS0wMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDA=";
const ACCOUNT_CARA = "Y2FyYS1hY2NvdW50";
const ACCOUNT_EVE = "ZXZlLWFjY291bnQ=";

/** A profile store that knows nothing about Dan but his fingerprint. */
const profiles = {
  resolve: (): ResolvedName =>
    ({
      name: "0d1a 2b3c",
      source: "fingerprint",
      username: null,
    }) as unknown as ResolvedName,
  accountIdBindings: () => [],
  accountIdHexOf: () => null,
} as unknown as ProfileApi;

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount(ACCOUNT_CARA);
  upsertAccepted({ signaturePubkeyB64: DAN_KEY, displayName: "", acceptedAtMs: 1 });
});

afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

describe("knownPerson", () => {
  it("a contact nothing names has no name, only a fingerprint stand-in", () => {
    const k = knownPerson(profiles, DAN_KEY);
    expect(k).toMatchObject({ name: null, contactFallbackName: "0d1a 2b3c", isContact: true });
  });

  it("is not a contact of another account signed in on the same browser", () => {
    bindContactsAccount(ACCOUNT_EVE);
    const k = knownPerson(profiles, DAN_KEY);
    expect(k).toMatchObject({ name: null, contactFallbackName: null, isContact: false });
  });
});

describe("the grants kept for a person", () => {
  afterEach(() => {
    session.current = null;
  });

  it("are all carried, in order, and never a bare member grant", () => {
    const hex = toHex(base64Decode(DAN_KEY));
    const grant = (wireB64: string) => ({ wireB64, version: 1, receivedAtMs: 1 });
    let store = fileContactGrant(EMPTY_RECEIVED_GRANTS, hex, grant("CONTACT"));
    store = fileScopedGrant(store, hex, "g1", "group", grant("GROUP"));
    store = fileScopedGrant(store, hex, "g1", "member", grant("MEMBER"));
    store = fileScopedGrant(store, hex, "c1", "channel", grant("CHANNEL"));
    store = fileFoundGrant(store, hex, grant("FOUND"));
    session.current = { receivedGrants: () => store };
    expect(knownPerson(profiles, DAN_KEY).keptGrantWires).toEqual(["CONTACT", "GROUP", "CHANNEL", "FOUND"]);
    expect(localCandidates(profiles, [])[0]?.keptGrantWires).toEqual(["CONTACT", "GROUP", "CHANNEL", "FOUND"]);
  });
});

describe("localCandidates", () => {
  it("lists the account's own contact by the fingerprint, marked as a stand-in", () => {
    const [c] = localCandidates(profiles, []);
    expect(c).toMatchObject({ keyB64: DAN_KEY, name: "0d1a 2b3c", nameIsFallback: true, isContact: true });
  });

  it("lists none of another account's contacts", () => {
    bindContactsAccount(ACCOUNT_EVE);
    expect(localCandidates(profiles, [])).toEqual([]);
  });
});
