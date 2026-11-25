// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The web's side of the pin order (`docs/ux/navigation.md` 2.2.1): the
 * mapping between the synced register's bytes and the list's row ids, the
 * whole list each action writes, and when Move up / Move down applies. The
 * rule itself is `crates/lakko-ui-rules` and is tested there; these run it
 * through the real wasm so the wiring is checked against the rule, not
 * against a guess of it.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import * as wasmNs from "../../wasm/lakko_ffi_wasm";
import init from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import type { ConversationRow } from "./conversation-list";
import { sortConversations } from "./conversation-list";
import type { FolderModel } from "./folders/folder-model";
import {
  allChatsOrderAfter,
  allChatsPinOrderIds,
  fallbackIdOfKey,
  folderPinsAfter,
  moveByOne,
  orderKeyOfRow,
  pinOrderView,
} from "./pin-order";
import type { UiRules } from "./ui-rules";

const rules = wasmNs as unknown as UiRules;

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

/** A 32-byte conversation id filled with `b`. */
const conv = (b: number): Uint8Array => new Uint8Array(32).fill(b);
/** A 16-byte channel id filled with `b`. */
const chan = (b: number): Uint8Array => new Uint8Array(16).fill(b);
const hex = (bytes: Uint8Array): string =>
  Array.from(bytes, (x) => x.toString(16).padStart(2, "0")).join("");

function row(id: string, over: Partial<ConversationRow> = {}): ConversationRow {
  return {
    id,
    kind: "chat",
    title: id,
    preview: "",
    lastActivityMs: 1_000,
    unread: 0,
    pinned: false,
    muted: false,
    archived: false,
    ...over,
  };
}

const chatRow = (b: number, over: Partial<ConversationRow> = {}): ConversationRow =>
  row(base64Encode(conv(b)), over);
const channelRow = (b: number, over: Partial<ConversationRow> = {}): ConversationRow =>
  row(hex(chan(b)), { kind: "channel", ...over });

describe("bytes <-> row ids", () => {
  it("keys a chat or group by its conversation id and a channel by its channel id", () => {
    expect(orderKeyOfRow(chatRow(1))).toEqual(conv(1));
    expect(orderKeyOfRow(row(base64Encode(conv(2)), { kind: "group" }))).toEqual(conv(2));
    expect(orderKeyOfRow(channelRow(3))).toEqual(chan(3));
    expect(orderKeyOfRow(row("not base64 !"))).toBeNull();
    expect(orderKeyOfRow(row("zz", { kind: "channel" }))).toBeNull();
  });

  it("names a stored key by the row that holds it, and keeps an unknown key under a placeholder", () => {
    const rows = [chatRow(1), channelRow(2)];
    const view = pinOrderView([chan(2), conv(9), conv(1), chan(7)], rows);
    expect(view.storedIds).toEqual([
      hex(chan(2)),
      fallbackIdOfKey(conv(9)),
      base64Encode(conv(1)),
      fallbackIdOfKey(chan(7)),
    ]);
    // Back to the exact bytes, unknown entries included.
    expect(view.keysOf(view.storedIds)).toEqual([chan(2), conv(9), conv(1), chan(7)]);
  });

  it("drops repeats and empty keys, and ids that map to nothing", () => {
    const view = pinOrderView([conv(1), new Uint8Array(0), conv(1)], [chatRow(1)]);
    expect(view.storedIds).toEqual([base64Encode(conv(1))]);
    expect(view.keysOf(["nothing", base64Encode(conv(1)), base64Encode(conv(1))])).toEqual([conv(1)]);
  });

  it("matches a channel row whatever the case of its hex id", () => {
    const upper = row(hex(chan(0xab)).toUpperCase(), { kind: "channel" });
    expect(pinOrderView([chan(0xab)], [upper]).storedIds).toEqual([upper.id]);
  });
});

describe("the order the list draws", () => {
  it("orders the pins by the stored list, an unnamed pin above the named ones", () => {
    const a = chatRow(1, { pinned: true, lastActivityMs: 10 });
    const b = chatRow(2, { pinned: true, lastActivityMs: 20 });
    const c = channelRow(3, { pinned: true, lastActivityMs: 30 });
    const d = chatRow(4, { lastActivityMs: 99 });
    const rows = [a, b, c, d];
    const pinOrder = allChatsPinOrderIds([conv(1), chan(3)], rows);
    const drawn = sortConversations(rows, rules, pinOrder).map((r) => r.id);
    // b is pinned but not named: above the named a, c; d is unpinned.
    expect(drawn).toEqual([b.id, a.id, c.id, d.id]);
  });

  it("a new message does not move a pin", () => {
    const a = chatRow(1, { pinned: true, lastActivityMs: 10 });
    const b = chatRow(2, { pinned: true, lastActivityMs: 5_000 });
    const rows = [a, b];
    const pinOrder = allChatsPinOrderIds([conv(1), conv(2)], rows);
    expect(sortConversations(rows, rules, pinOrder).map((r) => r.id)).toEqual([a.id, b.id]);
  });
});

describe("what each action writes (All chats)", () => {
  const a = chatRow(1, { pinned: true });
  const b = chatRow(2, { pinned: true });
  const c = channelRow(3, { pinned: true });
  const d = chatRow(4);
  // Drawn order: a, b, c, then d unpinned. The register also names a pin
  // whose conversation this device does not hold (conv 9).
  const drawn = [a, b, c, d];
  const stored = [conv(1), conv(9), conv(2), chan(3)];

  it("pin puts the new pin on top and keeps the unknown entry", () => {
    const out = allChatsOrderAfter(rules, stored, drawn, { t: "pin", id: d.id });
    expect(out[0]).toEqual(conv(4));
    expect(out).toContainEqual(conv(9));
    expect(new Set(out.map(hex)).size).toBe(out.length);
  });

  it("unpin removes only that pin", () => {
    const out = allChatsOrderAfter(rules, stored, drawn, { t: "unpin", id: b.id });
    expect(out).not.toContainEqual(conv(2));
    expect(out).toContainEqual(conv(1));
    expect(out).toContainEqual(conv(9));
    expect(out).toContainEqual(chan(3));
  });

  it("a move writes the shown pins in their new order", () => {
    const out = allChatsOrderAfter(rules, stored, drawn, { t: "move", id: c.id, toIndex: 0 });
    const shownOnly = out.filter((k) => hex(k) !== hex(conv(9)));
    expect(shownOnly).toEqual([chan(3), conv(1), conv(2)]);
    expect(out).toContainEqual(conv(9));
  });

  it("a move past the end lands last", () => {
    const out = allChatsOrderAfter(rules, stored, drawn, { t: "move", id: a.id, toIndex: 99 });
    const shownOnly = out.filter((k) => hex(k) !== hex(conv(9)));
    expect(shownOnly).toEqual([conv(2), chan(3), conv(1)]);
  });

  it("writes pins made before the order existed", () => {
    // Nothing stored yet: the move writes every shown pin.
    const out = allChatsOrderAfter(rules, [], drawn, { t: "move", id: b.id, toIndex: 0 });
    expect(out).toEqual([conv(2), conv(1), chan(3)]);
  });
});

describe("Move up / Move down", () => {
  const shown = ["a", "b", "c"];

  it("the first pin has no Move up, the last no Move down", () => {
    expect(moveByOne(rules, shown, "a", true)).toBeNull();
    expect(moveByOne(rules, shown, "a", false)).toEqual({ t: "move", id: "a", toIndex: 1 });
    expect(moveByOne(rules, shown, "c", false)).toBeNull();
    expect(moveByOne(rules, shown, "c", true)).toEqual({ t: "move", id: "c", toIndex: 1 });
    expect(moveByOne(rules, shown, "b", true)).toEqual({ t: "move", id: "b", toIndex: 0 });
  });

  it("one pin, or a row that is not a pin, moves nowhere", () => {
    expect(moveByOne(rules, ["a"], "a", true)).toBeNull();
    expect(moveByOne(rules, ["a"], "a", false)).toBeNull();
    expect(moveByOne(rules, shown, "x", true)).toBeNull();
  });
});

describe("folder pins", () => {
  const ref = (kind: 1 | 2 | 3, id: Uint8Array) => ({ kind, id });
  function folder(pinned: FolderModel["pinned"]): FolderModel {
    return {
      idHex: "00".repeat(16),
      name: "Work",
      includeKinds: 0,
      excludeFlags: 0,
      included: [],
      excluded: [],
      pinned,
    };
  }
  const a = chatRow(1);
  const b = row(base64Encode(conv(2)), { kind: "group" });
  const c = channelRow(3);
  const missing = ref(2, conv(9));

  it("a new folder pin goes on top; a pin not on this device keeps its reference", () => {
    const f = folder([ref(1, conv(1)), missing]);
    const edit = folderPinsAfter(rules, f, [a], c, { t: "pin" });
    expect(edit).not.toBeNull();
    if (edit === null || edit.t !== "pins") throw new Error("expected a pins edit");
    expect(edit.pinned[0]).toEqual(ref(3, chan(3)));
    expect(edit.pinned).toContainEqual(missing);
    expect(edit.pinned).toContainEqual(ref(1, conv(1)));
  });

  it("unpin removes only that pin, keeping the reference kinds held", () => {
    const f = folder([ref(1, conv(1)), ref(2, conv(2)), missing]);
    const edit = folderPinsAfter(rules, f, [a, b], a, { t: "unpin" });
    if (edit === null || edit.t !== "pins") throw new Error("expected a pins edit");
    expect(edit.pinned).toEqual([ref(2, conv(2)), missing]);
  });

  it("a move reorders the folder's own pins", () => {
    const f = folder([ref(1, conv(1)), ref(2, conv(2)), ref(3, chan(3))]);
    const edit = folderPinsAfter(rules, f, [a, b, c], c, { t: "move", toIndex: 0 });
    if (edit === null || edit.t !== "pins") throw new Error("expected a pins edit");
    expect(edit.pinned).toEqual([ref(3, chan(3)), ref(1, conv(1)), ref(2, conv(2))]);
  });

  it("keeps the ten-pin limit", () => {
    const ten = Array.from({ length: 10 }, (_, i) => ref(1, conv(100 + i)));
    expect(folderPinsAfter(rules, folder(ten), [], a, { t: "pin" })).toBeNull();
  });
});
