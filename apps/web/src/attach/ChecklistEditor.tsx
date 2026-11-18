// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 8.1 — the checklist editor, for a new
 * checklist and for the creator's `Edit checklist`. An edited item keeps its
 * id, so its ticks stay with it (ADR-0092 section 5); a new item has none and
 * the conversation assigns one when it signs.
 */
import { createMemo, createSignal, Index, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { requestConfirm } from "../ui/confirm-dialog";
import { Icon } from "../ui/icons";
import { AttachScreen, AttachSwitch } from "./AttachScreen";
import {
  CHECKLIST_ITEM_MAX,
  CHECKLIST_ITEMS_MAX,
  CHECKLIST_TITLE_MAX,
  isChecklistDraftValid,
  moved,
  movedTo,
} from "./poll-draft";
import type { ChecklistDraft, ChecklistDraftItem } from "./types";
import "./editors.css";
import { startDragReorder } from "./drag-reorder";

interface Row {
  readonly id?: number;
  readonly text: string;
}

/** One empty row trails the list while there is room. */
function withTrailing(rows: readonly Row[]): Row[] {
  const out = [...rows];
  const last = out[out.length - 1];
  if (out.length < CHECKLIST_ITEMS_MAX && (last === undefined || last.text.trim().length > 0)) out.push({ text: "" });
  return out;
}

export function ChecklistEditor(props: {
  readonly showOthersAdd: boolean;
  readonly showOthersMark: boolean;
  readonly initial?: ChecklistDraft;
  readonly onCancel: () => void;
  readonly onSubmit: (d: ChecklistDraft) => Promise<void> | void;
}): JSX.Element {
  const initial = props.initial;
  const [title, setTitle] = createSignal(initial?.title ?? "");
  const [rows, setRows] = createSignal<Row[]>(
    withTrailing(initial?.items.map((i) => ({ ...(i.id === undefined ? {} : { id: i.id }), text: i.text })) ?? []),
  );
  const [othersAdd, setOthersAdd] = createSignal(initial?.othersAdd ?? false);
  const [othersMark, setOthersMark] = createSignal(initial?.othersMark ?? true);
  const [busy, setBusy] = createSignal(false);

  const draft = createMemo((): ChecklistDraft | null => {
    const items: ChecklistDraftItem[] = rows()
      .filter((r) => r.text.trim().length > 0)
      .map((r) => ({ ...(r.id === undefined ? {} : { id: r.id }), text: r.text.trim() }));
    const d: ChecklistDraft = {
      title: title().trim(),
      othersAdd: props.showOthersAdd ? othersAdd() : false,
      othersMark: props.showOthersMark ? othersMark() : false,
      items,
    };
    return isChecklistDraftValid(d) ? d : null;
  });

  const dirty = (): boolean => {
    if (initial === undefined) return title().trim().length > 0 || rows().some((r) => r.text.trim().length > 0);
    const d = draft();
    return d === null || JSON.stringify(d) !== JSON.stringify(initial);
  };

  async function close(): Promise<void> {
    if (busy()) return;
    if (!dirty()) {
      props.onCancel();
      return;
    }
    const discard = await requestConfirm({
      title: t("checklist.editor.discard"),
      body: null,
      confirmLabel: t("editor.discard"),
      cancelLabel: t("editor.keep_editing"),
      tone: "danger",
    });
    if (discard) props.onCancel();
  }

  async function submit(): Promise<void> {
    const d = draft();
    if (d === null || busy()) return;
    setBusy(true);
    try {
      await props.onSubmit(d);
    } finally {
      setBusy(false);
    }
  }

  function setRow(i: number, text: string): void {
    const next = [...rows()];
    const old = next[i];
    next[i] = { ...(old?.id === undefined ? {} : { id: old.id }), text };
    setRows(withTrailing(next));
  }

  function leaveRow(i: number): void {
    const r = rows();
    const row = r[i];
    if (row === undefined || row.text.trim().length > 0 || i === r.length - 1 || r.length <= 1) return;
    setRows(r.filter((_, j) => j !== i));
  }

  let rowsEl: HTMLDivElement | undefined;
  const lastMovable = (): number => {
    const r = rows();
    return (r[r.length - 1]?.text ?? "").trim().length === 0 ? r.length - 2 : r.length - 1;
  };

  return (
    <AttachScreen
      title={initial === undefined ? t("checklist.editor.title") : t("checklist.editor.edit_title")}
      onClose={() => void close()}
      footer={
        <button type="button" class="primary" disabled={draft() === null || busy()} onClick={() => void submit()}>
          {initial === undefined ? t("checklist.editor.create") : t("checklist.editor.save")}
        </button>
      }
    >
      <label class="lk-attach-label" for="lk-checklist-title">
        {t("checklist.editor.name")}
      </label>
      <input
        id="lk-checklist-title"
        class="lk-attach-field"
        type="text"
        maxLength={CHECKLIST_TITLE_MAX}
        value={title()}
        onInput={(e) => setTitle(e.currentTarget.value)}
      />
      <div ref={rowsEl} class="lk-attach-rows">
        <Index each={rows()}>
          {(row, i) => (
            <div class="lk-attach-row" data-reorder-row="">
              <span
                class="lk-attach-grip"
                aria-hidden="true"
                classList={{ inert: i > lastMovable() }}
                onPointerDown={(e) =>
                  startDragReorder(e, rowsEl, i, lastMovable(), (from, to) => setRows(movedTo(rows(), from, to)))
                }
              >
                <Icon name="drag-handle" size={16} />
              </span>
              <input
                type="text"
                maxLength={CHECKLIST_ITEM_MAX}
                placeholder={t("checklist.editor.item", { n: i + 1 })}
                aria-label={t("checklist.editor.item", { n: i + 1 })}
                value={row().text}
                onInput={(e) => setRow(i, e.currentTarget.value)}
                onBlur={() => leaveRow(i)}
              />
              <button
                type="button"
                class="lk-icon-button"
                aria-label={t("poll.editor.move_up")}
                title={t("poll.editor.move_up")}
                disabled={i === 0 || i > lastMovable()}
                onClick={() => setRows(moved(rows(), i, i - 1))}
              >
                <Icon name="chevron-up" size={16} />
              </button>
              <button
                type="button"
                class="lk-icon-button"
                aria-label={t("poll.editor.move_down")}
                title={t("poll.editor.move_down")}
                disabled={i >= lastMovable()}
                onClick={() => setRows(moved(rows(), i, i + 1))}
              >
                <Icon name="chevron-down" size={16} />
              </button>
              <button
                type="button"
                class="lk-icon-button"
                aria-label={t("checklist.editor.remove_item", { n: i + 1 })}
                title={t("checklist.editor.remove_item", { n: i + 1 })}
                disabled={i > lastMovable() || rows().length <= 1}
                onClick={() => setRows(withTrailing(rows().filter((_, j) => j !== i)))}
              >
                <Icon name="minus-circle" size={16} />
              </button>
            </div>
          )}
        </Index>
      </div>
      <Show when={props.showOthersAdd}>
        <AttachSwitch title={t("checklist.others_add")} on={othersAdd()} onChange={setOthersAdd} />
      </Show>
      <Show when={props.showOthersMark}>
        <AttachSwitch title={t("checklist.others_mark")} on={othersMark()} onChange={setOthersMark} />
      </Show>
    </AttachScreen>
  );
}
