// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The chat list's one "Synchronizing..." label (2026-10-02).
 *
 * It shows for groups the PAIRING bundle named that this device has not
 * joined yet, and for nothing else. An account restored from the phrase with
 * an empty chat list shows no label: nobody is about to admit that device.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { MemoryStorage } from "../test/fake-storage";
import { addRestoredKnownGroups } from "../backup/restored-known-groups";
import { markBackupRestoreOwed } from "../backup/restore-owed";
import { saveKnownGroupIds } from "../devices/state-bundle";
import { showsSynchronizing } from "./synchronizing";

const ACCOUNT = "YWNjb3VudC1pZA==";
const G1 = "ZzEtZ3JvdXAtaWQ=";
const G2 = "ZzItZ3JvdXAtaWQ=";

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["sessionStorage"] = new MemoryStorage();
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["sessionStorage"];
});

describe("showsSynchronizing", () => {
  it("shows nothing for a restored account with an empty chat list", () => {
    // Everything a phrase restore leaves behind: the conversations the
    // backup named, and (while its fetch is pending) the owed flag.
    addRestoredKnownGroups(ACCOUNT, [G1, G2]);
    markBackupRestoreOwed(ACCOUNT);
    expect(showsSynchronizing([], ACCOUNT)).toBe(false);
  });

  it("shows nothing for an ordinary empty list", () => {
    expect(showsSynchronizing([], ACCOUNT)).toBe(false);
  });

  it("still shows while a group the pairing bundle named is not joined", () => {
    saveKnownGroupIds([G1, G2], ACCOUNT);
    expect(showsSynchronizing([], ACCOUNT)).toBe(true);
    expect(showsSynchronizing([G1], ACCOUNT)).toBe(true);
  });

  it("goes once every group the pairing bundle named is joined", () => {
    saveKnownGroupIds([G1, G2], ACCOUNT);
    expect(showsSynchronizing([G1, G2], ACCOUNT)).toBe(false);
    // And cannot come back: the pairing list cleared itself.
    expect(showsSynchronizing([], ACCOUNT)).toBe(false);
  });

  it("the restored set never leaks into the pairing list", () => {
    addRestoredKnownGroups(ACCOUNT, [G1]);
    saveKnownGroupIds([G2], ACCOUNT);
    // Joining the pairing-named group ends the label although a restored
    // group is still not joined.
    expect(showsSynchronizing([G2], ACCOUNT)).toBe(false);
  });
});

describe("the chat list", () => {
  const read = (rel: string): string =>
    readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

  it("takes the label from this rule alone, and no restore marker exists", () => {
    const list = read("./ChatList.tsx");
    expect(list).toMatch(/showsSynchronizing\(/);
    expect(list).not.toMatch(/restore-marker|restoredKind/);
    const auth = read("../auth/AuthContext.tsx");
    expect(auth).not.toMatch(/restore-marker|markRestored|clearRestored/);
  });
});
