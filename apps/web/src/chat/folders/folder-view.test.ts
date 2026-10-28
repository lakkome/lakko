// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The row-to-rule mapping, against the REAL shared rule. The rule's own
 * semantics are tested in `lakko-ui-rules`; these test that the web hands it
 * the right fields (muted, has-unread, archived, the prefixed ids) and applies
 * its answer.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../../wasm/lakko_ffi_wasm";
import type { ConversationRow } from "../conversation-list";
import {
  bytesToB64,
  EXCLUDE_ARCHIVED,
  EXCLUDE_MUTED,
  EXCLUDE_READ,
  INCLUDE_CHANNELS,
  INCLUDE_GROUPS,
  INCLUDE_ONE_TO_ONE,
  type FolderModel,
} from "./folder-model";
import {
  allChatsUnreadCount,
  folderRowsOrdered,
  folderUnreadCount, isPinnedInFolder,
  rowIsInFolder,
} from "./folder-view";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const rules = wasm;

const gid = (n: number): Uint8Array => new Uint8Array(32).fill(n);

function row(over: Partial<ConversationRow> & { id: string }): ConversationRow {
  return {
    kind: "group",
    title: over.id,
    preview: "",
    lastActivityMs: 1,
    unread: 0,
    pinned: false,
    muted: false,
    archived: false,
    ...over,
  };
}

function folder(over: Partial<FolderModel> = {}): FolderModel {
  return {
    idHex: "aa".repeat(16),
    name: "F",
    includeKinds: 0,
    excludeFlags: 0,
    included: [],
    excluded: [],
    pinned: [],
    ...over,
  };
}

const g1 = bytesToB64(gid(1));
const g2 = bytesToB64(gid(2));
const ch = "03".repeat(32);

describe("matching", () => {
  it("routes each kind to its toggle", () => {
    const rows = [row({ id: g1, kind: "chat" }), row({ id: g2 }), row({ id: ch, kind: "channel" })];
    const names = (f: FolderModel): string[] => folderRowsOrdered(rows, f, rules).map((r) => r.id);
    expect(names(folder({ includeKinds: INCLUDE_ONE_TO_ONE }))).toEqual([g1]);
    expect(names(folder({ includeKinds: INCLUDE_GROUPS }))).toEqual([g2]);
    expect(names(folder({ includeKinds: INCLUDE_CHANNELS }))).toEqual([ch]);
  });

  it("hands the rule muted, unread and archived", () => {
    const muted = row({ id: g1, muted: true, unread: 1 });
    const read = row({ id: g2, unread: 0 });
    const f = folder({ includeKinds: INCLUDE_GROUPS, excludeFlags: EXCLUDE_MUTED | EXCLUDE_READ });
    expect(rowIsInFolder(muted, f, rules)).toBe(false);
    expect(rowIsInFolder(read, f, rules)).toBe(false);
    // A manual "mark as unread" counts as unread.
    expect(rowIsInFolder({ ...read, hasUnread: true }, f, rules)).toBe(true);
    const arch = folder({ includeKinds: INCLUDE_GROUPS, excludeFlags: EXCLUDE_ARCHIVED });
    expect(rowIsInFolder(row({ id: g1, archived: true }), arch, rules)).toBe(false);
  });

  it("a hand-added group and a channel with the same id string stay apart", () => {
    // Pathological on purpose: the list could never produce this, but the
    // prefixed ids make it impossible rather than unlikely.
    const same = "abcd";
    const f = folder({ included: [{ kind: 3, id: new Uint8Array([0xab, 0xcd]) }] });
    expect(rowIsInFolder(row({ id: same, kind: "channel" }), f, rules)).toBe(true);
    expect(rowIsInFolder(row({ id: same, kind: "group" }), f, rules)).toBe(false);
  });
});

describe("ordering and pins", () => {
  it("puts the folder's own pins first, ignoring the global pin", () => {
    const rows = [
      row({ id: g1, pinned: true, lastActivityMs: 5 }),
      row({ id: g2, lastActivityMs: 1 }),
    ];
    const f = folder({ includeKinds: INCLUDE_GROUPS, pinned: [{ kind: 2, id: gid(2) }] });
    expect(folderRowsOrdered(rows, f, rules).map((r) => r.id)).toEqual([g2, g1]);
    expect(isPinnedInFolder(rows[1] as ConversationRow, f)).toBe(true);
    expect(isPinnedInFolder(rows[0] as ConversationRow, f)).toBe(false);
  });
});

describe("the tab count", () => {
  it("counts unread conversations in the folder, not muted ones", () => {
    const rows = [
      row({ id: g1, unread: 3 }),
      row({ id: g2, unread: 9, muted: true }),
      row({ id: ch, kind: "channel" }),
    ];
    expect(folderUnreadCount(rows, folder({ includeKinds: INCLUDE_GROUPS | INCLUDE_CHANNELS }), rules)).toBe(1);
  });
});

describe("allChatsUnreadCount", () => {
  it("counts every unread, unmuted conversation, archived and channels included", () => {
    const rows = [
      row({ id: "a", unread: 2 }),
      row({ id: "b", unread: 1, muted: true }),
      row({ id: "c", unread: 0, hasUnread: true, archived: true }),
      row({ id: "d", kind: "channel", unread: 5 }),
      row({ id: "e", unread: 3, hasUnread: false }),
      row({ id: "f" }),
    ];
    expect(allChatsUnreadCount(rows)).toBe(3);
    expect(allChatsUnreadCount([])).toBe(0);
  });
});
