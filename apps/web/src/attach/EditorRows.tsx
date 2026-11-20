// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 7.1a — the rows of the poll and checklist
 * editors: a muted section label, one single-line field per entry with a
 * remove (x) inside its end edge (only above the minimum), a drag handle that
 * takes no room in the row (in the dialog's side padding, shown on hover and
 * focus; inside the field's start padding on a touch screen), and an
 * `Add option` / `Add item`
 * row underneath (absent at the maximum). `Enter` moves to the next row and on
 * the last row adds one; `Alt+Up` / `Alt+Down` moves a row and announces it.
 */
import { createSignal, Index, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { startDragReorder } from "./drag-reorder";
import { canAddRow, canRemoveRow, enterAction, keyboardMoveTarget } from "./poll-draft";

let nextId = 0;

export function EditorRows(props: {
  /** The section label (`Options` / `Items`). */
  readonly label: string;
  readonly items: readonly string[];
  readonly min: number;
  readonly max: number;
  readonly maxLength: number;
  readonly placeholder: (i: number) => string;
  readonly removeLabel: (i: number) => string;
  readonly addLabel: string;
  readonly onInput: (i: number, text: string) => void;
  readonly onAdd: () => void;
  readonly onRemove: (i: number) => void;
  /** A row moved from `from` to `to`, by drag or by keyboard. */
  readonly onMove: (from: number, to: number) => void;
  /** Anything drawn before the field (the quiz's correct-answer radio). */
  readonly leading?: (i: number) => JSX.Element;
  /** Whether `leading` currently draws something (the handle sits before it). */
  readonly leadingShown?: boolean;
}): JSX.Element {
  nextId += 1;
  const labelId = `lk-attach-rows-label-${nextId}`;
  const [announce, setAnnounce] = createSignal("");
  let rowsEl: HTMLDivElement | undefined;

  const focusRow = (i: number): void => {
    queueMicrotask(() => {
      const inputs = rowsEl?.querySelectorAll<HTMLInputElement>("input[data-row-field]");
      inputs?.[i]?.focus();
    });
  };

  const onKeyDown = (e: KeyboardEvent, i: number): void => {
    if (e.isComposing) return;
    const n = props.items.length;
    if (e.key === "Enter" && !e.altKey && !e.shiftKey && !e.metaKey && !e.ctrlKey) {
      e.preventDefault();
      const a = enterAction(i, n, props.max);
      if (a === null) return;
      if (a.kind === "add") props.onAdd();
      focusRow(a.to);
      return;
    }
    const to = keyboardMoveTarget(i, e.key, e.altKey, n);
    if (to === null) return;
    e.preventDefault();
    props.onMove(i, to);
    // A fresh string each time so a repeated position is announced again.
    setAnnounce("");
    queueMicrotask(() => setAnnounce(t("editor.moved", { n: to + 1 })));
    focusRow(to);
  };

  return (
    <div class="lk-attach-entries">
      <p class="lk-attach-section" id={labelId}>
        {props.label}
      </p>
      <div ref={rowsEl} class="lk-attach-rows" role="group" aria-labelledby={labelId}>
        <Index each={props.items}>
          {(value, i) => (
            <div class="lk-attach-row" classList={{ "has-leading": props.leadingShown === true }} data-reorder-row="">
              {props.leading?.(i)}
              <div class="lk-attach-entry">
                <span
                  class="lk-attach-grip"
                  aria-hidden="true"
                  classList={{ inert: props.items.length < 2 }}
                  onPointerDown={(e) => startDragReorder(e, rowsEl, i, props.items.length - 1, props.onMove)}
                >
                  <Icon name="drag-handle" size={14} />
                </span>
                <input
                  type="text"
                  data-row-field=""
                  maxLength={props.maxLength}
                  placeholder={props.placeholder(i)}
                  aria-label={props.placeholder(i)}
                  aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown"
                  value={value()}
                  onInput={(e) => props.onInput(i, e.currentTarget.value)}
                  onKeyDown={(e) => onKeyDown(e, i)}
                />
                <Show when={canRemoveRow(props.items.length, props.min)}>
                  <button
                    type="button"
                    class="lk-attach-entry-remove"
                    aria-label={props.removeLabel(i)}
                    title={props.removeLabel(i)}
                    onClick={() => {
                      props.onRemove(i);
                      focusRow(Math.min(i, props.items.length - 1));
                    }}
                  >
                    <Icon name="xmark" size={14} />
                  </button>
                </Show>
              </div>
            </div>
          )}
        </Index>
      </div>
      <Show when={canAddRow(props.items.length, props.max)}>
        <button
          type="button"
          class="lk-attach-add"
          onClick={() => {
            const at = props.items.length;
            props.onAdd();
            focusRow(at);
          }}
        >
          <Icon name="plus" size={18} />
          <span>{props.addLabel}</span>
        </button>
      </Show>
      <p class="lk-visually-hidden" aria-live="polite">
        {announce()}
      </p>
    </div>
  );
}
