// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The order of the pinned conversations, set by hand
 * (`docs/ux/navigation.md` section 2.2.1, `docs/ux/chat-folders.md` 2.3).
 *
 * **The rule is not here.** Where a pin sits, and what list a pin, an unpin or
 * a move writes, is `crates/lakko-ui-rules` (`orderConversations`,
 * `pinOrderAfterPin` / `pinOrderAfterUnpin` / `pinOrderAfterMove`,
 * `pinCanMove`), shared with iOS and Android. What this module owns is the
 * web's side of the boundary:
 *
 * - All chats: the synced `ConversationOrder` register holds BYTES, the same
 *   keys the synced flags register uses (a conversation id for a chat or a
 *   group, the 16-byte channel id for a channel); the rule speaks the row ids
 *   the list draws. {@link pinOrderView} maps one way, {@link PinOrderView.keysOf}
 *   back, and an entry no row on this device names keeps its exact bytes.
 * - A folder: its pin list holds conversation references; the rule speaks
 *   the folder rule's ids (`ruleRowId`). {@link folderPinsAfter} maps both ways
 *   the same way.
 *
 * No wasm and no Solid: vitest runs it in bare node with a fake for the rules.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { channelFlagsKey } from "../channels/channel-flags";
import type { ConversationRowKind } from "./conversation-list";
import {
  MAX_FOLDER_PINS,
  pinIdsOf,
  refOfRow,
  ruleIdOfRef,
  ruleRowId,
  type ConversationRef,
  type FolderModel,
  type RegisterEdit,
} from "./folders/folder-model";
import type { UiRules } from "./ui-rules";

/** The rules this module asks. */
export type PinOrderRules = Pick<
  UiRules,
  "pinOrderAfterPin" | "pinOrderAfterUnpin" | "pinOrderAfterMove" | "pinCanMove"
>;

/** What a person did to the pins. */
export type PinAction =
  | { readonly t: "pin"; readonly id: string }
  | { readonly t: "unpin"; readonly id: string }
  /** `toIndex` is the pin's place among the SHOWN pins after the move. */
  | { readonly t: "move"; readonly id: string; readonly toIndex: number };

/** The row fields the mapping reads. */
export interface PinOrderRow {
  readonly id: string;
  readonly kind: ConversationRowKind;
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/**
 * The key the synced registers use for a row: the conversation id (the row id
 * is its standard base64) for a chat or a group, the channel id (the row id is
 * its hex) for a channel. `null` for an id that does not decode.
 */
export function orderKeyOfRow(row: PinOrderRow): Uint8Array | null {
  if (row.kind === "channel") return channelFlagsKey(row.id);
  try {
    const bytes = base64Decode(row.id);
    return bytes.length === 0 ? null : bytes;
  } catch {
    return null;
  }
}

/**
 * The id of a stored key no row on this device names: the channel id form
 * (hex) for a 16-byte key, the conversation id form (standard base64)
 * otherwise - the encodings the list uses for its own ids. Only used as a
 * placeholder the helpers carry through; {@link PinOrderView.keysOf} writes the
 * original bytes back, never a re-decoding of this string.
 */
export function fallbackIdOfKey(key: Uint8Array): string {
  return key.length === 16 ? hexOf(key) : base64Encode(key);
}

/** The stored order as the rule reads it, and the way back to bytes. */
export interface PinOrderView {
  /** The stored order as row ids, top first, no repeats. */
  readonly storedIds: readonly string[];
  /** The bytes to write for a list of ids the helpers returned; an id that
   *  maps to nothing is left out. */
  keysOf(ids: readonly string[]): Uint8Array[];
}

/**
 * Map the stored `ConversationOrder` keys to the ids of `rows`. A key no row
 * names keeps its place under a placeholder id (the helpers keep entries this
 * device does not show; a pin made on another device whose conversation has
 * not arrived here must not be lost by a reorder made here).
 */
export function pinOrderView(
  storedKeys: readonly Uint8Array[],
  rows: readonly PinOrderRow[],
): PinOrderView {
  const idByKeyHex = new Map<string, string>();
  const keyById = new Map<string, Uint8Array>();
  for (const row of rows) {
    const key = orderKeyOfRow(row);
    if (key === null) continue;
    const hex = hexOf(key);
    if (!idByKeyHex.has(hex)) idByKeyHex.set(hex, row.id);
    if (!keyById.has(row.id)) keyById.set(row.id, key);
  }
  const storedIds: string[] = [];
  const seen = new Set<string>();
  for (const key of storedKeys) {
    if (key.length === 0) continue;
    const hex = hexOf(key);
    const id = idByKeyHex.get(hex) ?? fallbackIdOfKey(key);
    if (seen.has(id)) continue;
    seen.add(id);
    storedIds.push(id);
    if (!keyById.has(id)) keyById.set(id, new Uint8Array(key));
  }
  return {
    storedIds,
    keysOf(ids) {
      const out: Uint8Array[] = [];
      const written = new Set<string>();
      for (const id of ids) {
        const key = keyById.get(id);
        if (key === undefined) continue;
        const hex = hexOf(key);
        if (written.has(hex)) continue;
        written.add(hex);
        out.push(new Uint8Array(key));
      }
      return out;
    },
  };
}

/** The ids of the pinned rows, in the order the list draws them. */
export function shownPinIds(rows: readonly { readonly id: string; readonly pinned: boolean }[]): string[] {
  return rows.filter((r) => r.pinned).map((r) => r.id);
}

/** The whole list an action writes, as the rule computes it. */
export function pinIdsAfter(
  rules: PinOrderRules,
  stored: readonly string[],
  shown: readonly string[],
  action: PinAction,
): string[] {
  switch (action.t) {
    case "pin":
      return rules.pinOrderAfterPin([...stored], [...shown], action.id);
    case "unpin":
      return rules.pinOrderAfterUnpin([...stored], [...shown], action.id);
    case "move":
      return rules.pinOrderAfterMove(
        [...stored],
        [...shown],
        action.id,
        Math.max(0, Math.floor(action.toIndex)),
      );
  }
}

/**
 * All chats: the bytes to write to the `ConversationOrder` register for an
 * action. `rows` is the list as drawn (pinned rows in their drawn order).
 */
export function allChatsOrderAfter(
  rules: PinOrderRules,
  storedKeys: readonly Uint8Array[],
  rows: readonly (PinOrderRow & { readonly pinned: boolean })[],
  action: PinAction,
): Uint8Array[] {
  const view = pinOrderView(storedKeys, rows);
  const ids = pinIdsAfter(rules, view.storedIds, shownPinIds(rows), action);
  return view.keysOf(ids);
}

/** The pin ids of All chats, as `orderConversations` takes them. */
export function allChatsPinOrderIds(
  storedKeys: readonly Uint8Array[],
  rows: readonly PinOrderRow[],
): string[] {
  return [...pinOrderView(storedKeys, rows).storedIds];
}

/**
 * Move up (`up`) / Move down: the action, or `null` when it does not apply
 * (the first pin has no Move up, the last no Move down, one pin has neither).
 */
export function moveByOne(
  rules: PinOrderRules,
  shown: readonly string[],
  id: string,
  up: boolean,
): PinAction | null {
  if (!rules.pinCanMove([...shown], id, up)) return null;
  const at = shown.indexOf(id);
  if (at < 0) return null;
  return { t: "move", id, toIndex: up ? at - 1 : at + 1 };
}

/** What a person did to one row's folder pin. */
export type FolderPinAction =
  | { readonly t: "pin" }
  | { readonly t: "unpin" }
  | { readonly t: "move"; readonly toIndex: number };

/**
 * A folder: the `pins` edit an action writes to the folder's own pin list, or
 * `null` when it would write nothing (a pin past the folder's limit, an id
 * that names no row). `shownRows` are the folder's pinned rows in the order
 * the list draws them; `row` is the row acted on.
 *
 * A pin the folder holds whose conversation is not on this device keeps its
 * exact reference (and so its place); only a new pin is built from its row.
 */
export function folderPinsAfter(
  rules: PinOrderRules,
  folder: FolderModel,
  shownRows: readonly PinOrderRow[],
  row: PinOrderRow,
  action: FolderPinAction,
): RegisterEdit | null {
  const refById = new Map<string, ConversationRef>();
  for (const ref of folder.pinned) {
    const id = ruleIdOfRef(ref);
    if (id !== null && !refById.has(id)) refById.set(id, ref);
  }
  const id = ruleRowId(row.kind, row.id);
  if (action.t === "pin") {
    if (refById.has(id)) return null;
    if (folder.pinned.length >= MAX_FOLDER_PINS) return null;
    const ref = refOfRow(row.kind, row.id);
    if (ref === null) return null;
    refById.set(id, ref);
  }
  const stored = pinIdsOf(folder);
  const shown = shownRows.map((r) => ruleRowId(r.kind, r.id));
  const full: PinAction =
    action.t === "move" ? { t: "move", id, toIndex: action.toIndex } : { t: action.t, id };
  const ids = pinIdsAfter(rules, stored, shown, full);
  const pinned: ConversationRef[] = [];
  const seen = new Set<string>();
  for (const next of ids) {
    const ref = refById.get(next);
    if (ref === undefined || seen.has(next)) continue;
    seen.add(next);
    pinned.push(ref);
  }
  return { t: "pins", folderIdHex: folder.idHex, pinned };
}
