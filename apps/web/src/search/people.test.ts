// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The local half of people search and the one result list. The normaliser
 * is the REAL core's `normalizeNameForSearch`, so "letter case and accents
 * never matter" is asserted against the rule the network terms are cut by.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, { normalizeNameForSearch } from "../../wasm/lakko_ffi_wasm";
import {
  asContacts,
  canAddContact,
  fallbackNormalize,
  localPeople,
  matchesLoosely,
  mergePeople,
  namesFormerContact,
  peopleRows,
  searchNormalizer,
  shownUsername,
  type LocalCandidate,
  type Normalize,
  type PersonResult,
} from "./people";

let normalize: Normalize;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
  normalize = searchNormalizer((s) => normalizeNameForSearch(s));
});

const cand = (over: Partial<LocalCandidate> & { keyHex: string }): LocalCandidate => ({
  keyB64: `b64-${over.keyHex}`,
  name: "Anna Weber",
  verifiedUsername: null,
  isContact: false,
  accountIdHex: "11".repeat(16),
  keptGrantWires: [],
  ...over,
});

const person = (over: Partial<PersonResult> & { keyHex: string }): PersonResult => ({
  keyB64: `b64-${over.keyHex}`,
  accountIdHex: "11".repeat(16),
  name: "Anna Weber",
  username: null,
  reach: "none",
  grantWireB64: null,
  ...over,
});

describe("the normaliser", () => {
  it("agrees with the core where the core answers", () => {
    expect(normalize("  Ánna   WEBER ")).toBe(normalizeNameForSearch("  Ánna   WEBER "));
    expect(normalize("Ánna WEBER")).toBe("anna weber");
  });

  it("answers a single letter, which the core refuses: local search runs from the first letter", () => {
    expect(normalizeNameForSearch("Á")).toBeUndefined();
    expect(normalize("Á")).toBe("a");
  });

  it("refuses blank input", () => {
    expect(fallbackNormalize("   ")).toBeNull();
  });
});

describe("matchesLoosely", () => {
  it("matches any part of any word", () => {
    expect(matchesLoosely("Anna Weber", "nna", normalize)).toBe(true);
    expect(matchesLoosely("Anna Weber", "eb", normalize)).toBe(true);
    expect(matchesLoosely("Anna Weber", "a", normalize)).toBe(true);
  });

  it("ignores case, accents and repeated spaces", () => {
    expect(matchesLoosely("Ånna Wéber", "ANNA   weber", normalize)).toBe(true);
  });

  it("needs every word of the query in some word of the name", () => {
    expect(matchesLoosely("Anna Weber", "weber an", normalize)).toBe(true);
    expect(matchesLoosely("Anna Weber", "anna x", normalize)).toBe(false);
  });

  it("matches within one word, not across two", () => {
    expect(matchesLoosely("Anna Weber", "aw", normalize)).toBe(false);
  });
});

describe("shownUsername", () => {
  it("never shows the username of someone who does not let this account find them", () => {
    expect(shownUsername("anna", "none")).toBeNull();
  });

  it("shows a verified one for a contact, an open or a granting person", () => {
    expect(shownUsername("anna", "contact")).toBe("anna");
    expect(shownUsername("anna", "open")).toBe("anna");
    expect(shownUsername("anna", "grant")).toBe("anna");
  });

  it("shows nothing that was not verified", () => {
    expect(shownUsername(null, "open")).toBeNull();
  });
});

describe("localPeople", () => {
  it("lists a contact with the username", () => {
    const [r] = localPeople([cand({ keyHex: "aa", isContact: true, verifiedUsername: "anna" })], "ann", normalize, null);
    expect(r).toMatchObject({ reach: "contact", username: "anna" });
    expect(canAddContact(r!)).toBe(false);
  });

  it("lists a member who lets this account find them, with Add contact", () => {
    const [r] = localPeople(
      [cand({ keyHex: "aa", keptGrantWires: ["GRANT"], verifiedUsername: "anna" })],
      "web",
      normalize,
      null,
    );
    expect(r).toMatchObject({ reach: "grant", grantWireB64: "GRANT", username: "anna" });
    expect(canAddContact(r!)).toBe(true);
  });

  it("lists a hidden member by name only: no username, no Add contact", () => {
    const [r] = localPeople([cand({ keyHex: "aa", verifiedUsername: "anna" })], "anna", normalize, null);
    expect(r).toMatchObject({ reach: "none", username: null, grantWireB64: null });
    expect(canAddContact(r!)).toBe(false);
  });

  it("does not find a hidden member by the username it may not show", () => {
    expect(
      localPeople([cand({ keyHex: "aa", name: "Someone", verifiedUsername: "anna" })], "anna", normalize, null),
    ).toEqual([]);
  });

  it("finds a findable member by username too", () => {
    const found = localPeople(
      [cand({ keyHex: "aa", name: "Someone", verifiedUsername: "anna", keptGrantWires: ["G"] })],
      "@ann",
      normalize,
      null,
    );
    expect(found).toHaveLength(1);
  });

  it("never lists the viewer, someone with no name, or anyone twice", () => {
    const found = localPeople(
      [cand({ keyHex: "self" }), cand({ keyHex: "aa", name: null }), cand({ keyHex: "bb" }), cand({ keyHex: "bb" })],
      "anna",
      normalize,
      "self",
    );
    expect(found.map((p) => p.keyHex)).toEqual(["bb"]);
  });

  it("is empty for a blank query", () => {
    expect(localPeople([cand({ keyHex: "aa" })], "   ", normalize, null)).toEqual([]);
  });
});

describe("mergePeople", () => {
  it("keeps one row per account key, with the strongest reach", () => {
    const merged = mergePeople(
      [[person({ keyHex: "aa", reach: "none", name: "Anna (renamed)" })], [person({ keyHex: "aa", reach: "open", username: "anna" })]],
      null,
    );
    expect(merged).toHaveLength(1);
    expect(merged[0]).toMatchObject({ reach: "open", name: "Anna (renamed)", username: "anna" });
  });

  it("carries the grant of a grant row and drops it when a stronger reach wins", () => {
    const viaGrant = mergePeople(
      [[person({ keyHex: "aa", reach: "none" })], [person({ keyHex: "aa", reach: "grant", grantWireB64: "G" })]],
      null,
    );
    expect(viaGrant[0]).toMatchObject({ reach: "grant", grantWireB64: "G" });
    const viaOpen = mergePeople(
      [[person({ keyHex: "aa", reach: "grant", grantWireB64: "G" })], [person({ keyHex: "aa", reach: "open" })]],
      null,
    );
    expect(viaOpen[0]).toMatchObject({ reach: "open", grantWireB64: null });
  });

  it("keeps a contact a contact", () => {
    const merged = mergePeople(
      [[person({ keyHex: "aa", reach: "contact" })], [person({ keyHex: "aa", reach: "grant", grantWireB64: "G" })]],
      null,
    );
    expect(merged[0]).toMatchObject({ reach: "contact", grantWireB64: null });
  });

  it("a contact's fingerprint stand-in gives way to a name another list found", () => {
    const merged = mergePeople(
      [
        [person({ keyHex: "aa", reach: "contact", name: "a1b2 c3d4", nameIsFallback: true })],
        [person({ keyHex: "aa", reach: "open", name: "Anna Weber" })],
      ],
      null,
    );
    expect(merged[0]).toMatchObject({ reach: "contact", name: "Anna Weber" });
    expect(merged[0]?.nameIsFallback).toBeUndefined();
  });

  it("a contact with nothing better keeps the fingerprint", () => {
    const merged = mergePeople(
      [
        [person({ keyHex: "aa", reach: "contact", name: "a1b2 c3d4", nameIsFallback: true })],
        [person({ keyHex: "aa", reach: "open", name: "@anna", nameIsFallback: true })],
      ],
      null,
    );
    expect(merged[0]).toMatchObject({ name: "a1b2 c3d4", nameIsFallback: true });
  });

  it("with equal reach, a grant that passed a check beats one nobody asked about", () => {
    const stale = person({ keyHex: "aa", reach: "grant", grantWireB64: "STALE" });
    const live = person({ keyHex: "aa", reach: "grant", grantWireB64: "LIVE", grantChecked: true });
    expect(mergePeople([[stale], [live]], null)[0]).toMatchObject({ grantWireB64: "LIVE", grantChecked: true });
    expect(mergePeople([[live], [stale]], null)[0]).toMatchObject({ grantWireB64: "LIVE", grantChecked: true });
  });

  it("of two checked grants keeps the first", () => {
    const merged = mergePeople(
      [
        [person({ keyHex: "aa", reach: "grant", grantWireB64: "ONE", grantChecked: true })],
        [person({ keyHex: "aa", reach: "grant", grantWireB64: "TWO", grantChecked: true })],
      ],
      null,
    );
    expect(merged[0]?.grantWireB64).toBe("ONE");
  });

  it("drops the viewer and keeps first-seen order", () => {
    const merged = mergePeople(
      [[person({ keyHex: "bb" }), person({ keyHex: "self" })], [person({ keyHex: "aa" })]],
      "self",
    );
    expect(merged.map((p) => p.keyHex)).toEqual(["bb", "aa"]);
  });
});

describe("canAddContact", () => {
  it("needs an account id to reach the person's identity bundle", () => {
    expect(canAddContact(person({ keyHex: "aa", reach: "open", accountIdHex: null }))).toBe(false);
    expect(canAddContact(person({ keyHex: "aa", reach: "open" }))).toBe(true);
  });
});

describe("a row as the contact list stands now", () => {
  const found = (over: Partial<PersonResult> = {}): PersonResult => ({
    keyB64: "KEY",
    keyHex: "aa",
    accountIdHex: "11",
    name: "Anna",
    username: "anna",
    reach: "open",
    grantWireB64: null,
    openBindingB64: "BINDING",
    ...over,
  });
  const none = { keysB64: new Set<string>(), accountIdsHex: new Set<string>() };

  it("a person who became a contact since the answer offers nothing more to add", () => {
    const [row] = asContacts([found()], { ...none, keysB64: new Set(["KEY"]) });
    expect(row).toMatchObject({ reach: "contact", grantWireB64: null, username: "anna" });
    expect(row!.openBindingB64).toBeUndefined();
    expect(canAddContact(row!)).toBe(false);
  });

  it("finds them by account id when their key rotated", () => {
    const [row] = asContacts(
      [found({ reach: "grant", grantWireB64: "G", grantChecked: true })],
      { ...none, accountIdsHex: new Set(["11"]) },
    );
    expect(row).toMatchObject({ reach: "contact", grantWireB64: null });
    expect(row!.grantChecked).toBeUndefined();
  });

  it("leaves everyone else as found", () => {
    const person = found();
    expect(asContacts([person], none)[0]).toBe(person);
  });
});

describe("the rows under People in Chats", () => {
  const person = (keyB64: string, reach: PersonResult["reach"]): PersonResult => ({
    keyB64,
    keyHex: keyB64,
    accountIdHex: null,
    name: keyB64,
    username: null,
    reach,
    grantWireB64: null,
  });

  it("a contact whose one-to-one matched stands there and leaves People", () => {
    expect(peopleRows([person("A", "contact")], new Set(), new Set(["A"]))).toEqual([]);
  });

  it("a contact with no matching conversation stays", () => {
    expect(peopleRows([person("A", "contact")], new Set(), new Set(["B"]))).toHaveLength(1);
  });

  it("a person who is no contact keeps their row, and the way to add them", () => {
    expect(peopleRows([person("A", "open")], new Set(), new Set(["A"]))).toHaveLength(1);
  });

  it("a contact the list does not show yet keeps a row, so the field is not blank", () => {
    expect(peopleRows([person("A", "contact")], new Set(), new Set())).toHaveLength(1);
  });

  it("a person a list row shows leaves People whatever their reach", () => {
    for (const reach of ["contact", "open", "grant", "none"] as const) {
      expect(peopleRows([person("A", reach)], new Set(["A"]), new Set())).toEqual([]);
    }
  });

  it("the list hides only the people it shows", () => {
    expect(peopleRows([person("A", "open"), person("B", "open")], new Set(["A"]), new Set())).toEqual([
      person("B", "open"),
    ]);
  });

  it("an answer that calls a removed contact a contact is false", () => {
    const contacts = { keysB64: new Set(["B"]), accountIdsHex: new Set<string>() };
    expect(namesFormerContact([person("A", "contact")], contacts)).toBe(true);
    expect(namesFormerContact([person("B", "contact")], contacts)).toBe(false);
    expect(namesFormerContact([person("A", "open")], contacts)).toBe(false);
  });

  it("a rotated key is still the contact: the account id names them", () => {
    const rotated = { ...person("NEW", "contact"), accountIdHex: "aa" };
    expect(namesFormerContact([rotated], { keysB64: new Set(), accountIdsHex: new Set(["aa"]) })).toBe(false);
  });
});
