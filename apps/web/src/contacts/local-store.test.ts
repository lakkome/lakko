// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the per-account contact store `local-store.ts`: the
 * G-4 (ADR-0016) account-pubkey PIN store, the accepted list, and the
 * rule that one browser's accounts never see each other's contacts.
 *
 * Runs under the default vitest `node` environment, which has no
 * `localStorage`; we install a minimal in-memory stub per test so
 * the store's `typeof localStorage` guard sees a real backend.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

/** Minimal synchronous in-memory `localStorage` Web Storage
 *  shim — enough for the store's getItem/setItem/removeItem use. */
class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.has(key) ? (this.map.get(key) as string) : null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, String(value));
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
  clear(): void {
    this.map.clear();
  }
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return Array.from(this.map.keys())[i] ?? null;
  }
}

/** The signed-in account most tests run as. */
const OWNER = "b3duZXItYWNjb3VudC0wMDAwMA==";

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount(OWNER);
});

afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
  // Force a fresh module instance so the store has no cross-test
  // state (the module is stateless — all reads hit localStorage —
  // but resetting modules keeps each test hermetic regardless).
});

// Import after the localStorage shim type is in place. The store
// reads `localStorage` lazily per call, so a top-level import is
// fine as long as the shim is installed in `beforeEach`.
import {
  readPin,
  removePin,
  setPin,
  loadAccepted,
  upsertAccepted,
  peerContactByMemberPubkeys,
  recordLogStatus,
  recordLogReset,
  readLogReset,
  recordLogVersion,
  readLogVersion,
  bindContactsAccount,
  boundContactsAccount,
  clearAccountContacts,
  loadRenames,
  removeAccepted,
  adoptOneToOne,
  subscribeAcceptedChanged,
  type AcceptedChange,
} from "./local-store";

const ACCT_A = "YWNjb3VudC1hLTAwMDAwMDAwMA=="; // arbitrary base64
const ACCT_B = "YWNjb3VudC1iLTAwMDAwMDAwMA==";
const PUB_OLD = "b2xkLXB1YmtleS0wMDAwMDAwMDAwMDA=";
const PUB_NEW = "bmV3LXB1YmtleS0wMDAwMDAwMDAwMDA=";

describe("account-pubkey PIN store (G-4)", () => {
  it("returns undefined for an unknown account_id", () => {
    expect(readPin(ACCT_A)).toBeUndefined();
  });

  it("persists a TOFU pin and reads it back", () => {
    const result = setPin(ACCT_A, PUB_OLD, 0);
    expect(result).toBe("set");
    const pin = readPin(ACCT_A);
    expect(pin).toBeDefined();
    expect(pin?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    expect(pin?.rotationMarker).toBe(0);
  });

  it("survives a fresh module read (localStorage-backed)", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    // A second independent read resolves the same persisted value.
    expect(readPin(ACCT_A)?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
  });

  it("treats a changed pubkey for the same account_id as a mutation, not a new contact", () => {
    setPin(ACCT_A, PUB_OLD, 100);
    // Advancing forward re-pins the SAME account_id to the new key.
    const result = setPin(ACCT_A, PUB_NEW, 200);
    expect(result).toBe("set");
    const pin = readPin(ACCT_A);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(PUB_NEW);
    expect(pin?.rotationMarker).toBe(200);
    // Still exactly one account_id key — not forked into two.
    expect(readPin(ACCT_B)).toBeUndefined();
  });

  it("allows an idempotent re-pin at the same marker", () => {
    setPin(ACCT_A, PUB_OLD, 500);
    const result = setPin(ACCT_A, PUB_NEW, 500);
    expect(result).toBe("set");
    expect(readPin(ACCT_A)?.pinnedSignaturePubkeyB64).toBe(PUB_NEW);
    expect(readPin(ACCT_A)?.rotationMarker).toBe(500);
  });

  it("refuses to lower the rotation marker (monotonic, rule #4)", () => {
    setPin(ACCT_A, PUB_OLD, 1000);
    const result = setPin(ACCT_A, PUB_NEW, 999);
    expect(result).toBe("rejected-backward");
    // The original pin is untouched — no backward re-pin.
    const pin = readPin(ACCT_A);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    expect(pin?.rotationMarker).toBe(1000);
  });

  it("keeps pins independent across distinct account_ids", () => {
    setPin(ACCT_A, PUB_OLD, 10);
    setPin(ACCT_B, PUB_NEW, 20);
    expect(readPin(ACCT_A)?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    expect(readPin(ACCT_B)?.pinnedSignaturePubkeyB64).toBe(PUB_NEW);
  });

  it("removePin drops the pin", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    removePin(ACCT_A);
    expect(readPin(ACCT_A)).toBeUndefined();
  });
});

describe("KT-log S4 — isReset marker on the pin record", () => {
  it("recordLogReset is a no-op when no pin exists", () => {
    expect(recordLogReset(ACCT_A, true)).toBe(false);
    expect(readLogReset(ACCT_A)).toBeUndefined();
  });

  it("records true/false onto an existing pin and reads it back", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    expect(recordLogReset(ACCT_A, true)).toBe(true);
    expect(readLogReset(ACCT_A)).toBe(true);
    expect(recordLogReset(ACCT_A, false)).toBe(true);
    expect(readLogReset(ACCT_A)).toBe(false);
  });

  it("a null marker CLEARS a previously-recorded value", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    recordLogReset(ACCT_A, true);
    expect(readLogReset(ACCT_A)).toBe(true);
    // null = no latest entry read → clear the stale marker.
    expect(recordLogReset(ACCT_A, null)).toBe(true);
    expect(readLogReset(ACCT_A)).toBeUndefined();
  });

  it("carries the isReset marker across a forward re-pin", () => {
    setPin(ACCT_A, PUB_OLD, 100);
    recordLogReset(ACCT_A, true);
    // A monotonic forward re-pin keeps the recorded marker.
    expect(setPin(ACCT_A, PUB_NEW, 200)).toBe("set");
    expect(readLogReset(ACCT_A)).toBe(true);
  });

  it("isReset and logStatus are independent fields on the same pin", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    recordLogStatus(ACCT_A, "confirmed");
    recordLogReset(ACCT_A, true);
    const pin = readPin(ACCT_A);
    expect(pin?.logStatus).toBe("confirmed");
    expect(pin?.isReset).toBe(true);
  });

  it("drops a junk (non-boolean) isReset on read (forward-compat guard)", () => {
    // Hand-craft a v2 row with a corrupt isReset value.
    localStorage.setItem(
      `lakko.contacts.v3.${OWNER}`,
      JSON.stringify({
        v: 3,
        accepted: [],
        pins: {
          [ACCT_A]: {
            pinnedSignaturePubkeyB64: PUB_OLD,
            rotationMarker: 0,
            isReset: "yes-please", // junk
          },
        },
      }),
    );
    const pin = readPin(ACCT_A);
    // The pin survives (structural fields valid); the junk marker is dropped.
    expect(pin?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    expect(pin?.isReset).toBeUndefined();
  });
});

describe("G-30 #2 — pinnedLogVersion on the pin record", () => {
  it("recordLogVersion is a no-op when no pin exists", () => {
    expect(recordLogVersion(ACCT_A, 5)).toBe(false);
    expect(readLogVersion(ACCT_A)).toBeUndefined();
  });

  it("records a version onto an existing pin and reads it back", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    expect(recordLogVersion(ACCT_A, 7)).toBe(true);
    expect(readLogVersion(ACCT_A)).toBe(7);
    // Advancing to a newer version overwrites.
    expect(recordLogVersion(ACCT_A, 9)).toBe(true);
    expect(readLogVersion(ACCT_A)).toBe(9);
  });

  it("a null argument CLEARS a previously-recorded version", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    recordLogVersion(ACCT_A, 7);
    expect(readLogVersion(ACCT_A)).toBe(7);
    expect(recordLogVersion(ACCT_A, null)).toBe(true);
    expect(readLogVersion(ACCT_A)).toBeUndefined();
  });

  it("is idempotent on an unchanged version (no write)", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    recordLogVersion(ACCT_A, 3);
    expect(recordLogVersion(ACCT_A, 3)).toBe(true);
    expect(readLogVersion(ACCT_A)).toBe(3);
    // Clearing when already clear is a no-op too.
    recordLogVersion(ACCT_A, null);
    expect(recordLogVersion(ACCT_A, null)).toBe(true);
    expect(readLogVersion(ACCT_A)).toBeUndefined();
  });

  it("carries the version across a forward re-pin", () => {
    setPin(ACCT_A, PUB_OLD, 100);
    recordLogVersion(ACCT_A, 4);
    // A monotonic forward re-pin keeps the recorded version.
    expect(setPin(ACCT_A, PUB_NEW, 200)).toBe("set");
    expect(readLogVersion(ACCT_A)).toBe(4);
  });

  it("pinnedLogVersion is independent of logStatus + isReset", () => {
    setPin(ACCT_A, PUB_OLD, 0);
    recordLogStatus(ACCT_A, "confirmed");
    recordLogReset(ACCT_A, false);
    recordLogVersion(ACCT_A, 11);
    const pin = readPin(ACCT_A);
    expect(pin?.logStatus).toBe("confirmed");
    expect(pin?.isReset).toBe(false);
    expect(pin?.pinnedLogVersion).toBe(11);
  });

  it("drops a junk (non-finite / negative / non-number) pinnedLogVersion on read", () => {
    localStorage.setItem(
      `lakko.contacts.v3.${OWNER}`,
      JSON.stringify({
        v: 3,
        accepted: [],
        pins: {
          [ACCT_A]: {
            pinnedSignaturePubkeyB64: PUB_OLD,
            rotationMarker: 0,
            pinnedLogVersion: -1, // junk (negative)
          },
          [ACCT_B]: {
            pinnedSignaturePubkeyB64: PUB_NEW,
            rotationMarker: 0,
            pinnedLogVersion: "lots", // junk (non-number)
          },
        },
      }),
    );
    // The pins survive (structural fields valid); the junk version is dropped.
    expect(readPin(ACCT_A)?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    expect(readLogVersion(ACCT_A)).toBeUndefined();
    expect(readPin(ACCT_B)?.pinnedSignaturePubkeyB64).toBe(PUB_NEW);
    expect(readLogVersion(ACCT_B)).toBeUndefined();
  });
});

describe("the contact list belongs to the signed-in account", () => {
  const ACCOUNT_CARA = "Y2FyYS1hY2NvdW50LTAwMDAwMA==";
  const ACCOUNT_EVE = "ZXZlLWFjY291bnQtMDAwMDAwMA==";
  const dan = { signaturePubkeyB64: PUB_OLD, displayName: "Dan", acceptedAtMs: 1 };
  const fay = { signaturePubkeyB64: PUB_NEW, displayName: "", acceptedAtMs: 2 };

  it("two accounts on one browser never see each other's contacts, renames or pins", () => {
    bindContactsAccount(ACCOUNT_CARA);
    upsertAccepted(dan);
    setPin(ACCT_A, PUB_OLD, 7);
    recordLogStatus(ACCT_A, "confirmed");

    bindContactsAccount(ACCOUNT_EVE);
    expect(loadAccepted()).toEqual([]);
    expect(loadRenames().size).toBe(0);
    expect(readPin(ACCT_A)).toBeUndefined();
    upsertAccepted(fay);
    setPin(ACCT_A, PUB_NEW, 1);

    bindContactsAccount(ACCOUNT_CARA);
    expect(loadAccepted().map((c) => c.displayName)).toEqual(["Dan"]);
    expect(loadRenames().get(PUB_OLD)).toBe("Dan");
    expect(readPin(ACCT_A)?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    expect(readPin(ACCT_A)?.rotationMarker).toBe(7);

    bindContactsAccount(ACCOUNT_EVE);
    expect(loadAccepted().map((c) => c.signaturePubkeyB64)).toEqual([PUB_NEW]);
    expect(readPin(ACCT_A)?.pinnedSignaturePubkeyB64).toBe(PUB_NEW);
    // Eve's own pin is not Cara's: a lower marker here is not refused.
    expect(readPin(ACCT_A)?.rotationMarker).toBe(1);
  });

  it("an explicit owner reads and writes that account whatever is bound", () => {
    bindContactsAccount(ACCOUNT_EVE);
    upsertAccepted(dan, ACCOUNT_CARA);
    expect(loadAccepted()).toEqual([]);
    expect(loadAccepted(ACCOUNT_CARA)).toHaveLength(1);
    expect(setPin(ACCT_A, PUB_OLD, 0, ACCOUNT_CARA)).toBe("set");
    expect(readPin(ACCT_A)).toBeUndefined();
    expect(readPin(ACCT_A, ACCOUNT_CARA)?.pinnedSignaturePubkeyB64).toBe(PUB_OLD);
    removeAccepted(PUB_OLD, ACCOUNT_CARA);
    expect(loadAccepted(ACCOUNT_CARA)).toEqual([]);
  });

  it("a removed contact is not brought back by their one-to-one; an explicit add brings them back", () => {
    bindContactsAccount(ACCOUNT_CARA);
    upsertAccepted({ ...dan, groupIdB64: "Z3JvdXA=" });
    removeAccepted(PUB_OLD);
    expect(loadAccepted()).toEqual([]);
    // The conversation is still there; the next load runs the adoption rule.
    const peer = { signatureKey: PUB_OLD, groupIdB64: "Z3JvdXA=", nowMs: 10 };
    expect(adoptOneToOne(peer)).toBe(false);
    expect(loadAccepted()).toEqual([]);
    // Adding them again (Accept, a link, starting a chat) is the person's own act.
    upsertAccepted(dan);
    expect(loadAccepted().map((c) => c.signaturePubkeyB64)).toEqual([PUB_OLD]);
    // And the rule may complete that row again (its conversation).
    expect(adoptOneToOne(peer)).toBe(true);
    expect(loadAccepted()[0]?.groupIdB64).toBe("Z3JvdXA=");
  });

  it("with no account bound a read is empty and a write writes nothing", () => {
    upsertAccepted(dan);
    setPin(ACCT_A, PUB_OLD, 0);
    bindContactsAccount(null);
    expect(boundContactsAccount()).toBeNull();
    expect(loadAccepted()).toEqual([]);
    expect(readPin(ACCT_A)).toBeUndefined();
    expect(upsertAccepted(fay)).toEqual([]);
    expect(setPin(ACCT_B, PUB_NEW, 0)).toBe("no-account");
    expect(recordLogStatus(ACCT_A, "confirmed")).toBe(false);
    // Nothing reached a shared row: the only row is the owner's.
    const keys = Array.from({ length: localStorage.length }, (_, i) => localStorage.key(i));
    expect(keys).toEqual([`lakko.contacts.v3.${OWNER}`]);
    bindContactsAccount(OWNER);
    expect(loadAccepted().map((c) => c.signaturePubkeyB64)).toEqual([PUB_OLD]);
    expect(readPin(ACCT_B)).toBeUndefined();
  });

  it("a read before any binding is empty, even with rows on the browser", () => {
    upsertAccepted(dan);
    bindContactsAccount(null);
    expect(loadAccepted()).toEqual([]);
    expect(loadRenames().size).toBe(0);
  });

  it("the retired device-wide row is never read", () => {
    localStorage.setItem(
      "lakko.contacts.v2",
      JSON.stringify({ v: 2, cursor: 0, accepted: [dan], pins: {} }),
    );
    expect(loadAccepted()).toEqual([]);
  });

  it("forget device clears only the current account, and the retired rows", () => {
    bindContactsAccount(ACCOUNT_CARA);
    upsertAccepted(dan);
    setPin(ACCT_A, PUB_OLD, 0);
    bindContactsAccount(ACCOUNT_EVE);
    upsertAccepted(fay);
    localStorage.setItem("lakko.contacts.v2", "{}");
    localStorage.setItem("lakko.contacts.v1", "{}");

    clearAccountContacts();
    expect(loadAccepted()).toEqual([]);
    expect(loadAccepted(ACCOUNT_CARA)).toHaveLength(1);
    expect(readPin(ACCT_A, ACCOUNT_CARA)).toBeDefined();
    expect(localStorage.getItem("lakko.contacts.v2")).toBeNull();
    expect(localStorage.getItem("lakko.contacts.v1")).toBeNull();

    clearAccountContacts(ACCOUNT_CARA);
    expect(loadAccepted(ACCOUNT_CARA)).toEqual([]);
  });

  it("a log out keeps the list for the next sign-in of the same account", () => {
    upsertAccepted(dan);
    bindContactsAccount(null);
    bindContactsAccount(OWNER);
    expect(loadAccepted().map((c) => c.displayName)).toEqual(["Dan"]);
  });

  it("listeners hear an account change apart from an edit", () => {
    const seen: AcceptedChange[] = [];
    const unsubscribe = subscribeAcceptedChanged((change) => seen.push(change));
    upsertAccepted(dan);
    bindContactsAccount(ACCOUNT_EVE);
    bindContactsAccount(ACCOUNT_EVE); // no change, no notice
    bindContactsAccount(null);
    unsubscribe();
    expect(seen).toEqual(["edited", "account-changed", "account-changed"]);
  });
});

describe("AcceptedContact accountIdHex (ADR-0021)", () => {
  it("round-trips accountIdHex through upsert + load", () => {
    upsertAccepted({
      signaturePubkeyB64: PUB_OLD,
      displayName: "alice",
      acceptedAtMs: 1,
      accountIdHex: "deadbeefdeadbeefdeadbeefdeadbeef",
    });
    const back = loadAccepted();
    expect(back).toHaveLength(1);
    expect(back[0]?.accountIdHex).toBe("deadbeefdeadbeefdeadbeefdeadbeef");
  });

  it("tolerates a contact without accountIdHex (pre-ADR-0021)", () => {
    upsertAccepted({
      signaturePubkeyB64: PUB_NEW,
      displayName: "bob",
      acceptedAtMs: 2,
    });
    const back = loadAccepted();
    expect(back).toHaveLength(1);
    expect(back[0]?.accountIdHex).toBeUndefined();
  });
});

describe("AcceptedContact groupIdB64 + rename (ADR-0021 follow-up)", () => {
  it("round-trips groupIdB64 through upsert + load", () => {
    upsertAccepted({
      signaturePubkeyB64: PUB_OLD,
      displayName: "alice",
      acceptedAtMs: 1,
      groupIdB64: "Z3JvdXAtaWQtYjY0",
    });
    const back = loadAccepted();
    expect(back).toHaveLength(1);
    expect(back[0]?.groupIdB64).toBe("Z3JvdXAtaWQtYjY0");
  });

  it("tolerates a contact without groupIdB64 (Welcome-joined chat)", () => {
    upsertAccepted({
      signaturePubkeyB64: PUB_NEW,
      displayName: "bob",
      acceptedAtMs: 2,
    });
    const back = loadAccepted();
    expect(back).toHaveLength(1);
    expect(back[0]?.groupIdB64).toBeUndefined();
  });

  it("a rename replaces displayName by signature pubkey, preserving the link", () => {
    upsertAccepted({
      signaturePubkeyB64: PUB_OLD,
      displayName: "@abcd1234",
      acceptedAtMs: 1,
      accountIdHex: "deadbeefdeadbeefdeadbeefdeadbeef",
      groupIdB64: "Z3JvdXAtaWQtYjY0",
    });
    // Simulate the rename handler: re-upsert by the same pubkey with a
    // new displayName, keeping the other fields.
    const existing = loadAccepted()[0];
    expect(existing).toBeDefined();
    if (existing === undefined) return;
    upsertAccepted({ ...existing, displayName: "Alice" });
    const back = loadAccepted();
    expect(back).toHaveLength(1);
    expect(back[0]?.displayName).toBe("Alice");
    // The chat link + account_id survive the rename.
    expect(back[0]?.groupIdB64).toBe("Z3JvdXAtaWQtYjY0");
    expect(back[0]?.accountIdHex).toBe("deadbeefdeadbeefdeadbeefdeadbeef");
  });
});

describe("peerContactByMemberPubkeys (ADR-0021 universal 1×1 resolver)", () => {
  const alice = {
    signaturePubkeyB64: PUB_OLD,
    displayName: "Alice",
    acceptedAtMs: 1,
  };
  const bob = {
    signaturePubkeyB64: PUB_NEW,
    displayName: "Bob",
    acceptedAtMs: 2,
  };

  it("resolves the non-self member to its contact", () => {
    // Members = [me (ownB64), Alice]; the non-self member is a known contact.
    const got = peerContactByMemberPubkeys(["bXktb3duLXB1YmtleQ==", PUB_OLD], "bXktb3duLXB1YmtleQ==", [
      alice,
      bob,
    ]);
    expect(got?.displayName).toBe("Alice");
  });

  it("skips the self member even when self is also a (stale) contact", () => {
    // Own pubkey appears first; it must be skipped, returning the peer.
    const got = peerContactByMemberPubkeys([PUB_NEW, PUB_OLD], PUB_NEW, [alice, bob]);
    expect(got?.displayName).toBe("Alice");
  });

  it("returns undefined when no non-self member is a known contact", () => {
    const got = peerContactByMemberPubkeys(["bXktb3duLXB1YmtleQ==", "c3RyYW5nZXItcHVia2V5"], "bXktb3duLXB1YmtleQ==", [
      alice,
      bob,
    ]);
    expect(got).toBeUndefined();
  });

  it("returns undefined when the only member is self", () => {
    const got = peerContactByMemberPubkeys([PUB_OLD], PUB_OLD, [alice, bob]);
    expect(got).toBeUndefined();
  });
});
