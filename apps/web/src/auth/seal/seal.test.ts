// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The seal announcement and the reset seal on web (ADR-0101 section 10),
 * through the real wasm core: what the next announcement lists, where its
 * entries may be carried from, the `seq`, which secret seals a reset, and
 * that a peer's check (`checkResetSeal`) accepts exactly what this device
 * says is sealed.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../../wasm/lakko_ffi_wasm";
import { chatsKeptBy, ownSealFacts, passkeyKeepsChats } from "./own-seals";
import {
  chooseSealSecret,
  hex,
  MAX_PASSKEY_SEALS,
  nextSealEntries,
  nextSealSeq,
  readSeals,
  resetOutlook,
  sameSealEntries,
  sealFactsOf,
  signResetSeal,
  signSeals,
  type SealEntry,
  type SealSecret,
} from "./seal-announcement";
import {
  clearSealFacts,
  keptAnnouncement,
  publishSeals,
  readOwnSeals,
  type OwnBundleSeals,
  type SealPorts,
} from "./seal-runtime";

const ACCOUNT_ID = new Uint8Array(16).fill(0x0a);
const ACCOUNT_B64 = "account-a";
const PHRASE = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

const exportKey = (byte: number): Uint8Array => new Uint8Array(64).fill(byte);
const prf = (byte: number): Uint8Array => new Uint8Array(32).fill(byte);
const cred = (byte: number): Uint8Array => new Uint8Array(20).fill(byte);
const identity = (seed: number) => wasm.Identity.fromSeed(new Uint8Array(32).fill(seed));

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

function entries(changes: Parameters<typeof nextSealEntries>[2], base: readonly SealEntry[] | null = null) {
  const next = nextSealEntries(wasm, base, changes);
  if (next.kind !== "entries") throw new Error(`expected entries, got ${next.kind}`);
  return next.entries;
}

function memoryStorage() {
  const map = new Map<string, string>();
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
    map,
  };
}

/** A directory that checks what the real one checks about `seq`. */
function fakeDirectory(initial: Partial<OwnBundleSeals> & { signaturePubkey: Uint8Array }) {
  const state = {
    row: {
      signaturePubkey: initial.signaturePubkey,
      keyGeneration: initial.keyGeneration ?? 1,
      resetSeals: initial.resetSeals ?? null,
      previousResetSeals: initial.previousResetSeals ?? null,
    } as OwnBundleSeals | null,
    uploads: [] as Uint8Array[],
    failFetch: false,
    refuseOnce: null as number | null,
    onUpload: null as ((bytes: Uint8Array) => void) | null,
  };
  const storage = memoryStorage();
  const ports: SealPorts = {
    fetchOwn: async () => {
      if (state.failFetch) throw new Error("offline");
      return state.row;
    },
    upload: async (_id, _identity, resetSeals) => {
      if (state.refuseOnce !== null) {
        const status = state.refuseOnce;
        state.refuseOnce = null;
        state.onUpload?.(resetSeals);
        throw Object.assign(new Error("refused"), { status });
      }
      state.uploads.push(resetSeals);
      if (state.row !== null) state.row = { ...state.row, resetSeals };
    },
    storage,
    nowMs: () => 1_700_000_000_000,
  };
  return { state, ports, storage };
}

describe("the next announcement", () => {
  it("lists the password of the export key held now", () => {
    const list = entries({ passwordExportKey: exportKey(1) });
    expect(list.map((e) => e.kind)).toEqual(["password"]);
    const other = entries({ passwordExportKey: exportKey(2) });
    expect(hex(list[0]!.sealPk)).not.toBe(hex(other[0]!.sealPk));
  });

  it("publishes nothing when no password entry can be listed", () => {
    expect(nextSealEntries(wasm, null, { savePhrase: PHRASE }).kind).toBe("no-password");
    expect(nextSealEntries(wasm, null, {}).kind).toBe("no-password");
  });

  it("carries every entry it cannot speak for and replaces the ones it can", () => {
    const base = entries(
      { addPasskey: { credentialId: cred(7), prfOutput: prf(7) }, savePhrase: PHRASE },
      entries({ passwordExportKey: exportKey(1) }),
    );
    expect(base.map((e) => e.kind)).toEqual(["password", "passkey", "phrase"]);
    const changed = entries({ passwordExportKey: exportKey(2) }, base);
    expect(changed.map((e) => e.kind)).toEqual(["password", "passkey", "phrase"]);
    expect(hex(changed[0]!.sealPk)).not.toBe(hex(base[0]!.sealPk));
    expect(hex(changed[1]!.sealPk)).toBe(hex(base[1]!.sealPk));
    expect(hex(changed[2]!.sealPk)).toBe(hex(base[2]!.sealPk));
  });

  it("adds a passkey under its tag, replaces the same passkey, and removes by tag", () => {
    const base = entries({ passwordExportKey: exportKey(1) });
    const one = entries({ addPasskey: { credentialId: cred(7), prfOutput: prf(7) } }, base);
    const tag = hex(wasm.resetSealPasskeyTag(cred(7)));
    expect(one[1]).toMatchObject({ kind: "passkey", tagHex: tag });
    const again = entries({ addPasskey: { credentialId: cred(7), prfOutput: prf(8) } }, one);
    expect(again.filter((e) => e.kind === "passkey")).toHaveLength(1);
    expect(hex(again[1]!.sealPk)).not.toBe(hex(one[1]!.sealPk));
    const removed = entries({ removePasskeyTagHex: tag }, again);
    expect(removed.map((e) => e.kind)).toEqual(["password"]);
  });

  it("refuses a ninth passkey that keeps the chats", () => {
    let list = entries({ passwordExportKey: exportKey(1) });
    for (let i = 0; i < MAX_PASSKEY_SEALS; i += 1) {
      list = entries({ addPasskey: { credentialId: cred(i + 1), prfOutput: prf(i + 1) } }, list);
    }
    expect(
      nextSealEntries(wasm, list, { addPasskey: { credentialId: cred(99), prfOutput: prf(99) } }).kind,
    ).toBe("passkeys-full");
  });

  it("re-keys a listed phrase after a rotation and adds none where none was listed", () => {
    const without = entries({ passwordExportKey: exportKey(1) });
    expect(entries({ rekeyPhrase: PHRASE }, without).map((e) => e.kind)).toEqual(["password"]);
    const otherPhrase =
      "legal winner thank year wave sausage worth useful legal winner thank yellow";
    const withPhrase = entries({ savePhrase: otherPhrase }, without);
    const rekeyed = entries({ rekeyPhrase: PHRASE }, withPhrase);
    expect(rekeyed.map((e) => e.kind)).toEqual(["password", "phrase"]);
    expect(hex(rekeyed[1]!.sealPk)).not.toBe(hex(withPhrase[1]!.sealPk));
  });

  it("after a reset lists the password alone", () => {
    const base = entries(
      { addPasskey: { credentialId: cred(7), prfOutput: prf(7) }, savePhrase: PHRASE },
      entries({ passwordExportKey: exportKey(1) }),
    );
    expect(entries({ afterReset: true, passwordExportKey: exportKey(2) }, base).map((e) => e.kind)).toEqual([
      "password",
    ]);
    expect(nextSealEntries(wasm, base, { afterReset: true }).kind).toBe("no-password");
  });

  it("signs above every seq it knows", () => {
    expect(nextSealSeq([null, null, null])).toBe(1n);
    expect(nextSealSeq([4n, 9n, null])).toBe(10n);
    expect(nextSealSeq([12n, 3n, 12n])).toBe(13n);
  });
});

describe("reading an announcement", () => {
  it("opens only under the account's own key and only for this account", () => {
    const list = entries({ passwordExportKey: exportKey(1), savePhrase: PHRASE });
    const bytes = signSeals(wasm, {
      identity: identity(1),
      accountId: ACCOUNT_ID,
      keyGeneration: 1n,
      seq: 3n,
      nowMs: 1,
      entries: list,
    });
    const opened = readSeals(wasm, bytes, identity(1).signaturePublicKey, ACCOUNT_ID);
    expect(opened?.seq).toBe(3n);
    expect(opened !== null && sameSealEntries(opened.entries, list)).toBe(true);
    expect(readSeals(wasm, bytes, identity(2).signaturePublicKey, ACCOUNT_ID)).toBeNull();
    expect(readSeals(wasm, bytes, identity(1).signaturePublicKey, new Uint8Array(16).fill(0x0b))).toBeNull();
    const damaged = bytes.slice();
    damaged[damaged.length - 1] = (damaged[damaged.length - 1] ?? 0) ^ 1;
    expect(readSeals(wasm, damaged, identity(1).signaturePublicKey, ACCOUNT_ID)).toBeNull();
  });

  it("says what keeps the chats: the phrase, else a passkey, else the password, and nothing while unknown", () => {
    const password = sealFactsOf({ keyGeneration: 1n, seq: 1n, entries: entries({ passwordExportKey: exportKey(1) }) });
    expect(chatsKeptBy(password)).toBe("password");
    const passkeyList = entries(
      { addPasskey: { credentialId: cred(7), prfOutput: prf(7) } },
      entries({ passwordExportKey: exportKey(1) }),
    );
    const passkey = sealFactsOf({ keyGeneration: 1n, seq: 2n, entries: passkeyList });
    expect(chatsKeptBy(passkey)).toBe("passkey");
    expect(passkeyKeepsChats(passkey, hex(wasm.resetSealPasskeyTag(cred(7))))).toBe(true);
    expect(passkeyKeepsChats(passkey, hex(wasm.resetSealPasskeyTag(cred(8))))).toBe(false);
    const phrase = sealFactsOf({
      keyGeneration: 1n,
      seq: 3n,
      entries: entries({ savePhrase: PHRASE }, passkeyList),
    });
    expect(chatsKeptBy(phrase)).toBe("phrase");
    expect(chatsKeptBy(null)).toBeNull();
    expect(passkeyKeepsChats(null, "00")).toBeNull();
  });
});

describe("publishing", () => {
  const account = () => ({
    accountIdB64: ACCOUNT_B64,
    accountId: ACCOUNT_ID,
    identity: identity(1),
    keyGeneration: 1n,
  });

  it("publishes the first announcement, records the facts and keeps the bytes", async () => {
    const dir = fakeDirectory({ signaturePubkey: identity(1).signaturePublicKey });
    expect(await publishSeals(wasm, dir.ports, account(), { passwordExportKey: exportKey(1) })).toBe("published");
    expect(dir.state.uploads).toHaveLength(1);
    expect(ownSealFacts(ACCOUNT_B64)).toMatchObject({ password: true, phrase: false, seq: "1" });
    expect(keptAnnouncement(dir.storage, ACCOUNT_B64)).toEqual(dir.state.uploads[0]);
  });

  it("sends nothing when what is published already says it", async () => {
    const dir = fakeDirectory({ signaturePubkey: identity(1).signaturePublicKey });
    await publishSeals(wasm, dir.ports, account(), { passwordExportKey: exportKey(1) });
    expect(await publishSeals(wasm, dir.ports, account(), { passwordExportKey: exportKey(1) })).toBe("unchanged");
    expect(dir.state.uploads).toHaveLength(1);
  });

  it("a new password replaces the entry with a higher seq and keeps the passkey and the phrase", async () => {
    const dir = fakeDirectory({ signaturePubkey: identity(1).signaturePublicKey });
    await publishSeals(wasm, dir.ports, account(), { passwordExportKey: exportKey(1) });
    await publishSeals(wasm, dir.ports, account(), { addPasskey: { credentialId: cred(7), prfOutput: prf(7) } });
    await publishSeals(wasm, dir.ports, account(), { savePhrase: PHRASE });
    expect(await publishSeals(wasm, dir.ports, account(), { passwordExportKey: exportKey(2) })).toBe("published");
    const facts = ownSealFacts(ACCOUNT_B64);
    expect(facts).toMatchObject({ phrase: true, seq: "4" });
    expect(facts?.passkeyTagsHex).toEqual([hex(wasm.resetSealPasskeyTag(cred(7)))]);
  });

  it("never carries an entry from an announcement that does not open under the account's key", async () => {
    // The directory serves an announcement signed by another key that lists
    // a passkey seal of its own: nothing of it is carried.
    const foreign = signSeals(wasm, {
      identity: identity(9),
      accountId: ACCOUNT_ID,
      keyGeneration: 1n,
      seq: 5n,
      nowMs: 1,
      entries: entries(
        { addPasskey: { credentialId: cred(66), prfOutput: prf(66) } },
        entries({ passwordExportKey: exportKey(66) }),
      ),
    });
    const dir = fakeDirectory({ signaturePubkey: identity(1).signaturePublicKey, resetSeals: foreign });
    expect(await publishSeals(wasm, dir.ports, account(), { passwordExportKey: exportKey(1) })).toBe("published");
    const facts = ownSealFacts(ACCOUNT_B64);
    expect(facts?.passkeyTagsHex).toEqual([]);
    // Its seq is still a floor: this device signs above it.
    expect(facts?.seq).toBe("6");
  });

  it("with no export key and nothing to carry, publishes nothing", async () => {
    const dir = fakeDirectory({ signaturePubkey: identity(1).signaturePublicKey });
    expect(await publishSeals(wasm, dir.ports, account(), { savePhrase: PHRASE })).toBe("no-password");
    expect(dir.state.uploads).toHaveLength(0);
  });

  it("after a rotation carries the entries from the kept announcement, opened under the old key", async () => {
    const dir = fakeDirectory({ signaturePubkey: identity(1).signaturePublicKey });
    await publishSeals(wasm, dir.ports, account(), { passwordExportKey: exportKey(1) });
    await publishSeals(wasm, dir.ports, account(), { addPasskey: { credentialId: cred(7), prfOutput: prf(7) } });
    // The rotation: the row moves to the new key and generation; the old
    // announcement becomes the previous one.
    dir.state.row = {
      signaturePubkey: identity(2).signaturePublicKey,
      keyGeneration: 2,
      resetSeals: null,
      previousResetSeals: dir.state.uploads[1]!,
    };
    const rotated = { accountIdB64: ACCOUNT_B64, accountId: ACCOUNT_ID, identity: identity(2), keyGeneration: 2n };
    // Without the kept announcement nothing can be listed for the password.
    expect(await publishSeals(wasm, dir.ports, rotated, {})).toBe("no-password");
    expect(
      await publishSeals(wasm, dir.ports, rotated, {}, { carryFromKept: { keptUnderKey: identity(1).signaturePublicKey } }),
    ).toBe("published");
    expect(ownSealFacts(ACCOUNT_B64)).toMatchObject({ password: true, seq: "3" });
    expect(ownSealFacts(ACCOUNT_B64)?.passkeyTagsHex).toHaveLength(1);
    const opened = readSeals(wasm, dir.state.uploads[2]!, identity(2).signaturePublicKey, ACCOUNT_ID);
    expect(opened?.keyGeneration).toBe(2n);
  });

  it("reads again and retries once when another device published first, then gives up", async () => {
    const dir = fakeDirectory({ signaturePubkey: identity(1).signaturePublicKey });
    await publishSeals(wasm, dir.ports, account(), { passwordExportKey: exportKey(1) });
    // Another device adds a passkey between this device's read and write.
    dir.state.refuseOnce = 409;
    dir.state.onUpload = () => {
      const theirs = signSeals(wasm, {
        identity: identity(1),
        accountId: ACCOUNT_ID,
        keyGeneration: 1n,
        seq: 2n,
        nowMs: 1,
        entries: entries(
          { addPasskey: { credentialId: cred(7), prfOutput: prf(7) } },
          entries({ passwordExportKey: exportKey(1) }),
        ),
      });
      dir.state.row = { ...dir.state.row!, resetSeals: theirs };
    };
    expect(await publishSeals(wasm, dir.ports, account(), { savePhrase: PHRASE })).toBe("published");
    const facts = ownSealFacts(ACCOUNT_B64);
    expect(facts).toMatchObject({ phrase: true, seq: "3" });
    expect(facts?.passkeyTagsHex).toHaveLength(1);

    dir.state.onUpload = null;
    dir.state.refuseOnce = 500;
    expect(await publishSeals(wasm, dir.ports, account(), { passwordExportKey: exportKey(3) })).toBe("retry");
    dir.state.failFetch = true;
    expect(await publishSeals(wasm, dir.ports, account(), { passwordExportKey: exportKey(3) })).toBe("retry");
  });

  it("reads the own announcement at a landing: facts when it opens, nothing when it does not", async () => {
    const dir = fakeDirectory({ signaturePubkey: identity(1).signaturePublicKey });
    await publishSeals(wasm, dir.ports, account(), { passwordExportKey: exportKey(1), savePhrase: PHRASE });
    dir.storage.map.clear();
    const opened = await readOwnSeals(wasm, dir.ports, account());
    expect(opened?.entries.map((e) => e.kind)).toEqual(["password", "phrase"]);
    expect(ownSealFacts(ACCOUNT_B64)?.phrase).toBe(true);
    expect(keptAnnouncement(dir.storage, ACCOUNT_B64)).not.toBeNull();

    const other = { ...account(), identity: identity(2) };
    expect(await readOwnSeals(wasm, dir.ports, other)).toBeNull();
    expect(ownSealFacts(ACCOUNT_B64)).toBeNull();

    clearSealFacts(dir.storage, ACCOUNT_B64);
    expect(keptAnnouncement(dir.storage, ACCOUNT_B64)).toBeNull();
  });
});

describe("sealing a reset", () => {
  const NEW_KEY = () => identity(2).signaturePublicKey;

  function announced(list: readonly SealEntry[], seq = 4n) {
    const bytes = signSeals(wasm, {
      identity: identity(1),
      accountId: ACCOUNT_ID,
      keyGeneration: 1n,
      seq,
      nowMs: 1,
      entries: list,
    });
    const opened = readSeals(wasm, bytes, identity(1).signaturePublicKey, ACCOUNT_ID);
    if (opened === null) throw new Error("fixture does not open");
    return { bytes, opened };
  }

  it("the current password seals; a password changed without the keys does not", () => {
    const { opened } = announced(entries({ passwordExportKey: exportKey(1) }));
    expect(resetOutlook(wasm, opened, [{ kind: "password", exportKey: exportKey(1) }])).toEqual({
      sealed: true,
      passkeyCouldSeal: false,
    });
    expect(resetOutlook(wasm, opened, [{ kind: "password", exportKey: exportKey(2) }])).toEqual({
      sealed: false,
      passkeyCouldSeal: false,
    });
    expect(resetOutlook(wasm, null, [{ kind: "password", exportKey: exportKey(1) }]).sealed).toBe(false);
  });

  it("offers the passkey when the account lists one that was not the confirmation used", () => {
    const { opened } = announced(
      entries(
        { addPasskey: { credentialId: cred(7), prfOutput: prf(7) } },
        entries({ passwordExportKey: exportKey(1) }),
      ),
    );
    const stalePassword: SealSecret = { kind: "password", exportKey: exportKey(2) };
    expect(resetOutlook(wasm, opened, [stalePassword])).toEqual({ sealed: false, passkeyCouldSeal: true });
    const passkey: SealSecret = { kind: "passkey", prfOutput: prf(7), credentialId: cred(7) };
    expect(resetOutlook(wasm, opened, [stalePassword, passkey])).toEqual({ sealed: true, passkeyCouldSeal: false });
    expect(chooseSealSecret(wasm, opened, [stalePassword, passkey])?.kind).toBe("passkey");
    // A passkey that gives another secret, or another passkey's tag, seals nothing.
    const wrong: SealSecret = { kind: "passkey", prfOutput: prf(8), credentialId: cred(7) };
    expect(resetOutlook(wasm, opened, [stalePassword, wrong])).toEqual({ sealed: false, passkeyCouldSeal: false });
    const otherTag: SealSecret = { kind: "passkey", prfOutput: prf(7), credentialId: cred(8) };
    expect(chooseSealSecret(wasm, opened, [otherTag])).toBeNull();
  });

  it("a peer accepts the seal this device says is sealed, and refuses one from an unlisted key", () => {
    const { bytes, opened } = announced(entries({ passwordExportKey: exportKey(1), savePhrase: PHRASE }));
    for (const secret of [
      { kind: "password", exportKey: exportKey(1) },
      { kind: "phrase", phrase: PHRASE },
    ] as SealSecret[]) {
      expect(chooseSealSecret(wasm, opened, [secret])).not.toBeNull();
      const seal = signResetSeal(wasm, {
        secret,
        accountId: ACCOUNT_ID,
        announcementSeq: opened.seq,
        fromGeneration: 1n,
        newSignaturePubkey: NEW_KEY(),
      });
      const check = wasm.checkResetSeal(bytes, seal, identity(1).signaturePublicKey, ACCOUNT_ID, 1n, 2n, NEW_KEY(), 0n);
      expect(check.valid).toBe(true);
      expect(check.announcementSeq).toBe(4n);
    }
    const unlisted = signResetSeal(wasm, {
      secret: { kind: "password", exportKey: exportKey(2) },
      accountId: ACCOUNT_ID,
      announcementSeq: opened.seq,
      fromGeneration: 1n,
      newSignaturePubkey: NEW_KEY(),
    });
    expect(
      wasm.checkResetSeal(bytes, unlisted, identity(1).signaturePublicKey, ACCOUNT_ID, 1n, 2n, NEW_KEY(), 0n).valid,
    ).toBe(false);
  });
});
