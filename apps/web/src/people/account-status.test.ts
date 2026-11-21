// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 — the per-person account status against the REAL core rules
 * (`accountDeletionStatus`, `mayShowUsername`, `directoryCheckDue`), the
 * directory-answer classification, the row codec and the store.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  classifyIdentityAnswer,
  decodePersonStatusRows,
  deletionEffectsOf,
  directoryCheckIsDue,
  encodePersonStatusRows,
  isDeletedPerson,
  PersonStatusStore,
  usernameShowable,
  withAnswer,
  withAttempt,
  withRemoval,
  withStatement,
  type AccountStatusRules,
  type PersonStatusBackend,
  type PersonStatusRecord,
} from "./account-status";

let rules: AccountStatusRules;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
  rules = wasm as unknown as AccountStatusRules;
});

const DAY = 24 * 60 * 60 * 1000;
const NOW = 1_800_000_000_000;

describe("whether a person is shown as Deleted account", () => {
  it("an unknown person is active", () => {
    expect(isDeletedPerson(rules, undefined)).toBe(false);
  });

  it("a verified statement marks the person deleted; a later SamePerson answer reverts it", () => {
    const rec = withStatement(undefined);
    expect(isDeletedPerson(rules, rec)).toBe(true);
    expect(isDeletedPerson(rules, withAnswer(rec, "same-person", NOW))).toBe(false);
  });

  it("recording a statement clears the earlier answer, so only a later one counts", () => {
    const before = withAttempt(withAnswer(undefined, "same-person", NOW - DAY), NOW - 1000);
    const rec = withStatement(before);
    expect(rec.lastDirectoryAnswer).toBeUndefined();
    expect(rec.lastAnswerAtMs).toBeUndefined();
    expect(rec.lastAttemptAtMs).toBeUndefined();
    // The earlier SamePerson no longer hides the statement.
    expect(isDeletedPerson(rules, rec)).toBe(true);
  });

  it("a directory Absent marks the person deleted", () => {
    expect(isDeletedPerson(rules, withAnswer(undefined, "absent", NOW))).toBe(true);
  });

  it("OtherKey never marks the person deleted", () => {
    expect(isDeletedPerson(rules, withAnswer(undefined, "other-key", NOW))).toBe(false);
  });

  it("the destructive effects need a verified statement AND an Absent answer", () => {
    const none = deletionEffectsOf(rules, undefined);
    expect(none).toEqual({ showDeleted: false, removeFromGroups: false, dropHeldSends: false });
    // A statement alone: shown, nothing removed or dropped.
    const stmt = deletionEffectsOf(rules, withStatement(undefined));
    expect(stmt).toEqual({ showDeleted: true, removeFromGroups: false, dropHeldSends: false });
    // A directory Absent alone: shown, nothing removed or dropped.
    const absent = deletionEffectsOf(rules, withAnswer(undefined, "absent", NOW));
    expect(absent).toEqual({ showDeleted: true, removeFromGroups: false, dropHeldSends: false });
    // Both: everything.
    const both = deletionEffectsOf(rules, withAnswer(withStatement(undefined), "absent", NOW));
    expect(both).toEqual({ showDeleted: true, removeFromGroups: true, dropHeldSends: true });
    // An Absent recorded BEFORE the statement does not count: the statement clears it.
    const stale = deletionEffectsOf(rules, withStatement(withAnswer(undefined, "absent", NOW - DAY)));
    expect(stale.removeFromGroups).toBe(false);
    expect(stale.dropHeldSends).toBe(false);
    // SamePerson after the statement: nothing at all.
    const back = deletionEffectsOf(rules, withAnswer(withStatement(undefined), "same-person", NOW));
    expect(back).toEqual({ showDeleted: false, removeFromGroups: false, dropHeldSends: false });
  });

  it("SamePerson after Absent makes the person active again", () => {
    const absent = withAnswer(undefined, "absent", NOW);
    const back = withAnswer(absent, "same-person", NOW + 1000);
    expect(isDeletedPerson(rules, back)).toBe(false);
    expect(back.lastSamePersonAtMs).toBe(NOW + 1000);
  });
});

describe("whether a username may be shown", () => {
  it("never before a confirmation", () => {
    expect(usernameShowable(rules, undefined, NOW)).toBe(false);
  });

  it("within 21 days of the last SamePerson, and not after", () => {
    const rec = withAnswer(undefined, "same-person", NOW - DAY);
    expect(usernameShowable(rules, rec, NOW)).toBe(true);
    const old = withAnswer(undefined, "same-person", NOW - 22 * DAY);
    expect(usernameShowable(rules, old, NOW)).toBe(false);
  });

  it("never for a deleted person, even recently confirmed", () => {
    const rec = withStatement(withAnswer(undefined, "same-person", NOW - DAY));
    expect(usernameShowable(rules, rec, NOW)).toBe(false);
  });

  it("an OtherKey answer does not refresh the confirmation", () => {
    const rec = withAnswer(withAnswer(undefined, "same-person", NOW - 22 * DAY), "other-key", NOW);
    expect(usernameShowable(rules, rec, NOW)).toBe(false);
  });
});

describe("when the directory may be asked", () => {
  it("due with no answer, not within an hour of an attempt", () => {
    expect(directoryCheckIsDue(rules, undefined, NOW)).toBe(true);
    expect(directoryCheckIsDue(rules, withAttempt(undefined, NOW - 60_000), NOW)).toBe(false);
  });

  it("due at once after a statement, even right after an earlier answer and attempt", () => {
    expect(directoryCheckIsDue(rules, withStatement(undefined), NOW)).toBe(true);
    const asked = withAttempt(withAnswer(undefined, "same-person", NOW - 60_000), NOW - 60_000);
    expect(directoryCheckIsDue(rules, asked, NOW)).toBe(false);
    expect(directoryCheckIsDue(rules, withStatement(asked), NOW)).toBe(true);
  });

  it("not within 7 days of an answer, due after", () => {
    const fresh = withAnswer(undefined, "same-person", NOW - DAY);
    expect(directoryCheckIsDue(rules, fresh, NOW)).toBe(false);
    const stale = withAnswer(undefined, "same-person", NOW - 8 * DAY);
    expect(directoryCheckIsDue(rules, stale, NOW)).toBe(true);
  });
});

describe("what one identity answer means for a held key", () => {
  const enc = (b: Uint8Array): string => Buffer.from(b).toString("base64");
  const dec = (s: string): Uint8Array => new Uint8Array(Buffer.from(s, "base64"));
  const HELD = enc(new Uint8Array(32).fill(1));
  const OTHER = enc(new Uint8Array(32).fill(2));
  const refuse = (): Uint8Array => {
    throw new Error("does not verify");
  };

  it("404 is Absent", () => {
    expect(classifyIdentityAnswer(HELD, null, refuse, dec, enc)).toBe("absent");
  });

  it("the held key is SamePerson", () => {
    expect(classifyIdentityAnswer(HELD, { signaturePubkeyB64: HELD }, refuse, dec, enc)).toBe("same-person");
  });

  it("another key with a link naming the held key is SamePerson", () => {
    const names = (): Uint8Array => dec(HELD);
    expect(
      classifyIdentityAnswer(HELD, { signaturePubkeyB64: OTHER, conversationLinkB64: "AAAA" }, names, dec, enc),
    ).toBe("same-person");
  });

  it("another key with no link, or a link that does not verify, is OtherKey", () => {
    expect(classifyIdentityAnswer(HELD, { signaturePubkeyB64: OTHER }, refuse, dec, enc)).toBe("other-key");
    expect(
      classifyIdentityAnswer(HELD, { signaturePubkeyB64: OTHER, conversationLinkB64: "AAAA" }, refuse, dec, enc),
    ).toBe("other-key");
  });
});

describe("the row and the store", () => {
  it("round-trips every field", () => {
    const rec: PersonStatusRecord = withRemoval(
      withAttempt(withAnswer(withStatement(undefined, "ab".repeat(16)), "absent", NOW), NOW + 5),
      "g1",
      "owed",
    );
    const rows = new Map([["k1", rec]]);
    const back = decodePersonStatusRows(encodePersonStatusRows(rows));
    expect(back.get("k1")).toEqual(rec);
  });

  it("an unreadable row is empty", () => {
    expect(decodePersonStatusRows("not json").size).toBe(0);
    expect(decodePersonStatusRows(JSON.stringify({ v: 99, p: {} })).size).toBe(0);
  });

  it("writes through to the backend only while an account is bound", async () => {
    const saved: ReadonlyMap<string, PersonStatusRecord>[] = [];
    const backend: PersonStatusBackend = {
      load: async () => null,
      save: async (_a, rows) => {
        saved.push(rows);
      },
    };
    const store = new PersonStatusStore(backend);
    await store.update("k", (r) => withStatement(r));
    expect(store.get("k")).toBeUndefined();
    await store.open("acct");
    await store.update("k", (r) => withStatement(r));
    expect(store.get("k")?.statementVerified).toBe(true);
    expect(saved.length).toBe(1);
    store.close();
    expect(store.get("k")).toBeUndefined();
  });
});
