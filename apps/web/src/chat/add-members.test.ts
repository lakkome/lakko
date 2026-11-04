// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Add members screen's rules (`docs/ux/conversation-info.md` section 2.4):
 * who is listed, who may be picked, the trailing button, and the add sequence
 * whose partial failure names the people it could not add.
 */
import { describe, expect, it } from "vitest";

import type { PersonResult } from "../search/people";
import {
  addPicksInTurn,
  contactRows,
  livePicks,
  personPickable,
  pendingPersonKeys,
  personRows,
  pickOfContact,
  pickOfPerson,
  submitLabel,
  togglePick,
  type AddPick,
  type ContactCandidate,
  type Excluded,
} from "./add-members";

const loose = (text: string, query: string): boolean => text.toLowerCase().includes(query.toLowerCase());

const contact = (keyB64: string, name: string, accountIdHex?: string, username: string | null = null): ContactCandidate => ({
  keyB64,
  accountIdHex,
  name,
  username,
});

const none: Excluded = { memberKeysB64: new Set(), pendingKeysB64: new Set(), ownKeyB64: null };

const person = (over: Partial<PersonResult> & { keyB64: string }): PersonResult => ({
  keyHex: over.keyB64,
  accountIdHex: "00".repeat(16),
  name: over.keyB64,
  username: null,
  reach: "open",
  grantWireB64: null,
  ...over,
});

describe("contactRows", () => {
  const anna = contact("A", "Anna Weber", "aa", "anna");
  const bob = contact("B", "Bob", "bb");
  const old = contact("C", "Carol");

  it("lists contacts not in the group and not pending, in the given order", () => {
    const rows = contactRows([anna, bob, old], "", {
      memberKeysB64: new Set(["B"]),
      pendingKeysB64: new Set(),
      ownKeyB64: null,
    }, loose);
    expect(rows.map((r) => r.contact.keyB64)).toEqual(["A", "C"]);
  });

  it("leaves out a contact an add is already waiting for, and this account", () => {
    const rows = contactRows([anna, bob, old], "", {
      memberKeysB64: new Set(),
      pendingKeysB64: new Set(["A"]),
      ownKeyB64: "C",
    }, loose);
    expect(rows.map((r) => r.contact.keyB64)).toEqual(["B"]);
  });

  it("lists a contact with no account id dimmed, not pickable", () => {
    const rows = contactRows([old], "", none, loose);
    expect(rows).toEqual([{ contact: old, pickable: false }]);
    expect(pickOfContact(old)).toBeNull();
  });

  it("filters from the first letter, by name or username, ignoring a leading @", () => {
    expect(contactRows([anna, bob], "bo", none, loose).map((r) => r.contact.keyB64)).toEqual(["B"]);
    expect(contactRows([anna, bob], "@ann", none, loose).map((r) => r.contact.keyB64)).toEqual(["A"]);
    expect(contactRows([anna, bob], "  ", none, loose)).toHaveLength(2);
    expect(contactRows([anna, bob], "zz", none, loose)).toHaveLength(0);
  });
});

describe("personRows", () => {
  it("skips members, pending adds, contacts, this account and duplicates", () => {
    const rows = personRows(
      [
        person({ keyB64: "M" }),
        person({ keyB64: "P" }),
        person({ keyB64: "K" }),
        person({ keyB64: "R", reach: "contact" }),
        person({ keyB64: "O" }),
        person({ keyB64: "N" }),
        person({ keyB64: "N" }),
      ],
      new Set(["K"]),
      { memberKeysB64: new Set(["M"]), pendingKeysB64: new Set(["P"]), ownKeyB64: "O" },
    );
    expect(rows.map((r) => r.person.keyB64)).toEqual(["N"]);
  });

  it("shows a result with no account id, or no way to reach them, without a checkbox", () => {
    const noId = person({ keyB64: "X", accountIdHex: null });
    const hidden = person({ keyB64: "Y", reach: "none" });
    const rows = personRows([noId, hidden], new Set(), none);
    expect(rows.map((r) => r.pickable)).toEqual([false, false]);
    expect(personPickable(person({ keyB64: "Z", reach: "grant", grantWireB64: "g" }))).toBe(true);
  });

  it("a pick from search carries the grant that found the person, never for an open result", () => {
    expect(pickOfPerson(person({ keyB64: "G", reach: "grant", grantWireB64: "wire", name: "Gil" }))).toEqual({
      keyB64: "G",
      accountIdHex: "00".repeat(16),
      label: "Gil",
      grantWireB64: "wire",
      openBindingB64: null,
    });
    expect(pickOfPerson(person({ keyB64: "O", grantWireB64: "stray" }))?.grantWireB64).toBeNull();
    expect(pickOfPerson(person({ keyB64: "Q", accountIdHex: null }))).toBeNull();
  });

  it("a pick from an Everyone result carries the mailbox binding of their public record, and only then", () => {
    expect(pickOfPerson(person({ keyB64: "E", reach: "open", openBindingB64: "bind", name: "Eve" }))).toEqual({
      keyB64: "E",
      accountIdHex: "00".repeat(16),
      label: "Eve",
      grantWireB64: null,
      openBindingB64: "bind",
    });
    // A grant result never carries a binding, even a stray one.
    expect(
      pickOfPerson(person({ keyB64: "G", reach: "grant", grantWireB64: "w", openBindingB64: "stray" }))
        ?.openBindingB64,
    ).toBeNull();
    // An open result with no known binding is still a pick, with none.
    expect(pickOfPerson(person({ keyB64: "O" }))?.openBindingB64).toBeNull();
  });
});

describe("picks and the trailing button", () => {
  const a: AddPick = { keyB64: "A", accountIdHex: "aa", label: "Anna", grantWireB64: null, openBindingB64: null };
  const b: AddPick = { keyB64: "B", accountIdHex: "bb", label: "Bob", grantWireB64: null, openBindingB64: null };

  it("toggles in pick order", () => {
    const one = togglePick([], a);
    const two = togglePick(one, b);
    expect(two.map((p) => p.keyB64)).toEqual(["A", "B"]);
    expect(togglePick(two, a).map((p) => p.keyB64)).toEqual(["B"]);
  });

  it("drops a pick who joined or whose add started meanwhile", () => {
    expect(
      livePicks([a, b], { memberKeysB64: new Set(["A"]), pendingKeysB64: new Set(), ownKeyB64: null }),
    ).toEqual([b]);
  });

  it("reads Add up to one pick and Add {count} from two", () => {
    expect(submitLabel(0)).toEqual({ kind: "single" });
    expect(submitLabel(1)).toEqual({ kind: "single" });
    expect(submitLabel(3)).toEqual({ kind: "count", count: 3 });
  });
});

describe("addPicksInTurn", () => {
  const picks: AddPick[] = ["Anna", "Bob", "Carol"].map((label, i) => ({
    keyB64: String(i),
    accountIdHex: "aa",
    label,
    grantWireB64: null,
    openBindingB64: null,
  }));

  it("adds one after another, then sweeps once, and names the failures in order", async () => {
    const log: string[] = [];
    let running = 0;
    const failed = await addPicksInTurn(picks, {
      addOne: async (p) => {
        running += 1;
        expect(running).toBe(1);
        log.push(`add ${p.label}`);
        await Promise.resolve();
        running -= 1;
        if (p.label !== "Bob") throw new Error("refused");
      },
      sweep: async () => {
        log.push("sweep");
      },
    });
    expect(log).toEqual(["add Anna", "add Bob", "add Carol", "sweep"]);
    expect(failed).toEqual(["Anna", "Carol"]);
  });

  it("a failed sweep is not a failed add", async () => {
    const failed = await addPicksInTurn(picks, {
      addOne: async () => {},
      sweep: async () => {
        throw new Error("offline");
      },
    });
    expect(failed).toEqual([]);
  });
});

describe("pendingPersonKeys", () => {
  it("maps a pending device leaf back to the person this device asked for", () => {
    const keys = pendingPersonKeys(["leaf-1"], [
      { subjectKeyB64: "anna", deviceKeysB64: ["leaf-1", "leaf-2"] },
      { subjectKeyB64: "bob", deviceKeysB64: ["leaf-9"] },
    ]);
    expect(keys.has("anna")).toBe(true);
    expect(keys.has("bob")).toBe(false);
    expect(keys.has("leaf-1")).toBe(true);
  });

  it("keeps an unbound leaf, whose key is the person key", () => {
    const keys = pendingPersonKeys(["carol"], [{ subjectKeyB64: "carol", deviceKeysB64: [] }]);
    expect([...keys]).toEqual(["carol"]);
  });
});
