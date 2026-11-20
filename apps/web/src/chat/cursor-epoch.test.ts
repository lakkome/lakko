// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0091 phase 1b - the epoch recorded with a conversation's cursor, and
 *  when a poll reads again from the commit leaving the loaded epoch. */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  clearCursorEpochForGroup,
  clearCursorEpochsForAccount,
  readCursorEpoch,
  recordCursorEpoch,
  rewindLine,
  snapshotRewindFrom,
} from "./cursor-epoch";
import { bindContactsAccount } from "../contacts/local-store";

const CARA = "Y2FyYS1hY2NvdW50LTAwMDA=";
const EVE = "ZXZlLWFjY291bnQtMDAwMDA=";

let data: Map<string, string>;

beforeEach(() => {
  data = new Map();
  (globalThis as unknown as { localStorage: Storage }).localStorage = {
    get length() {
      return data.size;
    },
    clear: () => data.clear(),
    getItem: (k: string) => data.get(k) ?? null,
    key: (i: number) => [...data.keys()][i] ?? null,
    removeItem: (k: string) => void data.delete(k),
    setItem: (k: string, v: string) => void data.set(k, String(v)),
  };
});

afterEach(() => bindContactsAccount(null));

describe("the recorded epoch", () => {
  it("never moves backward and belongs to the signed-in account", () => {
    bindContactsAccount(CARA);
    expect(readCursorEpoch("g")).toBeUndefined();
    recordCursorEpoch("g", 7n);
    recordCursorEpoch("g", 5n);
    expect(readCursorEpoch("g")).toBe(7n);
    recordCursorEpoch("g", 9n);
    expect(readCursorEpoch("g")).toBe(9n);

    bindContactsAccount(EVE);
    expect(readCursorEpoch("g")).toBeUndefined();
  });

  it("is a no-op with no account, and is dropped per group and per account", () => {
    recordCursorEpoch("g", 3n);
    expect(data.size).toBe(0);

    bindContactsAccount(CARA);
    recordCursorEpoch("g", 3n);
    recordCursorEpoch("h", 4n);
    recordCursorEpoch("g", 8n, EVE);
    clearCursorEpochForGroup("g");
    expect(readCursorEpoch("g")).toBeUndefined();
    expect(readCursorEpoch("h")).toBe(4n);
    clearCursorEpochsForAccount(CARA);
    expect(readCursorEpoch("h")).toBeUndefined();
    expect(readCursorEpoch("g", EVE)).toBe(8n);
  });

  it("an unreadable row reads as none", () => {
    bindContactsAccount(CARA);
    data.set(`lakko.chat.cursor-epoch.v1:${CARA}:g`, "not a number");
    expect(readCursorEpoch("g")).toBeUndefined();
  });
});

describe("snapshotRewindFrom", () => {
  const commits: ReadonlyMap<bigint, number> = new Map([
    [7n, 500],
    [8n, 640],
  ]);

  it("reads again from just before the commit leaving the loaded epoch", () => {
    expect(snapshotRewindFrom({ after: 700, loaded: 6n, recorded: 8n, registeredCommits: commits })).toBe(499);
    // At the cursor itself: the cursor is past it.
    expect(snapshotRewindFrom({ after: 500, loaded: 6n, recorded: 7n, registeredCommits: commits })).toBe(499);
  });

  it.each([
    ["no recorded epoch", { after: 700, loaded: 6n, recorded: undefined }],
    ["no loaded epoch", { after: 700, loaded: undefined, recorded: 8n }],
    ["the group at the recorded epoch", { after: 700, loaded: 8n, recorded: 8n }],
    ["the group above the recorded epoch", { after: 700, loaded: 9n, recorded: 8n }],
    // The commit is still ahead of the cursor: an ordinary read finds it.
    ["the commit ahead of the cursor", { after: 400, loaded: 6n, recorded: 8n }],
    // Pruned or replaced: nothing to read again.
    ["no commit named for the loaded epoch", { after: 700, loaded: 4n, recorded: 8n }],
  ])("does not rewind with %s", (_name, input) => {
    expect(snapshotRewindFrom({ ...input, registeredCommits: commits })).toBeUndefined();
  });

  it("does not rewind when the read named no commits", () => {
    expect(snapshotRewindFrom({ after: 700, loaded: 6n, recorded: 8n, registeredCommits: undefined })).toBeUndefined();
  });

  it("logs epochs only", () => {
    expect(rewindLine(6n, 8n)).toBe(
      "chat snapshot behind its cursor: loaded=6 recorded=8; reading again from the commit that leaves the loaded epoch",
    );
  });
});
