// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 8.1 — the checklist editor, for a new
 * checklist and for the creator's `Edit checklist`. An edited item keeps its
 * id, so its ticks stay with it (ADR-0092 section 5); a new item has none and
 * the conversation assigns one when it signs.
 */
import { createMemo, createSignal, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { requestConfirm } from "../ui/confirm-dialog";
import { AttachScreen, AttachSwitch, AttachSwitchGroup } from "./AttachScreen";
import { EditorRows } from "./EditorRows";
import {
  CHECKLIST_ITEM_MAX,
  CHECKLIST_ITEMS_MAX,
  CHECKLIST_ITEMS_MIN,
  CHECKLIST_TITLE_MAX,
  isChecklistDraftValid,
  movedTo,
  withAddedRow,
  withoutRow,
} from "./poll-draft";
import type { ChecklistDraft, ChecklistDraftItem } from "./types";
import "./editors.css";

interface Row {
  readonly id?: number;
  readonly text: string;
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
    initial === undefined || initial.items.length === 0
      ? [{ text: "" }]
      : initial.items.map((i) => ({ ...(i.id === undefined ? {} : { id: i.id }), text: i.text })),
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
    setRows(next);
  }

  const texts = createMemo(() => rows().map((r) => r.text));

  return (
    <AttachScreen
      compact
      title={initial === undefined ? t("checklist.editor.title") : t("checklist.editor.edit_title")}
      onClose={() => void close()}
      footer={
        <button type="button" class="primary" disabled={draft() === null || busy()} onClick={() => void submit()}>
          {initial === undefined ? t("checklist.editor.create") : t("checklist.editor.save")}
        </button>
      }
    >
      <input
        class="lk-attach-field"
        type="text"
        maxLength={CHECKLIST_TITLE_MAX}
        placeholder={t("checklist.editor.name")}
        aria-label={t("checklist.editor.name")}
        value={title()}
        onInput={(e) => setTitle(e.currentTarget.value)}
      />
      <EditorRows
        label={t("checklist.editor.items")}
        items={texts()}
        min={CHECKLIST_ITEMS_MIN}
        max={CHECKLIST_ITEMS_MAX}
        maxLength={CHECKLIST_ITEM_MAX}
        placeholder={(i) => t("checklist.editor.item", { n: i + 1 })}
        removeLabel={(i) => t("checklist.editor.remove_item", { n: i + 1 })}
        addLabel={t("checklist.editor.add_item")}
        onInput={setRow}
        onAdd={() => setRows(withAddedRow(rows(), { text: "" }, CHECKLIST_ITEMS_MAX))}
        onRemove={(i) => setRows(withoutRow(rows(), i, CHECKLIST_ITEMS_MIN))}
        onMove={(from, to) => setRows(movedTo(rows(), from, to))}
      />
      <Show when={props.showOthersAdd || props.showOthersMark}>
        <AttachSwitchGroup>
          <Show when={props.showOthersAdd}>
            <AttachSwitch title={t("checklist.others_add")} on={othersAdd()} onChange={setOthersAdd} />
          </Show>
          <Show when={props.showOthersMark}>
            <AttachSwitch title={t("checklist.others_mark")} on={othersMark()} onChange={setOthersMark} />
          </Show>
        </AttachSwitchGroup>
      </Show>
    </AttachScreen>
  );
}
