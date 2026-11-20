// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 8.2 — the checklist bubble. A done item is
 * muted and ticked (no strike-through: colour plus the tick carry it).
 */
import { createSignal, For, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import type { ChecklistItemRef, ChecklistItemView, ChecklistView } from "./types";
import "./bubbles.css";

/** UX 8.1 — an item is 1..=100 characters. */
export const CHECKLIST_ITEM_MAX_CHARS = 100;

export function ChecklistCard(props: {
  readonly view: ChecklistView;
  readonly onToggle: (ref: ChecklistItemRef, done: boolean) => void;
  readonly onAdd: (text: string) => void;
}): JSX.Element {
  const [adding, setAdding] = createSignal(false);
  const [draft, setDraft] = createSignal("");

  const label = (it: ChecklistItemView): string =>
    it.done
      ? t("checklist.item_done", { text: it.text, name: it.doneByName ?? "" })
      : t("checklist.item_not_done", { text: it.text });

  const commit = (): void => {
    const text = draft().trim();
    if (text.length === 0 || [...text].length > CHECKLIST_ITEM_MAX_CHARS) return;
    props.onAdd(text);
    setDraft("");
    setAdding(false);
  };

  return (
    <div class="lk-ak-card lk-checklist" role="group" aria-label={props.view.title}>
      <div class="lk-checklist-title">{props.view.title}</div>
      <div class="lk-poll-kind">
        {t("checklist.done_of", { done: props.view.doneCount, total: props.view.items.length })}
      </div>
      <ul class="lk-checklist-items">
        <For each={props.view.items}>
          {(it) => (
            <li>
              <Show
                when={it.canToggle && !props.view.closed}
                fallback={
                  <div class={`lk-checklist-item${it.done ? " done" : ""}`} aria-label={label(it)}>
                    <ItemBody item={it} />
                  </div>
                }
              >
                <button
                  type="button"
                  class={`lk-checklist-item${it.done ? " done" : ""}`}
                  role="checkbox"
                  aria-checked={it.done}
                  aria-label={label(it)}
                  onClick={() => props.onToggle(it.ref, !it.done)}
                >
                  <ItemBody item={it} />
                </button>
              </Show>
            </li>
          )}
        </For>
      </ul>
      <Show when={props.view.canAdd && !props.view.closed}>
        <Show
          when={adding()}
          fallback={
            <button type="button" class="lk-ak-button-link" onClick={() => setAdding(true)}>
              {t("checklist.add_item")}
            </button>
          }
        >
          <div class="lk-checklist-add">
            <input
              ref={(el) => queueMicrotask(() => el.focus())}
              type="text"
              maxLength={CHECKLIST_ITEM_MAX_CHARS}
              aria-label={t("checklist.add_item")}
              placeholder={t("checklist.add_item")}
              value={draft()}
              onInput={(e) => setDraft(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  commit();
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  e.stopPropagation();
                  setDraft("");
                  setAdding(false);
                }
              }}
              onBlur={() => {
                if (draft().trim().length === 0) setAdding(false);
              }}
            />
          </div>
        </Show>
      </Show>
      <Show when={props.view.closed}>
        <div class="lk-poll-footer">{t("poll.closed")}</div>
      </Show>
    </div>
  );
}

function ItemBody(props: { readonly item: ChecklistItemView }): JSX.Element {
  return (
    <>
      <span class={`lk-poll-mark box${props.item.done ? " on" : ""}`} aria-hidden="true">
        <Show when={props.item.done}>
          <Icon name="checkmark" size={12} />
        </Show>
      </span>
      <span class="lk-checklist-text" aria-hidden="true">{props.item.text}</span>
      <Show when={props.item.done && props.item.doneByName !== undefined}>
        <span class="lk-ak-small lk-ak-muted" aria-hidden="true">{props.item.doneByName}</span>
      </Show>
    </>
  );
}
