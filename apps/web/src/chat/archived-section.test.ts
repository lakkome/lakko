// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/navigation.md` 2.2 step 3 ("Archived, as built (2026-11-25)"),
 * asserted THROUGH the shared rule: the wasm module is the real one, so these
 * tests pin both that the web reaches `archived_section` and what it slices
 * with the answer.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { shownRows, splitArchived, type ArchivedRules, type ArchivedSplitOptions } from "./archived-section";

let rules: ArchivedRules;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
  rules = { archivedSection: wasm.archivedSection };
});

interface Row {
  readonly id: string;
  readonly archived: boolean;
  readonly pinned?: boolean;
}

const live = (id: string): Row => ({ id, archived: false });
const gone = (id: string): Row => ({ id, archived: true });
const ids = (rows: readonly Row[]): string[] => rows.map((r) => r.id);
const archivedRows = (n: number): Row[] => Array.from({ length: n }, (_, i) => gone(`a${i}`));

const chats: ArchivedSplitOptions = { searching: false, archiveOpen: false };
const archive: ArchivedSplitOptions = { searching: false, archiveOpen: true };

function split(rows: readonly Row[], options: ArchivedSplitOptions) {
  return splitArchived(
    rows,
    (r) => r.archived,
    (r) => r.pinned === true,
    options,
    rules,
  );
}

describe("splitArchived over the shared rule", () => {
  it("draws no bar when nothing is archived", () => {
    const s = split([live("x"), live("y")], chats);
    expect(s.view).toBe("chats");
    expect(s.bar).toBeNull();
    expect(s.divider).toBe(false);
    expect(ids(s.top)).toEqual(["x", "y"]);
    expect(s.archived).toEqual([]);
    expect(split([], chats).bar).toBeNull();
  });

  it("keeps one archived conversation out of the chats list, behind the bar", () => {
    const s = split([live("x"), gone("a")], chats);
    expect(s.view).toBe("chats");
    expect(s.bar).toEqual({ count: 1 });
    expect(s.divider).toBe(false);
    expect(ids(s.top)).toEqual(["x"]);
    expect(ids(shownRows(s))).toEqual(["x"]);
  });

  it("counts any number of archived conversations on the bar", () => {
    const s = split([live("x"), ...archivedRows(20)], chats);
    expect(s.bar).toEqual({ count: 20 });
    expect(ids(shownRows(s))).toEqual(["x"]);
  });

  it("keeps a pinned archived conversation with the pins", () => {
    const s = split([{ id: "p", archived: true, pinned: true }, live("x"), gone("a")], chats);
    expect(ids(s.top)).toEqual(["p", "x"]);
    expect(ids(s.archived)).toEqual(["a"]);
    expect(s.bar).toEqual({ count: 1 });
  });

  it("opens the archive in place of the chats list", () => {
    const rows = archivedRows(3);
    const s = split([live("x"), ...rows], archive);
    expect(s.view).toBe("archive");
    expect(s.bar).toBeNull();
    expect(ids(shownRows(s))).toEqual(ids(rows));
  });

  it("returns to the chats list once the last archived conversation is unarchived", () => {
    const s = split([live("x"), live("a")], archive);
    expect(s.view).toBe("chats");
    expect(s.bar).toBeNull();
  });

  it("lists archived matches under the divider when searching the chats view", () => {
    const s = split([live("x"), gone("a"), gone("b")], { searching: true, archiveOpen: false });
    expect(s.view).toBe("chats");
    expect(s.bar).toBeNull();
    expect(s.divider).toBe(true);
    expect(ids(shownRows(s))).toEqual(["x", "a", "b"]);
  });

  it("searches the archive alone when it is open", () => {
    const s = split([live("x"), gone("a")], { searching: true, archiveOpen: true });
    expect(s.view).toBe("archive");
    expect(s.divider).toBe(false);
    expect(ids(shownRows(s))).toEqual(["a"]);
    // An empty result is not an empty archive: the view stays.
    const none = split([live("x")], { searching: true, archiveOpen: true });
    expect(none.view).toBe("archive");
    expect(shownRows(none)).toEqual([]);
  });

  it("lists everything in the chats view before the rules load", () => {
    const s = splitArchived([gone("a")], (r) => r.archived, () => false, archive, null);
    expect(s.view).toBe("chats");
    expect(s.bar).toBeNull();
    expect(ids(s.top)).toEqual(["a"]);
  });
});
