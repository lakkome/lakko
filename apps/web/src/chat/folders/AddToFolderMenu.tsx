// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Add to folder" in a conversation row's menu (`docs/ux/chat-folders.md`
 * section 5): a submenu of the person's folders, each checked when the
 * conversation is currently IN it (by any rule), then "New folder...".
 *
 * What a tap writes is `toggleMembershipEdit`'s: add by hand, lift a hand
 * exclusion, take a hand-added chat out, or - for a chat that is in by a kind
 * toggle - exclude it by hand. With no folders yet, the item goes straight
 * to the editor.
 */
import { createSignal, For, Show, type JSX } from "solid-js";

import { useDeviceGroupSync } from "../../devices/sync/DeviceGroupSyncContext";
import { t } from "../../l10n";
import { Icon } from "../../ui/icons";
import type { ConversationRow } from "../conversation-list";
import { uiRules } from "../ui-rules";
import { canCreateFolder, openFolderEditor, toggleMembership } from "./folder-actions";
import { MAX_FOLDERS, membershipOf, type ConversationRef, type FolderModel } from "./folder-model";
import { rowIsInFolder } from "./folder-view";

export function AddToFolderItems(props: {
  row: ConversationRow;
  conversation: ConversationRef;
  folders: readonly FolderModel[];
  onDone: () => void;
}): JSX.Element {
  const sync = useDeviceGroupSync();
  const [open, setOpen] = createSignal(false);

  const inFolder = (f: FolderModel): boolean => {
    const rules = uiRules();
    return rules !== null && rowIsInFolder(props.row, f, rules);
  };

  // Read props BEFORE `onDone`: it closes the menu, and the props are read
  // through the menu's `<Show>` accessor, which is stale once it closes.
  const newFolder = (): void => {
    const preAdd = props.conversation;
    props.onDone();
    openFolderEditor({ mode: "new", preAdd });
  };

  return (
    <div
      class="lk-menu-subanchor"
      onMouseLeave={() => setOpen(false)}
      onKeyDown={(e) => {
        if (e.key === "ArrowLeft" || e.key === "Escape") setOpen(false);
      }}
    >
      <div class="lk-menu-sep" />
      <button
        type="button"
        role="menuitem"
        aria-haspopup={props.folders.length > 0 ? "menu" : undefined}
        aria-expanded={props.folders.length > 0 ? open() : undefined}
        disabled={props.folders.length === 0 && !canCreateFolder(props.folders)}
        onMouseEnter={() => setOpen(true)}
        onKeyDown={(e) => {
          if (e.key === "ArrowRight") setOpen(true);
        }}
        onClick={() => {
          if (props.folders.length === 0) newFolder();
          else setOpen((o) => !o);
        }}
      >
        <Icon name="folder" size={18} />
        {t("folders.add_to_folder")}
        <Show when={props.folders.length > 0}>
          <Icon name="chevron" size={14} class="lk-menu-sub-chevron" />
        </Show>
      </button>
      <Show when={open() && props.folders.length > 0}>
        <div class="lk-menu lk-menu-sub" role="menu" aria-label={t("folders.add_to_folder")}>
          <For each={props.folders}>
            {(f) => {
              const checked = (): boolean => inFolder(f);
              return (
                <button
                  type="button"
                  role="menuitemcheckbox"
                  aria-checked={checked()}
                  onClick={() => {
                    const conversation = props.conversation;
                    const m = membershipOf(f, conversation, checked());
                    props.onDone();
                    void toggleMembership(sync, f, conversation, m).catch((err: unknown) => {
                      // eslint-disable-next-line no-console
                      console.warn("add to folder failed", err);
                    });
                  }}
                >
                  <span class="lk-menu-check" aria-hidden="true">
                    <Show when={checked()}>
                      <Icon name="check" size={16} />
                    </Show>
                  </span>
                  <span class="lk-ellipsis">{f.name}</span>
                </button>
              );
            }}
          </For>
          <div class="lk-menu-sep" />
          <button
            type="button"
            role="menuitem"
            disabled={!canCreateFolder(props.folders)}
            onClick={newFolder}
          >
            <Icon name="plus" size={16} />
            {t("folders.new_ellipsis")}
          </button>
          <Show when={!canCreateFolder(props.folders)}>
            <p class="lk-menu-note">{t("folders.limit", { count: MAX_FOLDERS })}</p>
          </Show>
        </div>
      </Show>
    </div>
  );
}
