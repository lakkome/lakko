// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings > Chat folders (`docs/ux/chat-folders.md` section 4): the
 * person's folders in bar order with their match counts, New folder, and the
 * Suggested folders they do not already have.
 *
 * Reordering on the web is a drag handle plus Move up / Move down in each
 * row's menu for keyboard users; All chats is the first row, fixed, with no
 * handle. The one place the sync is mentioned: the empty-state line, and one
 * quiet line while an edit made here is still waiting to reach the other
 * devices.
 */
import { createMemo, createSignal, For, Show, type JSX } from "solid-js";

import { useDeviceGroupSync } from "../../devices/sync/DeviceGroupSyncContext";
import { t } from "../../l10n";
import { Icon } from "../../ui/icons";
import { createConversationRows } from "../conversation-rows";
import { uiRules } from "../ui-rules";
import {
  addSuggestion,
  canCreateFolder,
  deleteFolder,
  moveFolder,
  openFolderEditor,
} from "./folder-actions";
import { chatCountLabel, MAX_FOLDERS, openSuggestions, type FolderModel } from "./folder-model";
import { rowsInFolder } from "./folder-view";
import { MenuBackdrop } from "../../ui/MenuBackdrop";

export function ChatFoldersSection(): JSX.Element {
  const sync = useDeviceGroupSync();
  const rows = createConversationRows();

  const folders = (): readonly FolderModel[] => sync.folders();
  const suggestions = createMemo(() => openSuggestions(folders()));
  const canCreate = (): boolean => canCreateFolder(folders());

  const countOf = (f: FolderModel): string | null => {
    const rules = uiRules();
    if (rules === null) return null;
    return chatCountLabel(rowsInFolder(rows.orderedRows(), f, rules).length);
  };

  const [rowMenu, setRowMenu] = createSignal<{ index: number; x: number; y: number } | null>(null);
  const [dragFrom, setDragFrom] = createSignal<number | null>(null);
  const [dragOver, setDragOver] = createSignal<number | null>(null);

  const move = (from: number, to: number): void => {
    void moveFolder(sync, from, to).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("folder reorder failed", err);
    });
  };

  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.section.folders")}</h1>
      </div>

      <Show when={sync.pendingFolderSync()}>
        <p class="lk-folder-pending" role="status">
          {t("folders.settings.pending_sync")}
        </p>
      </Show>

      <Show when={folders().length === 0}>
        <p class="lk-folder-intro">
          {t("folders.settings.intro")}
        </p>
      </Show>

      <Show when={folders().length > 0}>
        <section class="lk-card">
          <h2>{t("folders.settings.yours")}</h2>
          <div class="lk-card-row lk-folder-row fixed">
            <span class="lk-folder-handle" aria-hidden="true" />
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">{t("folders.all_chats")}</span>
            </div>
          </div>
          <For each={folders()}>
            {(f, i) => (
              <div
                class="lk-card-row lk-folder-row"
                classList={{ "drag-over": dragOver() === i() && dragFrom() !== i() }}
                draggable={true}
                onDragStart={(e) => {
                  setDragFrom(i());
                  e.dataTransfer?.setData("text/plain", f.idHex);
                  if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
                }}
                onDragOver={(e) => {
                  if (dragFrom() === null) return;
                  e.preventDefault();
                  setDragOver(i());
                }}
                onDragLeave={() => setDragOver(null)}
                onDrop={(e) => {
                  e.preventDefault();
                  const from = dragFrom();
                  setDragFrom(null);
                  setDragOver(null);
                  if (from !== null && from !== i()) move(from, i());
                }}
                onDragEnd={() => {
                  setDragFrom(null);
                  setDragOver(null);
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setRowMenu({ index: i(), x: e.clientX, y: e.clientY });
                }}
              >
                <span class="lk-folder-handle" title={t("folders.settings.drag_hint")} aria-hidden="true">
                  <Icon name="drag-handle" size={20} />
                </span>
                <button
                  type="button"
                  class="lk-folder-row-open"
                  onClick={() => openFolderEditor({ mode: "edit", folderIdHex: f.idHex })}
                >
                  <span class="lk-card-row-title">{f.name}</span>
                  <Show when={countOf(f)}>
                    {(c) => <span class="lk-card-row-sub">{c()}</span>}
                  </Show>
                </button>
                <button
                  type="button"
                  class="lk-icon-button"
                  aria-label={t("folders.settings.more_a11y", { name: f.name })}
                  aria-haspopup="menu"
                  onClick={(e) => {
                    const r = e.currentTarget.getBoundingClientRect();
                    setRowMenu({ index: i(), x: r.left, y: r.bottom });
                  }}
                >
                  <Icon name="ellipsis" size={20} />
                </button>
              </div>
            )}
          </For>
        </section>
      </Show>

      <section class="lk-card">
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="folder-plus" size={20} />
          </span>
          <div class="lk-card-row-body">
            <button
              type="button"
              class="ghost lk-folder-new"
              disabled={!canCreate()}
              onClick={() => openFolderEditor({ mode: "new" })}
            >
              {t("folders.new")}
            </button>
            <Show when={!canCreate()}>
              <span class="lk-card-row-sub">{t("folders.limit", { count: MAX_FOLDERS })}</span>
            </Show>
          </div>
        </div>
      </section>

      <Show when={suggestions().length > 0}>
        <section class="lk-card">
          <h2>{t("folders.suggested.title")}</h2>
          <For each={suggestions()}>
            {(s) => (
              <div class="lk-card-row">
                <span class="lk-card-row-icon">
                  <Icon name="folder" size={20} />
                </span>
                <div class="lk-card-row-body">
                  <span class="lk-card-row-title">{s.name()}</span>
                  <span class="lk-card-row-sub">{s.description()}</span>
                </div>
                <button
                  type="button"
                  class="lk-card-row-action"
                  disabled={!canCreate()}
                  aria-label={t("folders.suggested.add_a11y", { name: s.name() })}
                  onClick={() => {
                    const rules = uiRules();
                    if (rules === null) return;
                    void addSuggestion(sync, () => rules.mintFolderId(), s).catch((err: unknown) => {
                      // eslint-disable-next-line no-console
                      console.warn("add suggested folder failed", err);
                    });
                  }}
                >
                  {t("folders.suggested.add")}
                </button>
              </div>
            )}
          </For>
        </section>
      </Show>

      <Show when={rowMenu()}>
        {(m) => {
          const f = (): FolderModel | undefined => folders()[m().index];
          const close = (): void => {
            setRowMenu(null);
          };
          return (
            <>
              <MenuBackdrop onClose={close} />
              <div
                class="lk-menu lk-menu-floating"
                role="menu"
                aria-label={f()?.name ?? ""}
                style={{ left: `${m().x}px`, top: `${m().y}px` }}
              >
                <button
                  type="button"
                  role="menuitem"
                  disabled={m().index === 0}
                  onClick={() => {
                    const i = m().index;
                    close();
                    move(i, i - 1);
                  }}
                >
                  {t("folders.settings.move_up")}
                </button>
                <button
                  type="button"
                  role="menuitem"
                  disabled={m().index >= folders().length - 1}
                  onClick={() => {
                    const i = m().index;
                    close();
                    move(i, i + 1);
                  }}
                >
                  {t("folders.settings.move_down")}
                </button>
                <button
                  type="button"
                  role="menuitem"
                  onClick={() => {
                    const folder = f();
                    close();
                    if (folder !== undefined) {
                      openFolderEditor({ mode: "edit", folderIdHex: folder.idHex });
                    }
                  }}
                >
                  {t("folders.edit")}
                </button>
                <button
                  type="button"
                  role="menuitem"
                  class="danger-item"
                  onClick={() => {
                    const folder = f();
                    close();
                    if (folder !== undefined) void deleteFolder(sync, folder);
                  }}
                >
                  {t("folders.delete")}
                </button>
              </div>
            </>
          );
        }}
      </Show>
    </>
  );
}
