// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A backup always lists the account's conversations (2026-10-02).
 *
 * The backup carries no MLS state, so after a restore this device holds none
 * of the account's groups itself. Without the restored set its first upload
 * would list nothing and erase the record. Pinned here:
 *
 *   - after a restore naming G1 and G2, the next upload carries G1, G2 plus
 *     the groups the device holds;
 *   - joining G1 takes it out of the restored set (it is then held);
 *   - deleting a chat takes its id out;
 *   - the per-account wipe clears the store, and nothing that runs at log
 *     out does;
 *   - another account on the same browser has its own set.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { MemoryStorage } from "../test/fake-storage";
import {
  addRestoredKnownGroups,
  backupKnownGroupIds,
  clearRestoredKnownGroups,
  dropRestoredKnownGroup,
  loadRestoredKnownGroups,
  pruneJoinedRestoredGroups,
} from "./restored-known-groups";

const ALICE = "YWxpY2UtYWNjb3VudC1pZA==";
const BOB = "Ym9iLWFjY291bnQtaWQtISE=";
const G1 = "ZzEtZ3JvdXAtaWQ=";
const G2 = "ZzItZ3JvdXAtaWQ=";
const OWN = "b3duLWdyb3VwLWlk";

let storage: MemoryStorage;

beforeEach(() => {
  storage = new MemoryStorage();
});

describe("the conversations a backup lists after a restore", () => {
  it("carries the restored groups plus the groups this device holds", () => {
    addRestoredKnownGroups(ALICE, [G1, G2], storage);
    // The device holds nothing yet: the upload still lists both.
    expect(backupKnownGroupIds(ALICE, [], storage)).toEqual([G1, G2]);
    // And with a group of its own, that one too.
    expect(backupKnownGroupIds(ALICE, [OWN], storage).sort()).toEqual([G1, G2, OWN].sort());
    // Nothing was pruned by merely asking.
    expect(loadRestoredKnownGroups(ALICE, storage)).toEqual([G1, G2]);
  });

  it("a device that never restored lists exactly the groups it holds", () => {
    expect(backupKnownGroupIds(ALICE, [OWN], storage)).toEqual([OWN]);
    expect(backupKnownGroupIds(ALICE, [], storage)).toEqual([]);
    expect(storage.length).toBe(0);
  });

  it("joining G1 removes it from the restored set and keeps it in the upload", () => {
    addRestoredKnownGroups(ALICE, [G1, G2], storage);
    const listed = backupKnownGroupIds(ALICE, [G1], storage);
    // Still listed once (as a held group), not twice.
    expect(listed.sort()).toEqual([G1, G2].sort());
    expect(loadRestoredKnownGroups(ALICE, storage)).toEqual([G2]);
  });

  it("a group joined and then deleted is no longer listed", () => {
    addRestoredKnownGroups(ALICE, [G1, G2], storage);
    // Joined: pruned the moment the registry holds it.
    expect(pruneJoinedRestoredGroups(ALICE, [G1], storage)).toEqual([G2]);
    // Deleted: gone from the registry, and not resurrected from the store.
    expect(backupKnownGroupIds(ALICE, [], storage)).toEqual([G2]);
  });

  it("deleting a chat drops its id even before any prune ran", () => {
    addRestoredKnownGroups(ALICE, [G1, G2], storage);
    dropRestoredKnownGroup(ALICE, G1, storage);
    expect(loadRestoredKnownGroups(ALICE, storage)).toEqual([G2]);
    expect(backupKnownGroupIds(ALICE, [], storage)).toEqual([G2]);
    // Dropping an id that is not there changes nothing.
    dropRestoredKnownGroup(ALICE, "bm90LXRoZXJl", storage);
    expect(loadRestoredKnownGroups(ALICE, storage)).toEqual([G2]);
  });

  it("an ended group is still held, so the join rule has already dropped it", () => {
    addRestoredKnownGroups(ALICE, [G1], storage);
    // The registry keeps an ended group until the person deletes the chat.
    expect(backupKnownGroupIds(ALICE, [G1], storage)).toEqual([G1]);
    expect(loadRestoredKnownGroups(ALICE, storage)).toEqual([]);
  });

  it("the row disappears once every restored group is joined", () => {
    addRestoredKnownGroups(ALICE, [G1, G2], storage);
    pruneJoinedRestoredGroups(ALICE, [G1, G2, OWN], storage);
    expect(storage.length).toBe(0);
  });

  it("a second restore adds to the set, never replaces it", () => {
    addRestoredKnownGroups(ALICE, [G1], storage);
    addRestoredKnownGroups(ALICE, [G2, G1], storage);
    expect(loadRestoredKnownGroups(ALICE, storage)).toEqual([G1, G2]);
    // A restore that named no groups forgets nothing.
    addRestoredKnownGroups(ALICE, [], storage);
    expect(loadRestoredKnownGroups(ALICE, storage)).toEqual([G1, G2]);
  });
});

describe("whose set it is, and how long it lives", () => {
  it("is per account", () => {
    addRestoredKnownGroups(ALICE, [G1], storage);
    addRestoredKnownGroups(BOB, [G2], storage);
    expect(backupKnownGroupIds(ALICE, [], storage)).toEqual([G1]);
    expect(backupKnownGroupIds(BOB, [], storage)).toEqual([G2]);
  });

  it("the wipe clears the account's set and leaves the other account's", () => {
    addRestoredKnownGroups(ALICE, [G1, G2], storage);
    addRestoredKnownGroups(BOB, [G2], storage);
    clearRestoredKnownGroups(ALICE, storage);
    expect(loadRestoredKnownGroups(ALICE, storage)).toEqual([]);
    expect(loadRestoredKnownGroups(BOB, storage)).toEqual([G2]);
  });

  it("reads nothing and writes nothing with no storage at all", () => {
    expect(loadRestoredKnownGroups(ALICE, null)).toEqual([]);
    expect(() => addRestoredKnownGroups(ALICE, [G1], null)).not.toThrow();
    expect(backupKnownGroupIds(ALICE, [OWN], null)).toEqual([OWN]);
  });

  it("answers empty on a damaged row rather than throwing", () => {
    storage.setItem(`lakko.backup.restored-groups.v1.${ALICE}`, "{{{");
    expect(loadRestoredKnownGroups(ALICE, storage)).toEqual([]);
    storage.setItem(`lakko.backup.restored-groups.v1.${ALICE}`, JSON.stringify({ a: 1 }));
    expect(loadRestoredKnownGroups(ALICE, storage)).toEqual([]);
    storage.setItem(`lakko.backup.restored-groups.v1.${ALICE}`, JSON.stringify([G1, 7, null, G1]));
    expect(loadRestoredKnownGroups(ALICE, storage)).toEqual([G1]);
  });
});

// `AuthContext.tsx` needs a Solid runtime this suite does not have, so which
// of its flows touch the store is pinned on its source.
describe("log out, forget device, and the uploads (AuthContext source)", () => {
  const source = readFileSync(
    fileURLToPath(new URL("../auth/AuthContext.tsx", import.meta.url)),
    "utf8",
  );
  const code = (text: string): string =>
    text
      .split("\n")
      .filter((l) => !/^\s*(\*|\/\/|\/\*)/.test(l))
      .join("\n");
  const logoutStart = source.indexOf("    logout() {");
  const forgetStart = source.indexOf("    async forgetDevice() {");

  it("log out keeps the set and the owed flag", () => {
    expect(logoutStart).toBeGreaterThan(-1);
    expect(forgetStart).toBeGreaterThan(logoutStart);
    const logout = code(source.slice(logoutStart, forgetStart));
    expect(logout).not.toMatch(
      /wipeAccountLocalData|clearRestoredKnownGroups|clearBackupRestoreOwed|localStorage/,
    );
  });

  it("forget device runs the one wipe, which holds both stores", () => {
    expect(code(source.slice(forgetStart))).toMatch(/await wipeAccountLocalData\(/);
    const wipe = readFileSync(
      fileURLToPath(new URL("../auth/wipe-account.ts", import.meta.url)),
      "utf8",
    );
    expect(wipe).toMatch(/clearRestoredKnownGroups\(accountIdB64\)/);
    expect(wipe).toMatch(/clearBackupRestoreOwed\(accountIdB64\)/);
  });

  it("the automatic backup lists the union, from the account identity's seed", () => {
    const start = source.indexOf("startAutoBackup({");
    expect(start).toBeGreaterThan(-1);
    const wiring = code(source.slice(start, source.indexOf("createEffect(", start)));
    expect(wiring).toMatch(/return backupKnownGroupIds\(/);
    expect(wiring).toMatch(/ed25519Seed: \(\) => chats\.accountIdentity\(\)\?\.ed25519Seed \?\? null/);
    expect(wiring).not.toMatch(/snapshot/i);
  });

  it("a joined group leaves the restored set as soon as the registry holds it", () => {
    expect(code(source)).toMatch(/pruneJoinedRestoredGroups\(base64Encode\(id\), held\)/);
  });
});
