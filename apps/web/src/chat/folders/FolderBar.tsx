// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The folder bar (`docs/ux/chat-folders.md` 2.1): one horizontal strip of
 * tabs between the search field and the list. All chats first and fixed,
 * then the person's folders in their order, each with the number of
 * conversations in it that have anything unread (muted ones not counted).
 * The selected tab is the accent label with an accent underline. Scrolls
 * sideways when it overflows, keeping the selected tab in view.
 *
 * Click selects; right-click (or a long press on touch) opens the tab menu.
 * Arrow keys move between tabs, as in any tab list.
 */
import { createEffect, For, Show, type JSX } from "solid-js";

import { createLongPress } from "../../ui/long-press";
import { t } from "../../l10n";
import { tabAriaLabel } from "./folder-model";

export interface FolderTab {
  /** `"all"` or the folder id in hex. */
  readonly id: string;
  readonly name: string;
  /** Unread conversations; 0 draws no count (All chats always 0). */
  readonly count: number;
}

export function FolderBar(props: {
  tabs: readonly FolderTab[];
  selected: string;
  onSelect: (id: string) => void;
  onMenu: (id: string, x: number, y: number) => void;
}): JSX.Element {
  let strip: HTMLDivElement | undefined;

  // Keep the selected tab scrolled into view.
  createEffect(() => {
    const id = props.selected;
    const el = strip?.querySelector<HTMLElement>(`[data-tab="${CSS.escape(id)}"]`);
    el?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
  });

  const focusTab = (index: number): void => {
    const buttons = strip?.querySelectorAll<HTMLButtonElement>('[role="tab"]');
    if (buttons === undefined || buttons.length === 0) return;
    const i = (index + buttons.length) % buttons.length;
    const b = buttons[i];
    if (b === undefined) return;
    b.focus();
    const id = b.dataset.tab;
    if (id !== undefined) props.onSelect(id);
  };

  return (
    <div class="lk-folder-bar" role="tablist" aria-label={t("folders.bar_a11y")} ref={strip}>
      <For each={props.tabs}>
        {(tab, i) => {
          const press = createLongPress((x, y) => props.onMenu(tab.id, x, y));
          const selected = (): boolean => props.selected === tab.id;
          return (
            <button
              type="button"
              role="tab"
              class="lk-folder-tab"
              classList={{ selected: selected() }}
              data-tab={tab.id}
              aria-selected={selected()}
              tabIndex={selected() ? 0 : -1}
              aria-label={tabAriaLabel(tab.name, tab.count)}
              onClick={(e) => {
                if (press.swallowClick(e)) return;
                props.onSelect(tab.id);
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                props.onMenu(tab.id, e.clientX, e.clientY);
              }}
              onKeyDown={(e) => {
                if (e.key === "ArrowRight") {
                  e.preventDefault();
                  focusTab(i() + 1);
                } else if (e.key === "ArrowLeft") {
                  e.preventDefault();
                  focusTab(i() - 1);
                } else if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
                  e.preventDefault();
                  const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                  props.onMenu(tab.id, r.left, r.bottom);
                }
              }}
              onPointerDown={press.onPointerDown}
              onPointerMove={press.onPointerMove}
              onPointerUp={press.onPointerUp}
              onPointerCancel={press.onPointerCancel}
              onPointerLeave={press.onPointerLeave}
            >
              <span class="lk-folder-tab-name">{tab.name}</span>
              <Show when={tab.count > 0}>
                <span class="lk-folder-tab-count" aria-hidden="true">
                  {tab.count}
                </span>
              </Show>
            </button>
          );
        }}
      </For>
    </div>
  );
}
