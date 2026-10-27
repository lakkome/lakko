// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 section 4 — saved messages on the web: the edits a Save becomes,
 * driven into the REAL fold (`SyncedState`), the projection the Saved screen
 * reads, the persisted outbox form, the client limits against the core, and
 * the screen's rows.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { stampEdits, type ConversationRef, type RegisterEdit } from "./folders/folder-model";
import { buildFolderChangeSet, projectSyncedState } from "../devices/sync/synced-projection";
import { decodeSyncState, encodeSyncState } from "../devices/sync/sync-state-store";
import {
  SAVED_MAX_CLIENT,
  SAVED_MAX_PER_CHANGE_SET,
  chunkSaveEdits,
  saveEdit,
  saveRefusal,
  savedKeySet,
  savedEntryKey,
  savedKindLabel,
  savedKindOf,
  savedEntriesOf,
  savedOpenRow,
  savedRowDateMs,
  savedTargetOf,
  savedRows,
  unsaveEdit,
} from "./saved-messages";
import type { AttachmentRefBytes } from "./attachments";
import { historyRowId } from "./lineage";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

let clock = 1_000n;
const mint = (): bigint => (clock += 1n);

function apply(s: wasm.SyncedState, edits: readonly RegisterEdit[]): void {
  const cs = buildFolderChangeSet(wasm, stampEdits(edits, mint));
  try {
    s.applyFolderChanges(cs);
  } finally {
    cs.free();
  }
}

const G1: ConversationRef = { kind: 2, id: new Uint8Array(32).fill(1) };
const G2: ConversationRef = { kind: 1, id: new Uint8Array(32).fill(2) };

function photo(n: number, thumb = true): AttachmentRefBytes {
  return {
    blobId: new Uint8Array(32).fill(n),
    key: new Uint8Array(32),
    nonce: new Uint8Array(12),
    mime: "image/jpeg",
    size: 10,
    name: "p.jpg",
    ...(thumb
      ? { thumb: { blobId: new Uint8Array(32).fill(n + 100), key: new Uint8Array(32), nonce: new Uint8Array(12), mime: "image/jpeg", size: 1 } }
      : {}),
  } as AttachmentRefBytes;
}

describe("the limits", () => {
  it("are the core's", () => {
    expect(SAVED_MAX_CLIENT).toBe(wasm.savedMaxClient());
    expect(SAVED_MAX_PER_CHANGE_SET).toBe(wasm.savedMaxPerChangeSet());
    expect(saveRefusal(SAVED_MAX_CLIENT - 1, 1)).toBeNull();
    expect(saveRefusal(SAVED_MAX_CLIENT, 1)).toBe("limit");
    const edits = Array.from({ length: 70 }, (_, i) => unsaveEdit(G1, i));
    expect(chunkSaveEdits(edits).map((c) => c.length)).toEqual([32, 32, 6]);
  });
});

describe("a save into the fold", () => {
  it("is read back with its snapshot and blob ids, newest first", () => {
    const s = new wasm.SyncedState();
    apply(s, [saveEdit({ ref: G1, messageId: 7, author: "Ann", text: "hello", attachments: [photo(1)], sentAtMs: 0 }, 1000)]);
    apply(s, [saveEdit({ ref: G2, messageId: 9, author: "Bo", text: "", attachments: [], sentAtMs: 0 }, 2000)]);
    const p = projectSyncedState(s);
    expect(p.saved.map((e) => e.messageId)).toEqual([9, 7]);
    const first = p.saved[1]!;
    expect(first).toMatchObject({ messageId: 7, author: "Ann", excerpt: "hello", attachmentKind: "photo" });
    expect(first.blobIdsHex).toEqual(["01".repeat(32), "65".repeat(32)]);
    expect(p.savedBlobIdsHex.length).toBe(2);
    expect(savedKeySet(p.saved).has(savedEntryKey("01".repeat(32), 7))).toBe(true);
    s.free();
  });

  it("is removed by an unsave, and comes back saved again", () => {
    const s = new wasm.SyncedState();
    apply(s, [saveEdit({ ref: G1, messageId: 7, author: "", text: "x", attachments: [], sentAtMs: 0 }, 1000)]);
    apply(s, [unsaveEdit(G1, 7)]);
    expect(projectSyncedState(s).saved).toEqual([]);
    apply(s, [saveEdit({ ref: G1, messageId: 7, author: "", text: "x", attachments: [], sentAtMs: 0 }, 3000)]);
    expect(projectSyncedState(s).saved.length).toBe(1);
    s.free();
  });

  it("survives the persisted outbox", () => {
    const edits = stampEdits(
      [saveEdit({ ref: G1, messageId: 7, author: "A", text: "t", attachments: [photo(1)], sentAtMs: 0 }, 5), unsaveEdit(G2, 3)],
      mint,
    );
    const row = decodeSyncState(encodeSyncState({ synced: new Uint8Array(), pending: [edits] }));
    expect(row?.pending).toEqual([edits]);
  });
});

describe("what a save records", () => {
  it("names the media kind, and a file's name when there is no text", () => {
    expect(savedKindOf([])).toBe("");
    expect(savedKindOf([photo(1)])).toBe("photo");
    expect(savedKindOf([{ ...photo(1), kind: "voice" } as AttachmentRefBytes])).toBe("voice");
    const file = { ...photo(1, false), mime: "application/pdf", name: "a.pdf" } as AttachmentRefBytes;
    const e = saveEdit({ ref: G1, messageId: 1, author: "A", text: "", attachments: [file], sentAtMs: 0 }, 9);
    expect(e).toMatchObject({ t: "saved", attachmentKind: "file", excerpt: "a.pdf" });
    expect(savedKindLabel("file", "a.pdf")).toEqual(["saved.kind.file", { name: "a.pdf" }]);
    expect(savedKindLabel("voice", "")).toEqual(["saved.kind.voice"]);
    expect(savedKindLabel("", "hi")).toBeNull();
  });
});

describe("the Saved screen's rows", () => {
  const entry = (messageId: number, savedAtMs: number, ref = G1) => ({
    ref,
    messageId,
    savedAtMs,
    sentAtMs: 0,
    author: "A",
    excerpt: "e",
    attachmentKind: "photo",
    blobIdsHex: [],
  });

  it("most recently saved first; a chat not held reads from the snapshot and does not open", () => {
    const rows = savedRows(
      [entry(1, 100), entry(2, 300), entry(3, 200, G2)],
      (ref) => (ref === G1 ? "Group" : null),
      (_ref, id) => id === 1,
    );
    expect(rows.map((r) => r.messageId)).toEqual([2, 3, 1]);
    expect(rows.find((r) => r.messageId === 3)).toMatchObject({ chatTitle: null, held: false });
    expect(rows.find((r) => r.messageId === 2)).toMatchObject({ chatTitle: "Group", held: false });
    expect(rows.find((r) => r.messageId === 1)).toMatchObject({ held: true, kind: "photo" });
  });

  it("an unknown kind token reads as text", () => {
    const [row] = savedRows([{ ...entry(1, 1), attachmentKind: "hologram" }], () => "T", () => true);
    expect(row?.kind).toBe("");
  });
});

describe("the message's own time", () => {
  it("travels in the snapshot and wins over the save time", () => {
    const s = new wasm.SyncedState();
    apply(s, [saveEdit({ ref: G1, messageId: 3, author: "A", text: "t", attachments: [], sentAtMs: 1_700_000_000_000 }, 1_800_000_000_000)]);
    const [e] = projectSyncedState(s).saved;
    expect(e?.sentAtMs).toBe(1_700_000_000_000);
    s.free();
    const [row] = savedRows([e!], () => "T", () => true);
    expect(savedRowDateMs(row!, 1_600_000_000_000)).toBe(1_700_000_000_000);
    expect(savedRowDateMs({ ...row!, sentAtMs: 0 }, 1_600_000_000_000)).toBe(1_600_000_000_000);
    expect(savedRowDateMs({ ...row!, sentAtMs: 0 }, null)).toBe(1_800_000_000_000);
  });

  it("an unknown time is 0 and survives the outbox", () => {
    const e = saveEdit({ ref: G1, messageId: 3, author: "A", text: "t", attachments: [], sentAtMs: Number.NaN }, 5);
    expect(e).toMatchObject({ sentAtMs: 0n });
    const stamped = stampEdits([e], mint);
    expect(decodeSyncState(encodeSyncState({ synced: new Uint8Array(), pending: [stamped] }))?.pending).toEqual([stamped]);
  });
});

describe("a delete for everyone", () => {
  it("names the saved entries of that conversation and those messages only", () => {
    const entries = [
      { ref: G1, messageId: 1, savedAtMs: 1, sentAtMs: 0, author: "", excerpt: "", attachmentKind: "", blobIdsHex: [] },
      { ref: G1, messageId: 2, savedAtMs: 1, sentAtMs: 0, author: "", excerpt: "", attachmentKind: "", blobIdsHex: [] },
      { ref: G2, messageId: 1, savedAtMs: 1, sentAtMs: 0, author: "", excerpt: "", attachmentKind: "", blobIdsHex: [] },
    ];
    expect(savedEntriesOf(entries, "01".repeat(32), [1, 9]).map((e) => [e.ref.kind, e.messageId])).toEqual([[2, 1]]);
  });
});

describe("saving across conversation segments (rule of 2026-10-27)", () => {
  const A = new Uint8Array(32).fill(0xa1);
  const B = new Uint8Array(32).fill(0xb2);
  const C = new Uint8Array(32).fill(0xc3);
  const b64 = (u: Uint8Array): string => btoa(String.fromCharCode(...u));

  it("a head row names the head group and its DS id", () => {
    expect(savedTargetOf(5, A, [])).toEqual({ groupId: A, messageId: 5 });
    expect(savedTargetOf(0, A, [])).toBeNull();
  });

  it("an older segment's row saves under its own segment and DS id", () => {
    // B continues A, C continues B: A is two segments back, B one.
    expect(savedTargetOf(historyRowId(7, 1), C, [A, B])).toEqual({ groupId: B, messageId: 7 });
    expect(savedTargetOf(historyRowId(7, 2), C, [A, B])).toEqual({ groupId: A, messageId: 7 });
    expect(savedTargetOf(historyRowId(7, 3), C, [A, B])).toBeNull();
  });

  it("the same message is found by the same register before and after its conversation moves", () => {
    const fold = new wasm.SyncedState();
    const before = savedTargetOf(5, A, [])!;
    apply(fold, [saveEdit({ ref: { kind: 2, id: before.groupId }, messageId: before.messageId, author: "A", text: "t", attachments: [], sentAtMs: 0 }, 1)]);
    const keys = savedKeySet(projectSyncedState(fold).saved);
    // A is now history of B: the row is drawn under a history id.
    const after = savedTargetOf(historyRowId(5, 1), B, [A])!;
    expect(keys.has(savedEntryKey("a1".repeat(32), after.messageId))).toBe(true);
    expect(after.groupId).toEqual(A);
    fold.free();
  });

  it("opens in the conversation that holds the segment", () => {
    expect(savedOpenRow(b64(C), 9, b64(C), [b64(A), b64(B)])).toBe(9);
    expect(savedOpenRow(b64(B), 9, b64(C), [b64(A), b64(B)])).toBe(historyRowId(9, 1));
    expect(savedOpenRow(b64(A), 9, b64(C), [b64(A), b64(B)])).toBe(historyRowId(9, 2));
    expect(savedOpenRow(b64(new Uint8Array(32).fill(9)), 9, b64(C), [b64(A)])).toBeNull();
  });

  it("a delete for everyone removes by segment", () => {
    const entries = [
      { ref: { kind: 2 as const, id: A }, messageId: 4, savedAtMs: 1, sentAtMs: 0, author: "", excerpt: "", attachmentKind: "", blobIdsHex: [] },
      { ref: { kind: 2 as const, id: B }, messageId: 4, savedAtMs: 1, sentAtMs: 0, author: "", excerpt: "", attachmentKind: "", blobIdsHex: [] },
    ];
    expect(savedEntriesOf(entries, "b2".repeat(32), [4]).map((e) => e.ref.id)).toEqual([B]);
  });
});
