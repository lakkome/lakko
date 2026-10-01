// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 5 - a person in Everyone is named from their public
 * record. Records are composed and opened by the REAL wasm core
 * (`composePublicRecord` / `openPublicRecord`); only the network answers are
 * faked. A record counts only when it names the hit's account and the key
 * the identity bundle serves; anything else is ignored, never an error.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { Wasm } from "../wasm";
import { base64Encode } from "../auth/account-id";
import { toHex } from "./people";
import { searchPeople, type KnownPerson, type PeopleSearchDeps } from "./people-search";
import { openPublicRecordWith } from "./wiring";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const ANNA_KEY = new Uint8Array(32).fill(0xa1);
const ANNA_ACC = new Uint8Array(16).fill(0xa2);
const BO_KEY = new Uint8Array(32).fill(0xb1);
const BO_ACC = new Uint8Array(16).fill(0xb2);

/** A 64-byte stand-in for an OPRF output / lookup key. */
const bytes64 = (b: number): Uint8Array => new Uint8Array(64).fill(b);
const TERM_OUT = bytes64(0x31);
const TERM_KEY = bytes64(0x32);
const USER_OUT = bytes64(0x41);
const USER_KEY = bytes64(0x42);

const NOBODY: KnownPerson = {
  name: null,
  contactFallbackName: null,
  verifiedUsername: null,
  isContact: false,
  keptGrantWireB64: null,
};

/** A public record as the person publishes it: over their name term and
 *  username outputs. */
function publicRecord(identityPk: Uint8Array, accountId: Uint8Array, name: string): Uint8Array {
  const payload = wasm.IntroductionPayload.forPublicRecord(identityPk, accountId, name, "anna", undefined);
  try {
    return wasm.composePublicRecord(payload, [TERM_OUT, USER_OUT], 1n);
  } finally {
    payload.free();
  }
}

function deps(over: Partial<PeopleSearchDeps> = {}): PeopleSearchDeps {
  const bundles = new Map<string, Uint8Array>([
    [toHex(ANNA_ACC), ANNA_KEY],
    [toHex(BO_ACC), BO_KEY],
  ]);
  return {
    canonicalUsername: (typed) => {
      const bare = typed.trim().replace(/^@+/, "").toLowerCase();
      return /^[a-z0-9_]{3,32}$/.test(bare) ? bare : null;
    },
    queryTerm: (q) => (q.trim().length >= 2 ? q.trim().toLowerCase().split(/\s+/)[0]! : null),
    nameMatches: (name, q) => wasm.nameMatchesQuery(name, q),
    usernameOprf: async () => ({ lookupKey: USER_KEY, output: USER_OUT }),
    accountOfUsernameLookupKey: async () => null,
    nameTermOprf: async () => ({ lookupKey: TERM_KEY, output: TERM_OUT }),
    accountsOfNameTermLookupKey: async () => [],
    identityKeyOf: async (acc) => bundles.get(toHex(acc)) ?? null,
    known: () => NOBODY,
    openIntroductions: async () => [],
    openPublicRecord: (record, output) => openPublicRecordWith(wasm as unknown as Wasm, record, output),
    verifyUsername: async () => false,
    reachCheck: async () => true,
    encodeGrant: (wire) => base64Encode(wire),
    ownKeyHex: null,
    ...over,
  };
}

describe("a name hit in Everyone", () => {
  it("is named from its public record, opened with the term's OPRF output", async () => {
    const record = publicRecord(ANNA_KEY, ANNA_ACC, "Anna Weber");
    const out = await searchPeople(
      "anna w",
      deps({ accountsOfNameTermLookupKey: async () => [{ accountId: ANNA_ACC, publicRecord: record }] }),
    );
    expect(out.people).toHaveLength(1);
    expect(out.people[0]).toMatchObject({
      name: "Anna Weber",
      reach: "open",
      keyHex: toHex(ANNA_KEY),
      accountIdHex: toHex(ANNA_ACC),
    });
  });

  it("ignores a record naming another account (the hit stays unnamed and is dropped)", async () => {
    // Bo's record served beside Anna's account id.
    const record = publicRecord(BO_KEY, BO_ACC, "Anna Weber");
    const out = await searchPeople(
      "anna",
      deps({ accountsOfNameTermLookupKey: async () => [{ accountId: ANNA_ACC, publicRecord: record }] }),
    );
    expect(out.people).toEqual([]);
    expect(out.state).toEqual({ kind: "not-found" });
  });

  it("ignores a record whose key is not the one the identity bundle serves", async () => {
    const record = publicRecord(BO_KEY, ANNA_ACC, "Anna Weber");
    const out = await searchPeople(
      "anna",
      deps({ accountsOfNameTermLookupKey: async () => [{ accountId: ANNA_ACC, publicRecord: record }] }),
    );
    expect(out.people).toEqual([]);
  });

  it("ignores a record that does not open, and one that is not a record", async () => {
    const sealedElsewhere = (() => {
      const payload = wasm.IntroductionPayload.forPublicRecord(ANNA_KEY, ANNA_ACC, "Anna Weber", undefined, undefined);
      try {
        return wasm.composePublicRecord(payload, [bytes64(0x77)], 1n);
      } finally {
        payload.free();
      }
    })();
    for (const record of [sealedElsewhere, new Uint8Array([1, 2, 3])]) {
      const out = await searchPeople(
        "anna",
        deps({ accountsOfNameTermLookupKey: async () => [{ accountId: ANNA_ACC, publicRecord: record }] }),
      );
      expect(out.people).toEqual([]);
      expect(out.state).toEqual({ kind: "not-found" });
    }
  });

  it("prefers a name this device already holds", async () => {
    const record = publicRecord(ANNA_KEY, ANNA_ACC, "Anna Weber");
    const out = await searchPeople(
      "anna",
      deps({
        accountsOfNameTermLookupKey: async () => [{ accountId: ANNA_ACC, publicRecord: record }],
        known: (k) => (k === base64Encode(ANNA_KEY) ? { ...NOBODY, name: "Anna (work)" } : NOBODY),
      }),
    );
    expect(out.people.map((p) => p.name)).toEqual(["Anna (work)"]);
  });
});

describe("a username hit", () => {
  it("is named from its public record, opened with the username's OPRF output", async () => {
    const record = publicRecord(ANNA_KEY, ANNA_ACC, "Anna Weber");
    const out = await searchPeople(
      "anna",
      deps({ accountOfUsernameLookupKey: async () => ({ accountId: ANNA_ACC, publicRecord: record }) }),
    );
    expect(out.people[0]).toMatchObject({ name: "Anna Weber", username: "anna" });
  });

  it("keeps its username as the name when the record names another key", async () => {
    const record = publicRecord(BO_KEY, ANNA_ACC, "Someone Else");
    const out = await searchPeople(
      "anna",
      deps({ accountOfUsernameLookupKey: async () => ({ accountId: ANNA_ACC, publicRecord: record }) }),
    );
    expect(out.people[0]).toMatchObject({ name: "@anna", username: "anna" });
  });
});

describe("a contact nothing names (only a fingerprint)", () => {
  const ANNA_FP = "a1a1 a1a1";
  const unnamedContact = (k: string): KnownPerson =>
    k === base64Encode(ANNA_KEY)
      ? { ...NOBODY, isContact: true, contactFallbackName: ANNA_FP }
      : NOBODY;

  it("is found by the name they published, and shown by it", async () => {
    const record = publicRecord(ANNA_KEY, ANNA_ACC, "Anna Weber");
    const out = await searchPeople(
      "anna w",
      deps({
        accountsOfNameTermLookupKey: async () => [{ accountId: ANNA_ACC, publicRecord: record }],
        known: unnamedContact,
      }),
    );
    expect(out.people).toHaveLength(1);
    expect(out.people[0]).toMatchObject({ name: "Anna Weber", reach: "contact" });
  });

  it("keeps the fingerprint when an introduction names nobody and nothing else does", async () => {
    // A username hit whose record does not open names the contact by the
    // username; the fingerprint is the last resort, never beaten by nothing.
    const out = await searchPeople(
      "anna",
      deps({
        accountOfUsernameLookupKey: async () => ({ accountId: ANNA_ACC, publicRecord: null }),
        known: unnamedContact,
      }),
    );
    expect(out.people[0]).toMatchObject({ name: "@anna", reach: "contact" });
  });
});
