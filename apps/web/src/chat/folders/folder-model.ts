// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Chat folders on the web, the pure part (`docs/ux/chat-folders.md`,
 * ADR-0058).
 *
 * The RULE that decides which conversations a folder holds, and in what
 * order, is not here: it is `folder_matches` / `order_conversations_in_folder`
 * in `crates/lakko-ui-rules`, reached over wasm. The FOLD that merges edits
 * from several devices is not here either: it is `lakko_sync::SyncedState`.
 * What this module owns is the web's side of both boundaries:
 *
 * - the folder as the web reads it ({@link FolderModel}), projected from the
 *   synced state once per change;
 * - the mapping from a folder's conversation references to the row ids the
 *   list draws, and back ({@link rowIdOfRef}, {@link refOfRow});
 * - what an edit writes ({@link RegisterEdit}): only the registers that
 *   changed, never a register this device did not touch (ADR-0058 section
 *   5a), which is what keeps a half-synced device from deleting a chat it
 *   cannot see from every other device's folder.
 *
 * No wasm and no Solid here, so vitest runs it in bare node.
 */

import { t } from "../../l10n";

// ---------- bits (the wire's, ADR-0058 section 2) --------------------------

export const INCLUDE_ONE_TO_ONE = 1 << 0;
export const INCLUDE_GROUPS = 1 << 1;
export const INCLUDE_CHANNELS = 1 << 2;
export const EXCLUDE_MUTED = 1 << 0;
export const EXCLUDE_READ = 1 << 1;
export const EXCLUDE_ARCHIVED = 1 << 2;

// ---------- the person's limits (UX spec section 3.4) ----------------------

/** Folders per account. */
export const MAX_FOLDERS = 20;
/** Characters in a folder name. */
export const MAX_FOLDER_NAME_CHARS = 24;
/** Chats added by hand to one folder. */
export const MAX_INCLUDED_BY_HAND = 200;
/** Chats excluded by hand from one folder. */
export const MAX_EXCLUDED_BY_HAND = 200;
/** Pins inside one folder. */
export const MAX_FOLDER_PINS = 10;

// ---------- references ------------------------------------------------------

/** The wire's reference kind: `1` one-to-one, `2` group, `3` channel. */
export type RefKind = 1 | 2 | 3;

/** A conversation a folder names: `(kind, id)`, id the raw bytes (the MLS
 *  group id for a one-to-one chat or a group, the channel id for a channel). */
export interface ConversationRef {
  readonly kind: RefKind;
  readonly id: Uint8Array;
}

/** The kind of a row in the merged list, as `conversation-list.ts` names it. */
export type RowKind = "chat" | "group" | "channel";

/**
 * The id a folder rule sees for a row.
 *
 * The list's own row ids are the group id in base64 for a chat or a group and
 * the channel id in hex for a channel - two encodings of two different id
 * spaces, which in practice never meet, but "in practice" is not a property.
 * The folder rule compares ids as strings with no kind attached, so the web
 * prefixes the namespace itself: `g:` for anything addressed by an MLS group
 * id (a one-to-one chat and a group are one namespace, and a chat that became
 * a group keeps its id), `c:` for a channel. A collision is then impossible
 * by construction rather than unlikely.
 */
export function ruleRowId(kind: RowKind, rowId: string): string {
  return kind === "channel" ? `c:${rowId}` : `g:${rowId}`;
}

/** The rule id of a folder reference, or `null` for a kind this build does
 *  not know (never produced: the decoder refuses it). */
export function ruleIdOfRef(ref: ConversationRef): string | null {
  switch (ref.kind) {
    case 1:
    case 2:
      return `g:${bytesToB64(ref.id)}`;
    case 3:
      return `c:${bytesToHex(ref.id)}`;
    default:
      return null;
  }
}

/** The reference for a row the person acts on. A chat and a group are told
 *  apart only for the wire; the rule matches them by id either way. */
export function refOfRow(kind: RowKind, rowId: string): ConversationRef | null {
  if (kind === "channel") {
    const id = hexToBytes(rowId);
    return id === null ? null : { kind: 3, id };
  }
  const id = b64ToBytes(rowId);
  if (id === null) return null;
  return { kind: kind === "chat" ? 1 : 2, id };
}

/** Whether two references name the same conversation. A one-to-one and a
 *  group reference with the same id are the same conversation (a chat that
 *  grew into a group), exactly as the rule sees them. */
export function sameConversation(a: ConversationRef, b: ConversationRef): boolean {
  return ruleIdOfRef(a) === ruleIdOfRef(b);
}

// ---------- the folder as the web reads it --------------------------------

/** One live folder, projected from `SyncedState.folders()`. */
export interface FolderModel {
  /** Hex of the 16-byte folder id - the key everything here uses. */
  readonly idHex: string;
  readonly name: string;
  readonly includeKinds: number;
  readonly excludeFlags: number;
  /** Added by hand (`INCLUDED` members). */
  readonly included: readonly ConversationRef[];
  /** Excluded by hand (`EXCLUDED` members). */
  readonly excluded: readonly ConversationRef[];
  /** The folder's own pins, top first. */
  readonly pinned: readonly ConversationRef[];
}

/** The folder's rule, in the rule's own terms (row ids prefixed as
 *  {@link ruleRowId} makes them). */
export interface RuleInputs {
  readonly includeKinds: number;
  readonly excludeFlags: number;
  readonly included: string[];
  readonly excluded: string[];
}

export function ruleInputsOf(folder: FolderModel): RuleInputs {
  const ids = (refs: readonly ConversationRef[]): string[] =>
    refs.map(ruleIdOfRef).filter((x): x is string => x !== null);
  return {
    includeKinds: folder.includeKinds,
    excludeFlags: folder.excludeFlags,
    included: ids(folder.included),
    excluded: ids(folder.excluded),
  };
}

/** The folder's pins as rule ids, in their stored order. */
export function pinIdsOf(folder: FolderModel): string[] {
  return folder.pinned.map(ruleIdOfRef).filter((x): x is string => x !== null);
}

// ---------- what an edit writes (ADR-0058 section 1) -----------------------

/** Member state on the wire: `0` none (the element tombstone), `1` included,
 *  `2` excluded. */
export type MemberState = 0 | 1 | 2;

/** One register an edit writes. The stamp is added when the edit is sent
 *  ({@link StampedEdit}), so a builder never reads the clock. */
export type RegisterEdit =
  | {
      readonly t: "meta";
      readonly folderIdHex: string;
      readonly name: string;
      readonly includeKinds: number;
      readonly excludeFlags: number;
    }
  | {
      readonly t: "member";
      readonly folderIdHex: string;
      readonly ref: ConversationRef;
      readonly state: MemberState;
    }
  | { readonly t: "pins"; readonly folderIdHex: string; readonly pinned: readonly ConversationRef[] }
  | { readonly t: "removed"; readonly folderIdHex: string }
  | { readonly t: "order"; readonly folderIdsHex: readonly string[] }
  /** ADR-0065 section 4 — save a message (the snapshot a row is drawn from
   *  before the message is held, and its blob ids for the keeps). */
  | {
      readonly t: "saved";
      readonly ref: ConversationRef;
      readonly messageId: bigint;
      readonly savedAtMs: bigint;
      /** The message's own time, unix ms; `0n` when not known. */
      readonly sentAtMs: bigint;
      readonly author: string;
      readonly excerpt: string;
      readonly attachmentKind: string;
      readonly blobIdsHex: readonly string[];
    }
  /** Remove a message from saved (a tombstone; saving it again brings it
   *  back). */
  | { readonly t: "saved-removed"; readonly ref: ConversationRef; readonly messageId: bigint };

/** A register edit with its own HLC stamp. */
export type StampedEdit = RegisterEdit & { readonly hlc: bigint };

/** Stamp every register with a fresh HLC from `mint` (strictly increasing). */
export function stampEdits(
  edits: readonly RegisterEdit[],
  mint: () => bigint,
): StampedEdit[] {
  return edits.map((e) => ({ ...e, hlc: mint() }));
}

/** What the editor holds while the person edits. */
export interface FolderDraft {
  readonly name: string;
  readonly includeKinds: number;
  readonly excludeFlags: number;
  readonly included: readonly ConversationRef[];
  readonly excluded: readonly ConversationRef[];
}

/** A new folder's starting draft: nothing included, Archived excluded
 *  (UX spec 3.1). `preAdd` is the chat "New folder..." was opened from. */
export function newFolderDraft(preAdd?: ConversationRef): FolderDraft {
  return {
    name: "",
    includeKinds: 0,
    excludeFlags: EXCLUDE_ARCHIVED,
    included: preAdd === undefined ? [] : [preAdd],
    excluded: [],
  };
}

export function draftOf(folder: FolderModel): FolderDraft {
  return {
    name: folder.name,
    includeKinds: folder.includeKinds,
    excludeFlags: folder.excludeFlags,
    included: folder.included,
    excluded: folder.excluded,
  };
}

/** Whether Save may be pressed: a name, and something included (UX 3.1). */
export function draftCanSave(draft: FolderDraft, isValidName: (n: string) => boolean): boolean {
  const name = draft.name.trim();
  if (name.length === 0 || !isValidName(name)) return false;
  return draft.includeKinds !== 0 || draft.included.length > 0;
}

/** Whether the draft differs from what it was opened on. */
export function draftIsDirty(original: FolderDraft, draft: FolderDraft): boolean {
  return (
    original.name !== draft.name ||
    original.includeKinds !== draft.includeKinds ||
    original.excludeFlags !== draft.excludeFlags ||
    !sameRefSet(original.included, draft.included) ||
    !sameRefSet(original.excluded, draft.excluded)
  );
}

function sameRefSet(a: readonly ConversationRef[], b: readonly ConversationRef[]): boolean {
  if (a.length !== b.length) return false;
  const ids = new Set(a.map(ruleIdOfRef));
  return b.every((r) => ids.has(ruleIdOfRef(r)));
}

/**
 * The registers a Save writes: the meta register if the name or a toggle
 * changed (always, for a new folder), and one member register for each
 * conversation whose hand state changed - `INCLUDED`, `EXCLUDED`, or `NONE`
 * for one taken out of either list. Nothing else.
 *
 * `original` is `null` for a new folder. A conversation this device cannot
 * resolve is still in `original`'s lists and, unless the person removed it
 * from the draft, still in the draft's, so it produces no register and its
 * existing one is never rewritten.
 */
export function diffFolderDraft(
  folderIdHex: string,
  original: FolderDraft | null,
  draft: FolderDraft,
): RegisterEdit[] {
  const edits: RegisterEdit[] = [];
  const name = draft.name.trim();
  if (
    original === null ||
    original.name !== name ||
    original.includeKinds !== draft.includeKinds ||
    original.excludeFlags !== draft.excludeFlags
  ) {
    edits.push({
      t: "meta",
      folderIdHex,
      name,
      includeKinds: draft.includeKinds,
      excludeFlags: draft.excludeFlags,
    });
  }
  const stateOf = (d: FolderDraft | null): Map<string, { ref: ConversationRef; s: MemberState }> => {
    const m = new Map<string, { ref: ConversationRef; s: MemberState }>();
    if (d === null) return m;
    for (const r of d.included) {
      const k = ruleIdOfRef(r);
      if (k !== null) m.set(k, { ref: r, s: 1 });
    }
    // Excluded beats included, as in the rule; a draft never holds both.
    for (const r of d.excluded) {
      const k = ruleIdOfRef(r);
      if (k !== null) m.set(k, { ref: r, s: 2 });
    }
    return m;
  };
  const before = stateOf(original);
  const after = stateOf(draft);
  for (const [k, v] of after) {
    if (before.get(k)?.s !== v.s) {
      edits.push({ t: "member", folderIdHex, ref: v.ref, state: v.s });
    }
  }
  for (const [k, v] of before) {
    if (!after.has(k)) edits.push({ t: "member", folderIdHex, ref: v.ref, state: 0 });
  }
  return edits;
}

// ---------- the row menu's "Add to folder" (UX spec section 5) -------------

/** How a conversation relates to a folder, for the Add-to-folder list. */
export type Membership =
  | "in-by-hand"
  | "in-by-kind"
  | "excluded-by-hand"
  | "not-in";

/**
 * Classify one row against one folder. `inFolder` is the shared rule's answer
 * (`folderMatches`), which the checkmark shows; the hand lists decide what a
 * tap writes.
 */
export function membershipOf(
  folder: FolderModel,
  ref: ConversationRef,
  inFolder: boolean,
): Membership {
  if (folder.excluded.some((r) => sameConversation(r, ref))) return "excluded-by-hand";
  if (!inFolder) return "not-in";
  return folder.included.some((r) => sameConversation(r, ref)) ? "in-by-hand" : "in-by-kind";
}

/**
 * The one register a tap on a folder in the Add-to-folder list writes:
 *
 * - not in it: add by hand;
 * - excluded by hand: remove the exclusion instead;
 * - in it by hand: take it out (`NONE`);
 * - in it by a kind toggle: exclude it by hand - the person asked to take it
 *   out, and how the folder was built is not their problem.
 *
 * "Remove from folder" is the same as tapping a checked folder.
 */
export function toggleMembershipEdit(
  folder: FolderModel,
  ref: ConversationRef,
  membership: Membership,
): RegisterEdit {
  const state: MemberState =
    membership === "not-in" ? 1 : membership === "in-by-kind" ? 2 : 0;
  // A hand-listed ref keeps the exact bytes the folder already holds.
  const held =
    folder.included.find((r) => sameConversation(r, ref)) ??
    folder.excluded.find((r) => sameConversation(r, ref)) ??
    ref;
  return { t: "member", folderIdHex: folder.idHex, ref: held, state };
}

/** A pin toggle inside a folder: the whole pin list, as one value. `null`
 *  when pinning would pass the limit. */
export function togglePinEdit(folder: FolderModel, ref: ConversationRef): RegisterEdit | null {
  const idx = folder.pinned.findIndex((r) => sameConversation(r, ref));
  if (idx >= 0) {
    return {
      t: "pins",
      folderIdHex: folder.idHex,
      pinned: folder.pinned.filter((_, i) => i !== idx),
    };
  }
  if (folder.pinned.length >= MAX_FOLDER_PINS) return null;
  return { t: "pins", folderIdHex: folder.idHex, pinned: [ref, ...folder.pinned] };
}

/** Move the folder at `from` to `to` in the bar order. */
export function reorderEdit(folders: readonly FolderModel[], from: number, to: number): RegisterEdit | null {
  if (from === to || from < 0 || to < 0 || from >= folders.length || to >= folders.length) {
    return null;
  }
  const ids = folders.map((f) => f.idHex);
  const [moved] = ids.splice(from, 1);
  if (moved === undefined) return null;
  ids.splice(to, 0, moved);
  return { t: "order", folderIdsHex: ids };
}

/**
 * The order register a CREATE writes beside the new folder's registers: the
 * folders as the bar shows them, then the new one. Without it the new folder
 * sits outside the order, where the fold places it by the stamp of its
 * CURRENT meta register (ADR-0058), so every later rename moved it to the end
 * of the bar (seen live 2026-09-30).
 */
export function appendToOrderEdit(folders: readonly FolderModel[], newFolderIdHex: string): RegisterEdit {
  const ids = folders.map((f) => f.idHex).filter((id) => id !== newFolderIdHex);
  ids.push(newFolderIdHex);
  return { t: "order", folderIdsHex: ids };
}

// ---------- the Suggested list (UX spec section 4) -------------------------

export interface Suggestion {
  readonly key: "unread" | "personal" | "groups" | "channels";
  /** The suggested name in the interface language at the time of the call;
   *  adding the suggestion stores this text as the folder's own name. */
  readonly name: () => string;
  readonly description: () => string;
  readonly includeKinds: number;
  readonly excludeFlags: number;
}

const ALL_KINDS = INCLUDE_ONE_TO_ONE | INCLUDE_GROUPS | INCLUDE_CHANNELS;

export const SUGGESTIONS: readonly Suggestion[] = [
  {
    key: "unread",
    name: () => t("folders.suggested.unread.name"),
    description: () => t("folders.suggested.unread.description"),
    includeKinds: ALL_KINDS,
    excludeFlags: EXCLUDE_READ,
  },
  {
    key: "personal",
    name: () => t("folders.suggested.personal.name"),
    description: () => t("folders.suggested.personal.description"),
    includeKinds: INCLUDE_ONE_TO_ONE,
    excludeFlags: 0,
  },
  {
    key: "groups",
    name: () => t("folders.suggested.groups.name"),
    description: () => t("folders.suggested.groups.description"),
    includeKinds: INCLUDE_GROUPS,
    excludeFlags: 0,
  },
  {
    key: "channels",
    name: () => t("folders.suggested.channels.name"),
    description: () => t("folders.suggested.channels.description"),
    includeKinds: INCLUDE_CHANNELS,
    excludeFlags: 0,
  },
];

/** Suggestions the person does not already have, compared by RULE, not by
 *  name: a folder with the same toggles and no hand lists is that suggestion
 *  whatever it is called. */
export function openSuggestions(folders: readonly FolderModel[]): Suggestion[] {
  return SUGGESTIONS.filter(
    (s) =>
      !folders.some(
        (f) =>
          f.includeKinds === s.includeKinds &&
          f.excludeFlags === s.excludeFlags &&
          f.included.length === 0 &&
          f.excluded.length === 0,
      ),
  );
}

/** The registers adding a suggestion writes: one meta register. */
export function suggestionEdit(folderIdHex: string, s: Suggestion): RegisterEdit {
  return {
    t: "meta",
    folderIdHex,
    name: s.name(),
    includeKinds: s.includeKinds,
    excludeFlags: s.excludeFlags,
  };
}

// ---------- counts ----------------------------------------------------------

/** What the folder count reads per row. */
export interface CountRow {
  readonly muted: boolean;
  readonly hasUnread: boolean;
}

/** The tab count (UX 2.1): conversations with anything unread, muted ones
 *  not counted. Conversations, not messages. */
export function unreadConversationCount(rows: readonly CountRow[]): number {
  return rows.filter((r) => r.hasUnread && !r.muted).length;
}

/** "12 chats" (and "1 chat"). */
export function chatCountLabel(n: number): string {
  return t("folders.chat_count", { count: n });
}

/** The tab's accessible label (UX 9): "Work, 3 unread chats". */
export function tabAriaLabel(name: string, count: number): string {
  if (count === 0) return name;
  return t("folders.tab_a11y", { name, count });
}

/** Clip a typed name to the limit, by characters (code points) rather than
 *  UTF-16 units, so an emoji is never cut in half. */
export function clipFolderName(typed: string): string {
  const chars = Array.from(typed);
  return chars.length <= MAX_FOLDER_NAME_CHARS ? typed : chars.slice(0, MAX_FOLDER_NAME_CHARS).join("");
}

// ---------- bytes ------------------------------------------------------------

export function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

export function hexToBytes(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

export function bytesToB64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

export function b64ToBytes(text: string): Uint8Array | null {
  try {
    const raw = atob(text);
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i += 1) out[i] = raw.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}
