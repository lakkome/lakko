// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The folder editor and its chat picker (`docs/ux/chat-folders.md` 3.1-3.4).
 *
 * One modal, opened from Edit folder, from Settings > Chat folders and from
 * "New folder..." in a row's Add-to-folder list. Mounted once for the whole
 * session (`FolderEditorHost`, beside the confirmation dialog) and driven by
 * `openFolderEditor`.
 *
 * What a Save writes is decided by `diffFolderDraft`: the meta register when
 * the name or a toggle changed, and one member register per chat whose hand
 * state changed. A chat this device cannot resolve is kept in the draft
 * untouched, so it is never written away (ADR-0058 section 5a).
 */
import { createMemo, createSignal, For, Show, type JSX } from "solid-js";

import { useDeviceGroupSync } from "../../devices/sync/DeviceGroupSyncContext";
import { t } from "../../l10n";
import { createLiveText } from "../../l10n/live-text";
import { requestConfirm } from "../../ui/confirm-dialog";
import { Avatar } from "../../ui/Avatar";
import { Icon } from "../../ui/icons";
import { createConversationRows } from "../conversation-rows";
import type { ConversationRow } from "../conversation-list";
import { rowMatchesQuery } from "../list-preview";
import { uiRules } from "../ui-rules";
import {
  canCreateFolder,
  closeFolderEditor,
  currentEditorRequest,
  deleteFolder,
  newFolderIdHex,
  type EditorRequest,
} from "./folder-actions";
import {
  chatCountLabel,
  clipFolderName,
  appendToOrderEdit,
  diffFolderDraft,
  draftCanSave,
  draftIsDirty,
  draftOf,
  EXCLUDE_ARCHIVED,
  EXCLUDE_MUTED,
  EXCLUDE_READ,
  INCLUDE_CHANNELS,
  INCLUDE_GROUPS,
  INCLUDE_ONE_TO_ONE,
  MAX_EXCLUDED_BY_HAND,
  MAX_INCLUDED_BY_HAND,
  newFolderDraft,
  refOfRow,
  ruleIdOfRef,
  ruleRowId,
  sameConversation,
  type ConversationRef,
  type FolderDraft,
  type FolderModel,
} from "./folder-model";
import { rowsInFolder } from "./folder-view";

/** The one editor host; renders nothing while no editor is open. */
export function FolderEditorHost(): JSX.Element {
  return (
    <Show when={currentEditorRequest()} keyed>
      {(req) => <FolderEditor request={req} />}
    </Show>
  );
}

const KIND_TOGGLES: ReadonlyArray<{ bit: number; labelKey: string }> = [
  { bit: INCLUDE_ONE_TO_ONE, labelKey: "folders.editor.kind.one_to_one" },
  { bit: INCLUDE_GROUPS, labelKey: "folders.editor.kind.groups" },
  { bit: INCLUDE_CHANNELS, labelKey: "folders.editor.kind.channels" },
];
const EXCLUDE_TOGGLES: ReadonlyArray<{ bit: number; labelKey: string }> = [
  { bit: EXCLUDE_MUTED, labelKey: "folders.editor.exclude.muted" },
  { bit: EXCLUDE_READ, labelKey: "folders.editor.exclude.read" },
  { bit: EXCLUDE_ARCHIVED, labelKey: "folders.editor.exclude.archived" },
];

function FolderEditor(props: { request: EditorRequest }): JSX.Element {
  const sync = useDeviceGroupSync();
  const rows = createConversationRows();

  // The folder as it was when the editor opened. A folder deleted on another
  // device while open is simply gone on Save (the fold refuses the edit).
  const original: FolderModel | null =
    props.request.mode === "edit"
      ? (sync.folders().find((f) => f.idHex === (props.request as { folderIdHex: string }).folderIdHex) ??
        null)
      : null;
  const initial: FolderDraft =
    original !== null
      ? draftOf(original)
      : newFolderDraft(props.request.mode === "new" ? props.request.preAdd : undefined);
  const baseline: FolderDraft | null = original === null ? null : draftOf(original);

  const [draft, setDraft] = createSignal<FolderDraft>(initial);
  const [picker, setPicker] = createSignal<"include" | "exclude" | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();

  const isValidName = (n: string): boolean => uiRules()?.isValidFolderName(n) ?? false;
  const canSave = (): boolean => !busy() && draftCanSave(draft(), isValidName);
  const dirty = (): boolean => draftIsDirty(initial, draft());

  /** Row lookup by rule id, for the hand lists and the picker. */
  const rowByRuleId = createMemo(
    (): ReadonlyMap<string, ConversationRow> =>
      new Map(rows.orderedRows().map((r) => [ruleRowId(r.kind, r.id), r])),
  );

  /** "12 chats" - the live match count, once the folder has a rule. */
  const matchCount = createMemo((): number | null => {
    const d = draft();
    const rules = uiRules();
    if (rules === null) return null;
    if (d.includeKinds === 0 && d.included.length === 0) return null;
    const asFolder: FolderModel = {
      idHex: "",
      name: d.name,
      includeKinds: d.includeKinds,
      excludeFlags: d.excludeFlags,
      included: d.included,
      excluded: d.excluded,
      pinned: [],
    };
    return rowsInFolder(rows.orderedRows(), asFolder, rules).length;
  });

  const toggleBit = (field: "includeKinds" | "excludeFlags", bit: number): void => {
    setDraft((d) => ({ ...d, [field]: d[field] ^ bit }));
  };

  const removeRef = (field: "included" | "excluded", ref: ConversationRef): void => {
    setDraft((d) => ({ ...d, [field]: d[field].filter((r) => !sameConversation(r, ref)) }));
  };

  async function close(): Promise<void> {
    if (dirty()) {
      const discard = await requestConfirm({
        title: t("folders.editor.discard.title"),
        body: null,
        confirmLabel: t("folders.editor.discard.confirm"),
        cancelLabel: t("folders.editor.discard.keep"),
        tone: "danger",
      });
      if (!discard) return;
    }
    closeFolderEditor();
  }

  async function save(): Promise<void> {
    const rules = uiRules();
    if (!canSave() || rules === null) return;
    const creating = original === null;
    if (creating && !canCreateFolder(sync.folders())) return;
    const folderIdHex = creating
      ? newFolderIdHex(() => rules.mintFolderId())
      : (original as FolderModel).idHex;
    const edits = diffFolderDraft(folderIdHex, baseline, draft());
    // A new folder also takes its place at the end of the bar order.
    const all = creating ? [...edits, appendToOrderEdit(sync.folders(), folderIdHex)] : edits;
    setBusy(true);
    setError(null);
    try {
      await sync.applyFolderEdits(all);
      closeFolderEditor();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("folder save failed", err);
      setError(() => t("folders.editor.error.save"));
    } finally {
      setBusy(false);
    }
  }

  async function remove(): Promise<void> {
    if (original === null) return;
    const deleted = await deleteFolder(sync, original);
    if (deleted) closeFolderEditor();
  }

  const HandList = (p: { field: "included" | "excluded" }): JSX.Element => (
    <For each={draft()[p.field]}>
      {(ref) => {
        const id = ruleIdOfRef(ref);
        const row = (): ConversationRow | undefined =>
          id === null ? undefined : rowByRuleId().get(id);
        // A chat this device does not hold yet is kept, not shown.
        return (
          <Show when={row()}>
            {(r) => (
              <div class="lk-folder-hand-row">
                <Avatar label={r().title} id={r().id} personKey={r().personKey} />
                <span class="lk-ellipsis">{r().title}</span>
                <button
                  type="button"
                  class="lk-icon-button"
                  aria-label={t("folders.editor.remove_chat_a11y", { name: r().title })}
                  title={t("common.remove")}
                  onClick={() => removeRef(p.field, ref)}
                >
                  <Icon name="close" size={16} />
                </button>
              </div>
            )}
          </Show>
        );
      }}
    </For>
  );

  return (
    <div
      class="lk-modal-backdrop"
      onClick={(e) => {
        if (e.target === e.currentTarget) void close();
      }}
      onKeyDown={(e) => {
        if (e.key === "Escape" && picker() === null) {
          e.stopPropagation();
          void close();
        }
      }}
    >
      <div
        class="lk-modal lk-folder-editor"
        role="dialog"
        aria-modal="true"
        aria-labelledby="lk-folder-editor-title"
      >
        <h2 id="lk-folder-editor-title">{original === null ? t("folders.new") : t("folders.edit")}</h2>
        <label class="lk-folder-name">
          <input
            type="text"
            placeholder={t("folders.editor.name_placeholder")}
            aria-label={t("folders.editor.name_placeholder")}
            value={draft().name}
            // eslint-disable-next-line jsx-a11y/no-autofocus
            autofocus
            onInput={(e) => {
              const clipped = clipFolderName(e.currentTarget.value);
              if (clipped !== e.currentTarget.value) e.currentTarget.value = clipped;
              setDraft((d) => ({ ...d, name: clipped }));
            }}
          />
        </label>

        <section class="lk-folder-section" aria-labelledby="lk-folder-included">
          <h3 id="lk-folder-included">{t("folders.editor.included")}</h3>
          <Show when={matchCount() !== null}>
            <p class="lk-folder-count">{chatCountLabel(matchCount() ?? 0)}</p>
          </Show>
          <For each={KIND_TOGGLES}>
            {(toggle) => (
              <label class="lk-check lk-folder-toggle">
                <input
                  type="checkbox"
                  checked={(draft().includeKinds & toggle.bit) !== 0}
                  onChange={() => toggleBit("includeKinds", toggle.bit)}
                />
                {t(toggle.labelKey)}
              </label>
            )}
          </For>
          <HandList field="included" />
          <button type="button" class="ghost small lk-folder-add" onClick={() => setPicker("include")}>
            <Icon name="plus" size={16} />
            {t("folders.editor.add_chats")}
          </button>
        </section>

        <section class="lk-folder-section" aria-labelledby="lk-folder-excluded">
          <h3 id="lk-folder-excluded">{t("folders.editor.excluded")}</h3>
          <For each={EXCLUDE_TOGGLES}>
            {(toggle) => (
              <label class="lk-check lk-folder-toggle">
                <input
                  type="checkbox"
                  checked={(draft().excludeFlags & toggle.bit) !== 0}
                  onChange={() => toggleBit("excludeFlags", toggle.bit)}
                />
                {t(toggle.labelKey)}
              </label>
            )}
          </For>
          <HandList field="excluded" />
          <button type="button" class="ghost small lk-folder-add" onClick={() => setPicker("exclude")}>
            <Icon name="plus" size={16} />
            {t("folders.editor.exclude_chats")}
          </button>
        </section>

        <Show when={error()}>
          {(msg) => (
            <p class="lk-folder-error" role="alert">
              {msg()}
            </p>
          )}
        </Show>

        <div class="lk-folder-actions">
          <button type="button" class="ghost" onClick={() => void close()}>
            {t("common.cancel")}
          </button>
          <button type="button" class="primary" disabled={!canSave()} onClick={() => void save()}>
            {t("common.save")}
          </button>
        </div>

        <Show when={original !== null}>
          <div class="lk-folder-delete">
            <button type="button" class="ghost lk-folder-danger" onClick={() => void remove()}>
              {t("folders.delete")}
            </button>
          </div>
        </Show>
      </div>

      <Show when={picker()} keyed>
        {(which) => (
          <ChatPicker
            which={which}
            rows={rows.orderedRows()}
            selected={which === "include" ? draft().included : draft().excluded}
            onDone={(refs) => {
              setDraft((d) => {
                const other = which === "include" ? d.excluded : d.included;
                // A chat is in at most one hand list: picking it here takes
                // it out of the other.
                const otherKept = other.filter((o) => !refs.some((r) => sameConversation(r, o)));
                return which === "include"
                  ? { ...d, included: refs, excluded: otherKept }
                  : { ...d, excluded: refs, included: otherKept };
              });
              setPicker(null);
            }}
            onCancel={() => setPicker(null)}
          />
        )}
      </Show>
    </div>
  );
}

/**
 * The shared chat picker (UX 3.3): the search field, then the merged list
 * with a checkbox per row. The rows already in the set are checked. The same
 * picker serves Add chats and Exclude chats; its title says which.
 *
 * Refs in the set this device cannot resolve are carried through untouched.
 */
function ChatPicker(props: {
  which: "include" | "exclude";
  rows: readonly ConversationRow[];
  selected: readonly ConversationRef[];
  onDone: (refs: ConversationRef[]) => void;
  onCancel: () => void;
}): JSX.Element {
  const [query, setQuery] = createSignal("");
  const [picked, setPicked] = createSignal<ConversationRef[]>([...props.selected]);
  const limit = props.which === "include" ? MAX_INCLUDED_BY_HAND : MAX_EXCLUDED_BY_HAND;
  const atLimit = (): boolean => picked().length >= limit;

  const refFor = (r: ConversationRow): ConversationRef | null => refOfRow(r.kind, r.id);
  const isPicked = (ref: ConversationRef): boolean => picked().some((p) => sameConversation(p, ref));

  const visible = (): readonly ConversationRow[] => {
    const q = query();
    return q.trim().length === 0
      ? props.rows
      : props.rows.filter((r) => rowMatchesQuery(q, r.title, r.preview));
  };

  const toggle = (ref: ConversationRef): void => {
    if (isPicked(ref)) setPicked((p) => p.filter((x) => !sameConversation(x, ref)));
    else if (!atLimit()) setPicked((p) => [...p, ref]);
  };

  return (
    <div
      class="lk-modal-backdrop lk-picker-backdrop"
      onClick={(e) => {
        if (e.target === e.currentTarget) props.onCancel();
      }}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          props.onCancel();
        }
      }}
    >
      <div class="lk-modal lk-folder-picker" role="dialog" aria-modal="true" aria-labelledby="lk-picker-title">
        <h2 id="lk-picker-title">{props.which === "include" ? t("folders.editor.add_chats") : t("folders.editor.exclude_chats")}</h2>
        <Show when={atLimit()}>
          <p class="lk-folder-count" role="status">
            {props.which === "include"
              ? t("folders.picker.limit_include", { count: MAX_INCLUDED_BY_HAND })
              : t("folders.picker.limit_exclude", { count: MAX_EXCLUDED_BY_HAND })}
          </p>
        </Show>
        <label class="lk-search">
          <Icon name="search" size={16} />
          <input
            type="search"
            placeholder={t("chats.search.placeholder")}
            aria-label={t("chats.search.label")}
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
          />
        </label>
        <div class="lk-folder-picker-list">
          <For each={visible()}>
            {(r) => {
              const ref = refFor(r);
              if (ref === null) return null;
              return (
                <label class="lk-row pick">
                  <input
                    type="checkbox"
                    checked={isPicked(ref)}
                    disabled={!isPicked(ref) && atLimit()}
                    onChange={() => toggle(ref)}
                  />
                  <Avatar label={r.title} id={r.id} personKey={r.personKey} />
                  <span class="lk-row-title">
                    <span class="lk-ellipsis">{r.title}</span>
                  </span>
                </label>
              );
            }}
          </For>
        </div>
        <div class="lk-folder-actions">
          <button type="button" class="primary" onClick={() => props.onDone(picked())}>
            {t("common.done")}
          </button>
        </div>
      </div>
    </div>
  );
}
