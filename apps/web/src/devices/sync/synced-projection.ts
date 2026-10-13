// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Between the Rust fold (`SyncedState`, ADR-0058) and the shapes the rest of
 * the web app reads.
 *
 * The fold of drafts, flags, read cursors, the conversation order and the
 * chat folders is one Rust object shared with iOS and Android. The app's
 * components were written against four plain maps keyed on the group id in
 * hex (`state.ts`), and they keep reading those: this module projects the
 * fold into them after every change, and builds the one wasm change set an
 * edit becomes. Nothing here decides who wins - that is the fold's.
 */

import type {
  FolderChangeSet as WasmFolderChangeSet,
  FolderConversationRef as WasmRef,
  SyncedState as WasmSyncedState,
} from "../../../wasm/lakko_ffi_wasm";
import type { Wasm } from "../../wasm";
import type { LineageEntry } from "../../chat/lineage";
import {
  bytesToHex,
  hexToBytes,
  type ConversationRef,
  type FolderModel,
  type RefKind,
  type StampedEdit,
} from "../../chat/folders/folder-model";
import {
  groupIdHex,
  type DraftState,
  type FlagsState,
  type OrderState,
  type ReadCursorState,
} from "./state";

/** Everything the app reads, as plain values. */
export interface SyncedProjection {
  readonly drafts: Readonly<Record<string, DraftState>>;
  readonly flags: Readonly<Record<string, FlagsState>>;
  readonly readCursors: Readonly<Record<string, ReadCursorState>>;
  readonly order: OrderState | null;
  readonly folders: readonly FolderModel[];
  /** ADR-0073 R.5 — the lineage registers (`successor -> root`). */
  readonly lineage: readonly LineageEntry[];
}

function refsOf(list: WasmRef[]): ConversationRef[] {
  const out: ConversationRef[] = [];
  for (const r of list) {
    const kind = r.kind;
    if (kind === 1 || kind === 2 || kind === 3) out.push({ kind: kind as RefKind, id: r.id });
    r.free();
  }
  return out;
}

/** Read the whole fold out. Frees every wasm object it reads. */
export function projectSyncedState(state: WasmSyncedState): SyncedProjection {
  const drafts: Record<string, DraftState> = {};
  for (const d of state.allDrafts()) {
    drafts[groupIdHex(d.groupId)] = { text: d.text, replyToId: d.replyToId, updatedAtHlc: d.hlc };
    d.free();
  }
  const flags: Record<string, FlagsState> = {};
  for (const f of state.allFlags()) {
    flags[groupIdHex(f.groupId)] = {
      pinned: f.pinned,
      muted: f.muted,
      archived: f.archived,
      unreadOverride: f.unreadOverride,
      updatedAtHlc: f.hlc,
    };
    f.free();
  }
  const readCursors: Record<string, ReadCursorState> = {};
  for (const c of state.allReadCursors()) {
    readCursors[groupIdHex(c.groupId)] = {
      upToMessageId: c.upToMessageId,
      readAtMs: c.readAtMs,
      updatedAtHlc: c.hlc,
    };
    c.free();
  }
  let order: OrderState | null = null;
  const o = state.conversationOrder();
  if (o !== undefined) {
    order = { groupIds: o.groupIds(), updatedAtHlc: o.hlc };
    o.free();
  }
  const folders: FolderModel[] = [];
  for (const v of state.folders()) {
    folders.push({
      idHex: bytesToHex(v.folderId),
      name: v.name,
      includeKinds: v.includeKinds,
      excludeFlags: v.excludeFlags,
      included: refsOf(v.included()),
      excluded: refsOf(v.excluded()),
      pinned: refsOf(v.pinned()),
    });
    v.free();
  }
  const lineage: LineageEntry[] = [];
  for (const l of state.allLineage()) {
    lineage.push({
      successorHex: groupIdHex(l.successorGroupId),
      rootHex: groupIdHex(l.rootConversationId),
      hlc: l.hlc,
    });
    l.free();
  }
  return { drafts, flags, readCursors, order, folders, lineage };
}

/**
 * The wasm change set for one edit: exactly the stamped registers given,
 * nothing else. The caller folds it locally AND sends it, then frees it.
 * Throws when a folder id is not 16 bytes or a register breaks a bound.
 */
export function buildFolderChangeSet(
  wasm: Pick<Wasm, "FolderChangeSet">,
  edits: readonly StampedEdit[],
): WasmFolderChangeSet {
  const cs = new wasm.FolderChangeSet();
  const fid = (hex: string): Uint8Array => {
    const bytes = hexToBytes(hex);
    if (bytes === null) throw new Error("folder id is not hex");
    return bytes;
  };
  try {
    for (const e of edits) {
      switch (e.t) {
        case "meta":
          cs.addMeta(e.hlc, fid(e.folderIdHex), e.name, e.includeKinds, e.excludeFlags);
          break;
        case "member":
          cs.addMember(e.hlc, fid(e.folderIdHex), e.ref.kind, e.ref.id, e.state);
          break;
        case "pins":
          cs.addPins(
            e.hlc,
            fid(e.folderIdHex),
            Uint8Array.from(e.pinned.map((r) => r.kind)),
            e.pinned.map((r) => r.id),
          );
          break;
        case "removed":
          cs.addRemoved(e.hlc, fid(e.folderIdHex));
          break;
        case "order":
          cs.setOrder(e.hlc, e.folderIdsHex.map(fid));
          break;
      }
    }
    cs.validate();
    return cs;
  } catch (err) {
    cs.free();
    throw err;
  }
}

// ---------- merging a restored fold (ADR-0058 section 5.2) -----------------

/**
 * The restore merge: fold everything the backup holds into this device's
 * fold, every register with its ORIGINAL stamp - member registers, pins,
 * tombstones and the folder order included (`SyncedState.mergeFrom`, the
 * Rust fold, pinned commutative and idempotent there). Never "take one side":
 * the newer register from each side survives. Mutates `local`; returns
 * whether anything changed.
 */
export function mergeRestoredFold(local: WasmSyncedState, backup: WasmSyncedState): boolean {
  return local.mergeFrom(backup);
}
