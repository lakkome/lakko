// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The chat-folder actions, wired to the sync runtime
 * (`docs/ux/chat-folders.md`, ADR-0058).
 *
 * Every action here turns a person's intent into the registers it changes
 * (`folder-model.ts`) and hands exactly those to
 * `DeviceGroupSyncApi.applyFolderEdits`, which stamps, folds and sends them.
 * Also the module-level state of the one folder editor on screen, so the
 * editor can be opened from the folder bar, a row menu and Settings alike.
 */
import { createSignal } from "solid-js";

import type { DeviceGroupSyncApi } from "../../devices/sync/DeviceGroupSyncContext";
import { requestConfirm } from "../../ui/confirm-dialog";
import {
  appendToOrderEdit,
  bytesToHex,
  MAX_FOLDERS,
  openSuggestions,
  reorderEdit,
  suggestionEdit,
  toggleMembershipEdit,
  togglePinEdit,
  type ConversationRef,
  type FolderModel,
  type Membership,
  type Suggestion,
} from "./folder-model";

type FolderSync = Pick<DeviceGroupSyncApi, "applyFolderEdits" | "folders">;

// ---------- the editor on screen -------------------------------------------

/** What the editor was opened on. */
export type EditorRequest =
  | { readonly mode: "edit"; readonly folderIdHex: string }
  | { readonly mode: "new"; readonly preAdd?: ConversationRef };

const [editorRequest, setEditorRequest] = createSignal<EditorRequest | null>(null);

/** The open editor, or `null`. Read by `FolderEditorHost`. */
export function currentEditorRequest(): EditorRequest | null {
  return editorRequest();
}

/** Open the folder editor. */
export function openFolderEditor(request: EditorRequest): void {
  setEditorRequest(request);
}

/** Close it (the editor asks about unsaved changes first). */
export function closeFolderEditor(): void {
  setEditorRequest(null);
}

// ---------- actions -----------------------------------------------------------

/** Whether a new folder may be made (UX 3.4: 20 per account). */
export function canCreateFolder(folders: readonly FolderModel[]): boolean {
  return folders.length < MAX_FOLDERS;
}

/** A fresh 16-byte folder id, minted once (ADR-0058: a rename is not a
 *  delete plus a create). */
export function newFolderIdHex(mint: () => Uint8Array): string {
  return bytesToHex(mint());
}

/** Add a suggested folder as is. */
export async function addSuggestion(
  sync: FolderSync,
  mint: () => Uint8Array,
  s: Suggestion,
): Promise<void> {
  if (!canCreateFolder(sync.folders())) return;
  if (!openSuggestions(sync.folders()).some((o) => o.key === s.key)) return;
  const idHex = newFolderIdHex(mint);
  await sync.applyFolderEdits([suggestionEdit(idHex, s), appendToOrderEdit(sync.folders(), idHex)]);
}

/** Delete a folder, after the confirmation of UX 3.5. Resolves `true` when
 *  it was deleted. */
export async function deleteFolder(sync: FolderSync, folder: FolderModel): Promise<boolean> {
  const ok = await requestConfirm({
    title: `Delete folder "${folder.name}"?`,
    body: "The chats in it stay in All chats.",
    confirmLabel: "Delete",
    cancelLabel: "Cancel",
    tone: "danger",
  });
  if (!ok) return false;
  await sync.applyFolderEdits([{ t: "removed", folderIdHex: folder.idHex }]);
  return true;
}

/** Move a folder in the bar order. */
export async function moveFolder(sync: FolderSync, from: number, to: number): Promise<void> {
  const edit = reorderEdit(sync.folders(), from, to);
  if (edit !== null) await sync.applyFolderEdits([edit]);
}

/** A tap on a folder in the Add-to-folder list, or Remove from folder. */
export async function toggleMembership(
  sync: FolderSync,
  folder: FolderModel,
  ref: ConversationRef,
  membership: Membership,
): Promise<void> {
  await sync.applyFolderEdits([toggleMembershipEdit(folder, ref, membership)]);
}

/** Pin / Unpin inside a folder. Resolves `false` when the pin limit was hit
 *  (the caller disables the item before that). */
export async function togglePinInFolder(
  sync: FolderSync,
  folder: FolderModel,
  ref: ConversationRef,
): Promise<boolean> {
  const edit = togglePinEdit(folder, ref);
  if (edit === null) return false;
  await sync.applyFolderEdits([edit]);
  return true;
}
