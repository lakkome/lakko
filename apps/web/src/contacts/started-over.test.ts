// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/identity-recovery.md` 16.11 - what is shown when a contact
 * started over (`started-over.ts`), and what the contacts store keeps for it
 * (`local-store.ts` `recordStartedOver` / `adoptStartedOver`).
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  closableSurface,
  requestCarriesStartedOverNote,
  sentRequestCarriesStartedOverNote,
  startedOverClosesConversation,
  withoutAddForStartedOver,
} from "./started-over";
import {
  acceptReset,
  accountIdB64OfHex,
  adoptStartedOver,
  bindContactsAccount,
  loadAccepted,
  loadKeyAliases,
  loadStartedOverKeys,
  loadSupersededKeys,
  acceptedResetOfKey,
  moveStartedOverKey,
  raiseSealSeqFloor,
  readPin,
  recordStartedOver,
  sealSeqFloorOf,
  setPin,
  startedOverOf,
  upsertAccepted,
} from "./local-store";
import { findPersonRecord } from "../chat/conversation-info";

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.has(k) ? (this.map.get(k) as string) : null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, String(v));
  }
  removeItem(k: string): void {
    this.map.delete(k);
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

const OWNER = "b3duZXItYWNjb3VudC0wMDAwMA==";
const ANNA_HEX = "0123456789abcdef0123456789abcdef";
const ANNA = accountIdB64OfHex(ANNA_HEX) as string;
const OLD = "b2xkLWtleQ==";
const OLDER = "b2xkZXIta2V5";
const NEW = "bmV3LWtleQ==";
const NEWER = "bmV3ZXIta2V5";

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount(OWNER);
});
afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

function unsealed(over: Partial<Parameters<typeof recordStartedOver>[0]> = {}) {
  return recordStartedOver({
    accountIdB64: ANNA,
    heldKeyB64: OLD,
    newKeyB64: NEW,
    keyGeneration: 2,
    signedAtMs: 1_700_000_000_000,
    nowMs: 1_700_000_500_000,
    ...over,
  });
}

describe("startedOverClosesConversation (the OLD one-to-one)", () => {
  const keys = new Set([OLD]);
  it("closes the one-to-one whose other person is bound to a key the account left behind", () => {
    expect(
      startedOverClosesConversation({ isOneToOne: true, peerKeyB64: OLD, startedOverKeysB64: keys }),
    ).toBe(true);
  });
  it("a NEW one-to-one with the same account is bound to the new key: open", () => {
    expect(
      startedOverClosesConversation({ isOneToOne: true, peerKeyB64: NEW, startedOverKeysB64: keys }),
    ).toBe(false);
  });
  it("a group is never closed by it, whoever is in it", () => {
    expect(
      startedOverClosesConversation({ isOneToOne: false, peerKeyB64: OLD, startedOverKeysB64: keys }),
    ).toBe(false);
  });
  it("a roster that does not name exactly one other person decides nothing", () => {
    expect(
      startedOverClosesConversation({ isOneToOne: true, peerKeyB64: null, startedOverKeysB64: keys }),
    ).toBe(false);
  });
  it("reads the store's map as well as a set", () => {
    expect(
      startedOverClosesConversation({
        isOneToOne: true,
        peerKeyB64: OLD,
        startedOverKeysB64: new Map([[OLD, ANNA]]),
      }),
    ).toBe(true);
  });
});

describe("closableSurface", () => {
  it("an ordinary one-to-one and every group offer everything", () => {
    for (const s of [
      closableSurface({ isOneToOne: true, peerDeleted: false, peerStartedOver: false }),
      closableSurface({ isOneToOne: false, peerDeleted: true, peerStartedOver: true }),
    ]) {
      expect(s).toEqual({
        composer: true,
        callButtons: true,
        reply: true,
        react: true,
        ephemeral: true,
        closedLine: null,
      });
    }
  });
  it("a started-over person's old one-to-one: no composer, no call buttons, nothing sent", () => {
    expect(closableSurface({ isOneToOne: true, peerDeleted: false, peerStartedOver: true })).toEqual({
      composer: false,
      callButtons: false,
      reply: false,
      react: false,
      ephemeral: false,
      closedLine: "started-over",
    });
  });
  it("a deleted account's line outranks the started-over one", () => {
    expect(closableSurface({ isOneToOne: true, peerDeleted: true, peerStartedOver: true }).closedLine).toBe(
      "deleted",
    );
  });
});

describe("requestCarriesStartedOverNote", () => {
  it("only a request signed by the key the new account publishes carries the note", () => {
    expect(requestCarriesStartedOverNote({ requestKeyB64: NEW, pendingNewKeyB64: NEW })).toBe(true);
    expect(requestCarriesStartedOverNote({ requestKeyB64: NEWER, pendingNewKeyB64: NEW })).toBe(false);
  });
  it("a stranger, an ordinary contact and an already accepted new account carry none", () => {
    expect(requestCarriesStartedOverNote({ requestKeyB64: NEW, pendingNewKeyB64: null })).toBe(false);
  });
});

describe("sentRequestCarriesStartedOverNote (item D)", () => {
  it("a request this device sent to a key the account did not leave behind carries the note", () => {
    expect(sentRequestCarriesStartedOverNote({ requestKeyB64: NEW, leftBehindKeysB64: [OLD] })).toBe(true);
    expect(sentRequestCarriesStartedOverNote({ requestKeyB64: OLD, leftBehindKeysB64: [OLD] })).toBe(false);
    expect(sentRequestCarriesStartedOverNote({ requestKeyB64: NEW, leftBehindKeysB64: [] })).toBe(false);
    expect(sentRequestCarriesStartedOverNote({ requestKeyB64: null, leftBehindKeysB64: [OLD] })).toBe(false);
  });
});

describe("recordStartedOver (the one write of an unsealed verdict)", () => {
  it("keeps the pin on the old key and records the fact beside it", () => {
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    expect(unsealed()).toBe("recorded");
    const pin = readPin(ANNA);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(OLD);
    expect(pin?.acceptedKeyGeneration).toBe(1);
    expect(startedOverOf(ANNA)).toEqual({
      newKeyB64: NEW,
      keyGeneration: 2,
      signedAtMs: 1_700_000_000_000,
      noticedAtMs: 1_700_000_500_000,
    });
    expect(loadStartedOverKeys()).toEqual(new Map([[OLD, ANNA]]));
  });

  it("writes NOTHING an accepted reset writes: no superseded key, no reset time", () => {
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    unsealed();
    expect(loadSupersededKeys().size).toBe(0);
    // What a Welcome to a reset identity derives its continuity key from.
    expect(acceptedResetOfKey(NEW)).toBeNull();
    expect(acceptedResetOfKey(OLD)).toBeNull();
    expect(readPin(ANNA)?.resetSignedAtMs).toBeUndefined();
  });

  it("the account's proven predecessors are left behind too (the leaves may be bound to one)", () => {
    setPin(ANNA, OLDER, 0, undefined, { keyGeneration: 1 });
    setPin(ANNA, OLD, 5, undefined, { proven: true });
    unsealed({ heldPredecessorsB64: ["Y29udi1rZXk="] });
    expect([...loadStartedOverKeys().keys()].sort()).toEqual([OLD, OLDER, "Y29udi1rZXk="].sort());
  });

  it("the same reset again writes nothing and keeps when it was first noticed", () => {
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    unsealed();
    expect(unsealed({ nowMs: 1_800_000_000_000 })).toBe("already");
    expect(startedOverOf(ANNA)?.noticedAtMs).toBe(1_700_000_500_000);
  });

  it("is refused for an account that is not pinned under the held key", () => {
    expect(unsealed()).toBe("no-pin");
    setPin(ANNA, OLDER, 0);
    expect(unsealed()).toBe("no-pin");
    expect(startedOverOf(ANNA)).toBeUndefined();
  });

  it("survives a later re-pin of the same key (the store rebuilds the pin)", () => {
    setPin(ANNA, OLD, 0);
    unsealed();
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    expect(startedOverOf(ANNA)?.newKeyB64).toBe(NEW);
    expect(loadStartedOverKeys().get(OLD)).toBe(ANNA);
  });

  it("follows the new account's own proven rotation", () => {
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    unsealed();
    expect(moveStartedOverKey(ANNA, "c29tZS1vdGhlcg==", NEWER)).toBe(false);
    expect(moveStartedOverKey(ANNA, NEW, NEWER)).toBe(true);
    expect(startedOverOf(ANNA)?.newKeyB64).toBe(NEWER);
    expect(readPin(ANNA)?.pinnedSignaturePubkeyB64).toBe(OLD);
  });
});

describe("adoptStartedOver (the person accepted the new account)", () => {
  beforeEach(() => {
    setPin(ANNA, OLD, 7, undefined, { keyGeneration: 1 });
    raiseSealSeqFloor(ANNA, 9n);
    upsertAccepted({
      signaturePubkeyB64: OLD,
      displayName: "Anna from work",
      acceptedAtMs: 1,
      accountIdHex: ANNA_HEX,
      groupIdB64: "b2xkLWNoYXQ=",
      x25519PubkeyB64: "b2xkLXgyNTUxOQ==",
    });
    unsealed();
  });

  it("moves the pin only for exactly the recorded key", () => {
    expect(adoptStartedOver(ANNA, NEWER, 2_000_000)).toBe("none");
    expect(readPin(ANNA)?.pinnedSignaturePubkeyB64).toBe(OLD);
    expect(adoptStartedOver(ANNA, NEW, 2_000_000)).toBe("adopted");
    expect(readPin(ANNA)?.pinnedSignaturePubkeyB64).toBe(NEW);
    // Decided once: a second call has no record to act on.
    expect(adoptStartedOver(ANNA, NEW, 2_000_000)).toBe("none");
  });

  it("records the generation, never lowers the marker, and retires nothing", () => {
    adoptStartedOver(ANNA, NEW, 2_000_000);
    const pin = readPin(ANNA);
    expect(pin?.acceptedKeyGeneration).toBe(2);
    expect(pin?.rotationMarker).toBe(2000);
    expect(pin?.startedOver).toBeUndefined();
    expect(pin?.supersededKeysB64).toBeUndefined();
    expect(pin?.resetSignedAtMs).toBeUndefined();
    expect(pin?.provenPredecessorsB64).toBeUndefined();
    expect(loadSupersededKeys().size).toBe(0);
    expect(acceptedResetOfKey(NEW)).toBeNull();
    // The floor described the old account's announcements.
    expect(sealSeqFloorOf(ANNA)).toBe(0n);
  });

  it("the old conversation stays closed and keeps the person's name", () => {
    adoptStartedOver(ANNA, NEW, 2_000_000);
    expect(loadStartedOverKeys().get(OLD)).toBe(ANNA);
    // The old key is still its own row's: never an alias of the new account.
    expect(loadKeyAliases().has(OLD)).toBe(false);
    upsertAccepted({ signaturePubkeyB64: NEW, displayName: "", acceptedAtMs: 2, accountIdHex: ANNA_HEX });
    expect(loadKeyAliases().has(OLD)).toBe(false);
  });

  it("the old contact row is not touched; the new account gets a row of its own (walk of 2026-11-28)", () => {
    adoptStartedOver(ANNA, NEW, 2_000_000);
    expect(loadAccepted()).toEqual([
      {
        signaturePubkeyB64: OLD,
        displayName: "Anna from work",
        acceptedAtMs: 1,
        accountIdHex: ANNA_HEX,
        groupIdB64: "b2xkLWNoYXQ=",
        x25519PubkeyB64: "b2xkLXgyNTUxOQ==",
      },
    ]);
    // The accept writes the new account's row: never merged into the old one.
    const rows = upsertAccepted({
      signaturePubkeyB64: NEW,
      displayName: "",
      acceptedAtMs: 2,
      accountIdHex: ANNA_HEX,
      groupIdB64: "bmV3LWNoYXQ=",
    });
    expect(rows.map((r) => [r.signaturePubkeyB64, r.groupIdB64, r.displayName])).toEqual([
      [OLD, "b2xkLWNoYXQ=", "Anna from work"],
      [NEW, "bmV3LWNoYXQ=", ""],
    ]);
    expect(rows[1]?.previousSignaturePubkeysB64 ?? []).not.toContain(OLD);
  });
});

describe("one safety number per conversation (UX 16.11, last bullet)", () => {
  it("the old one-to-one's card is over the old key, the contact (and the new chat) over the new one", () => {
    setPin(ANNA, OLD, 7, undefined, { keyGeneration: 1 });
    upsertAccepted({
      signaturePubkeyB64: OLD,
      displayName: "Anna",
      acceptedAtMs: 1,
      accountIdHex: ANNA_HEX,
      groupIdB64: "b2xkLWNoYXQ=",
    });
    unsealed();
    // Before the new account is accepted, the only conversation is the old
    // one and its card is the contact's, over the old key.
    expect(findPersonRecord(loadAccepted(), { groupIdB64: "b2xkLWNoYXQ=", peerKeyB64: OLD })?.signaturePubkeyB64).toBe(OLD);
    adoptStartedOver(ANNA, NEW, 2_000_000);
    upsertAccepted({
      signaturePubkeyB64: NEW,
      displayName: "",
      acceptedAtMs: 2,
      accountIdHex: ANNA_HEX,
      groupIdB64: "bmV3LWNoYXQ=",
    });
    // The old one-to-one keeps its own contact row, over the old key, so its
    // number is the old number.
    expect(findPersonRecord(loadAccepted(), { groupIdB64: "b2xkLWNoYXQ=", peerKeyB64: OLD })?.signaturePubkeyB64).toBe(OLD);
    // The new one-to-one is bound to the new key: the contact, new number.
    expect(findPersonRecord(loadAccepted(), { groupIdB64: "bmV3LWNoYXQ=", peerKeyB64: NEW })?.signaturePubkeyB64).toBe(NEW);
  });
});

describe("the old conversation's card offers no Add to contacts (UX 16.11)", () => {
  it("a key the account left behind loses the add row; any other card keeps it", () => {
    setPin(ANNA, OLD, 7, undefined, { keyGeneration: 1 });
    unsealed();
    adoptStartedOver(ANNA, NEW, 2_000_000);
    const sections = ["message", "add-contact", "block", "report"] as const;
    expect(withoutAddForStartedOver(sections, loadStartedOverKeys().has(OLD))).toEqual(["message", "block", "report"]);
    expect(withoutAddForStartedOver(sections, loadStartedOverKeys().has(NEW))).toEqual([...sections]);
  });
});

describe("an unsealed reset AFTER a sealed one this device accepted", () => {
  it("the new account never reads as the product of the earlier accepted reset", () => {
    setPin(ANNA, OLDER, 0, undefined, { keyGeneration: 1 });
    acceptReset({
      accountIdB64: ANNA,
      heldKeyB64: OLDER,
      newKeyB64: OLD,
      keyGeneration: 2,
      signedAtMs: 1_600_000_000_000,
      nowMs: 1_600_000_100_000,
    });
    expect(acceptedResetOfKey(OLD)).not.toBeNull();
    unsealed({ keyGeneration: 3 });
    // Pending: the earlier, sealed reset still stands for the old key.
    expect(acceptedResetOfKey(OLD)).not.toBeNull();
    expect(acceptedResetOfKey(NEW)).toBeNull();
    adoptStartedOver(ANNA, NEW, 1_700_001_000_000);
    // Adopted: nothing an accepted reset wrote is left to act on, so no
    // continuity key, history answer or channel return can run for NEW.
    expect(acceptedResetOfKey(NEW)).toBeNull();
    expect(acceptedResetOfKey(OLD)).toBeNull();
    expect(loadSupersededKeys().size).toBe(0);
    // Every earlier self's conversation is closed, none is moved.
    expect([...loadStartedOverKeys().keys()].sort()).toEqual([OLD, OLDER].sort());
    expect(readPin(ANNA)?.acceptedKeyGeneration).toBe(3);
  });
});

describe("a reset that proves sealed after it read as unsealed", () => {
  it("acceptReset clears the pending record and the left-behind keys it retires", () => {
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    unsealed();
    expect(
      acceptReset({
        accountIdB64: ANNA,
        heldKeyB64: OLD,
        newKeyB64: NEW,
        keyGeneration: 2,
        signedAtMs: 1_700_000_000_000,
        nowMs: 1_700_000_900_000,
      }),
    ).toBe("accepted");
    expect(startedOverOf(ANNA)).toBeUndefined();
    expect(loadStartedOverKeys().size).toBe(0);
    expect(loadSupersededKeys().get(OLD)).toBe(ANNA);
  });
});

describe("the seal seq floor", () => {
  it("is 0 for an account that is not pinned, and cannot be raised without a pin", () => {
    expect(sealSeqFloorOf(ANNA)).toBe(0n);
    expect(raiseSealSeqFloor(ANNA, 5n)).toBe(false);
    expect(sealSeqFloorOf(ANNA)).toBe(0n);
  });
  it("only ever goes up, and holds a full u64", () => {
    setPin(ANNA, OLD, 0);
    expect(raiseSealSeqFloor(ANNA, 5n)).toBe(true);
    expect(raiseSealSeqFloor(ANNA, 5n)).toBe(false);
    expect(raiseSealSeqFloor(ANNA, 3n)).toBe(false);
    expect(sealSeqFloorOf(ANNA)).toBe(5n);
    const big = 18_446_744_073_709_551_615n;
    expect(raiseSealSeqFloor(ANNA, big)).toBe(true);
    expect(sealSeqFloorOf(ANNA)).toBe(big);
  });
  it("travels across a re-pin and an accepted reset", () => {
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 1 });
    raiseSealSeqFloor(ANNA, 6n);
    setPin(ANNA, NEW, 9, undefined, { proven: true, provenGeneration: 2 });
    expect(sealSeqFloorOf(ANNA)).toBe(6n);
    expect(readPin(ANNA)?.acceptedKeyGeneration).toBe(2);
    acceptReset({
      accountIdB64: ANNA,
      heldKeyB64: NEW,
      newKeyB64: NEWER,
      keyGeneration: 3,
      signedAtMs: 1,
      nowMs: 20_000,
    });
    expect(sealSeqFloorOf(ANNA)).toBe(6n);
  });
  it("a junk stored floor reads as 0", () => {
    setPin(ANNA, OLD, 0);
    const raw = JSON.parse(localStorage.getItem(Object.keys(localStorageDump())[0] as string) as string) as {
      pins: Record<string, { sealSeqFloor?: unknown }>;
    };
    (raw.pins[ANNA] as { sealSeqFloor?: unknown }).sealSeqFloor = "-4";
    localStorage.setItem(Object.keys(localStorageDump())[0] as string, JSON.stringify(raw));
    expect(sealSeqFloorOf(ANNA)).toBe(0n);
  });
  it("provenGeneration is read only with a proven re-pin, and never lowers", () => {
    setPin(ANNA, OLD, 0, undefined, { keyGeneration: 4 });
    setPin(ANNA, OLD, 0, undefined, { provenGeneration: 9 });
    expect(readPin(ANNA)?.acceptedKeyGeneration).toBe(4);
    setPin(ANNA, NEW, 1, undefined, { proven: true, provenGeneration: 2 });
    expect(readPin(ANNA)?.acceptedKeyGeneration).toBe(4);
    setPin(ANNA, NEWER, 2, undefined, { proven: true, provenGeneration: 6 });
    expect(readPin(ANNA)?.acceptedKeyGeneration).toBe(6);
  });
});

function localStorageDump(): Record<string, string> {
  const out: Record<string, string> = {};
  for (let i = 0; i < localStorage.length; i += 1) {
    const k = localStorage.key(i);
    if (k !== null) out[k] = localStorage.getItem(k) as string;
  }
  return out;
}
