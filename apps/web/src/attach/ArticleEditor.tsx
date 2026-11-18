// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 11.1 — the article editor.
 *
 * The body is a list of blocks, each edited on its own: a focused text block
 * is a plain text area, an unfocused one shows its formatting. Bold, italic
 * and links apply to the selection in the focused block and are kept as runs
 * (`article-model.ts`), so what is published is the canonical tree the core
 * requires. Pasting keeps plain text only. No raw HTML or Markdown is read.
 */
import { blocksFromPastedHtml, type PasteNode } from "./article-paste";
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Match,
  onCleanup,
  onMount,
  Show,
  Switch,
  type JSX,
} from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { requestConfirm } from "../ui/confirm-dialog";
import {
  applyTextEdit,
  blocksTextBytes,
  buildArticle,
  editorBlocksOf,
  hasContent,
  imageCount,
  insertLink,
  nextHeadingKind,
  plainText,
  rangeHasMark,
  toggleMark,
  withEditedBlock,
  type ArticleRun,
  type EditorBlock,
  type Mark,
} from "./article-model";
import type { Article, ArticleDraftOut } from "./types";
import "./bubbles.css";

/** UX 11.1 — a title is 1..=255 characters. */
const TITLE_MAX_CHARS = 255;

interface Selection {
  readonly index: number;
  readonly start: number;
  readonly end: number;
}

export function ArticleEditor(props: {
  readonly initial?: { readonly article: Article; readonly images: readonly Blob[] };
  /** Which conversation's draft this is; the parent keeps drafts by it. */
  readonly draftKey: string;
  readonly maxImages: number;
  readonly maxTextBytes: number;
  readonly urlValid: (url: string) => boolean;
  /** Set when editing an existing article. */
  readonly editing?: boolean;
  readonly onCancel: () => void;
  readonly onPublish: (out: ArticleDraftOut) => Promise<void> | void;
  readonly onSaveDraft: (out: ArticleDraftOut) => void;
}): JSX.Element {
  const initialBlocks = props.initial === undefined ? [] : editorBlocksOf(props.initial.article);
  const [title, setTitle] = createSignal(props.initial?.article.title ?? "");
  const [blocks, setBlocks] = createSignal<EditorBlock[]>(
    initialBlocks.length > 0 ? initialBlocks : [{ kind: "paragraph", runs: [] }],
  );
  const [images, setImages] = createSignal<Blob[]>([...(props.initial?.images ?? [])]);
  const [focused, setFocused] = createSignal<number | null>(null);
  const [selection, setSelection] = createSignal<Selection | null>(null);
  const [pendingCaret, setPendingCaret] = createSignal<{ index: number; caret: number } | null>(null);
  const [atLimit, setAtLimit] = createSignal(false);
  const [linkDialog, setLinkDialog] = createSignal<{ sel: Selection; url: string; text: string } | null>(null);
  const [busy, setBusy] = createSignal(false);
  let fileInput: HTMLInputElement | undefined;

  // Object URLs for the images, revoked when the list changes or the editor closes.
  const imageUrls = createMemo<string[]>((prev) => {
    for (const u of prev ?? []) URL.revokeObjectURL(u);
    return images().map((b) => URL.createObjectURL(b));
  });
  onCleanup(() => {
    for (const u of imageUrls()) URL.revokeObjectURL(u);
  });

  const out = (): ArticleDraftOut => buildArticle(title(), blocks(), images());
  const canPublish = (): boolean => title().trim().length > 0 && out().article.blocks.length > 0 && !busy();

  const focusedBlock = (): EditorBlock | undefined => {
    const i = focused();
    return i === null ? undefined : blocks()[i];
  };
  const focusedIsText = (): boolean => {
    const b = focusedBlock();
    return b !== undefined && b.kind !== "image" && b.kind !== "divider";
  };

  const setBlocksChecked = (next: EditorBlock[]): boolean => {
    if (blocksTextBytes(next) > props.maxTextBytes) {
      setAtLimit(true);
      return false;
    }
    setAtLimit(false);
    setBlocks(next);
    return true;
  };

  const runsOf = (b: EditorBlock): readonly ArticleRun[] =>
    b.kind === "divider" ? [] : b.kind === "image" ? b.caption : b.runs;

  const replaceRuns = (index: number, runs: ArticleRun[]): void => {
    const list = blocks();
    const b = list[index];
    if (b === undefined || b.kind === "divider") return;
    const next = list.map((x, i) =>
      i !== index ? x : b.kind === "image" ? { ...b, caption: runs } : { ...b, runs },
    );
    setBlocksChecked(next);
  };

  const onTextInput = (index: number, el: HTMLTextAreaElement): void => {
    const b = blocks()[index];
    if (b === undefined || b.kind === "divider") return;
    const runs = applyTextEdit(runsOf(b), el.value);
    const next = withEditedBlock(blocks(), index, runs);
    if (!setBlocksChecked(next)) {
      el.value = plainText(runsOf(b));
      return;
    }
    // A line break moved the rest of the text into new blocks: continue at
    // the start of the last of them.
    if (el.value.includes("\n") && b.kind !== "image") {
      const lines = el.value.split(/\r\n|\r|\n/);
      setPendingCaret({ index: index + lines.length - 1, caret: 0 });
      setFocused(index + lines.length - 1);
    }
  };

  const rememberSelection = (index: number, el: HTMLTextAreaElement): void => {
    setSelection({ index, start: el.selectionStart, end: el.selectionEnd });
  };

  const applyMark = (mark: Mark): void => {
    const sel = selection();
    if (sel === null || sel.index !== focused()) return;
    const b = blocks()[sel.index];
    if (b === undefined || b.kind === "divider") return;
    replaceRuns(sel.index, toggleMark(runsOf(b), sel.start, sel.end, mark));
    setPendingCaret({ index: sel.index, caret: sel.end });
  };

  const markActive = (mark: Mark): boolean => {
    const sel = selection();
    if (sel === null || sel.index !== focused()) return false;
    const b = blocks()[sel.index];
    if (b === undefined || b.kind === "divider") return false;
    return rangeHasMark(runsOf(b), sel.start, sel.end, mark);
  };

  const setKind = (kind: "paragraph" | "h1" | "h2" | "quote" | { list: boolean }): void => {
    const i = focused();
    if (i === null) return;
    const b = blocks()[i];
    if (b === undefined || b.kind === "image" || b.kind === "divider") return;
    let nb: EditorBlock;
    if (typeof kind === "object") {
      nb =
        b.kind === "list-item" && b.ordered === kind.list
          ? { kind: "paragraph", runs: b.runs }
          : { kind: "list-item", ordered: kind.list, depth: 0, runs: b.runs };
    } else if (kind === "quote") {
      nb = b.kind === "quote" ? { kind: "paragraph", runs: b.runs } : { kind: "quote", runs: b.runs };
    } else {
      nb = { kind, runs: b.runs };
    }
    setBlocks(blocks().map((x, j) => (j === i ? nb : x)));
    setPendingCaret({ index: i, caret: selection()?.end ?? 0 });
  };

  const cycleHeading = (): void => {
    const b = focusedBlock();
    if (b === undefined) return;
    setKind(nextHeadingKind(b.kind));
  };

  const insertAfterFocus = (inserted: EditorBlock[]): void => {
    const list = blocks();
    const at = focused() === null ? list.length : (focused() as number) + 1;
    const next = [...list.slice(0, at), ...inserted, ...list.slice(at)];
    // Always leave a text block to type into after a non-text block.
    if (at + inserted.length >= next.length) next.push({ kind: "paragraph", runs: [] });
    setBlocks(next);
    setFocused(at + inserted.length);
    setPendingCaret({ index: at + inserted.length, caret: 0 });
  };

  const addDivider = (): void => insertAfterFocus([{ kind: "divider" }]);

  /** UX 11.1 — rich paste keeps headings, bold, italic, lists, quotes and
   *  links; everything else is plain text. The HTML is parsed into an inert
   *  document (`DOMParser`), never inserted. Plain-text paste is the
   *  textarea's own. */
  const onPaste = (index: number, e: ClipboardEvent): void => {
    const html = e.clipboardData?.getData("text/html") ?? "";
    if (html.length === 0 || typeof DOMParser === "undefined") return;
    const doc = new DOMParser().parseFromString(html, "text/html");
    const pasted = blocksFromPastedHtml(doc.body as unknown as PasteNode, (u) => props.urlValid(u));
    if (pasted.length === 0) return;
    e.preventDefault();
    const list = blocks();
    const here = list[index];
    const empty = here !== undefined && here.kind !== "image" && here.kind !== "divider" && plainText(here.runs).length === 0;
    const next = empty
      ? [...list.slice(0, index), ...pasted, ...list.slice(index + 1)]
      : [...list.slice(0, index + 1), ...pasted, ...list.slice(index + 1)];
    if (!setBlocksChecked(next)) return;
    const last = (empty ? index : index + 1) + pasted.length - 1;
    setFocused(last);
    setPendingCaret({ index: last, caret: plainText(runsOf(next[last]!)).length });
  };

  const pickImage = (): void => {
    if (imageCount(blocks()) >= props.maxImages) {
      setAtLimit(true);
      return;
    }
    fileInput?.click();
  };

  const onImagePicked = (file: File | undefined): void => {
    if (file === undefined || !file.type.startsWith("image/")) return;
    if (imageCount(blocks()) >= props.maxImages) {
      setAtLimit(true);
      return;
    }
    const index = images().length;
    setImages([...images(), file]);
    insertAfterFocus([{ kind: "image", image: index, caption: [] }]);
  };

  const openLink = (): void => {
    const sel = selection();
    const i = focused();
    if (i === null || !focusedIsText()) return;
    const b = blocks()[i];
    if (b === undefined) return;
    const s = sel !== null && sel.index === i ? sel : { index: i, start: 0, end: 0 };
    const text = plainText(runsOf(b)).slice(s.start, s.end);
    setLinkDialog({ sel: s, url: "https://", text });
  };

  const commitLink = (): void => {
    const d = linkDialog();
    if (d === null) return;
    const url = d.url.trim();
    if (!props.urlValid(url) || d.text.trim().length === 0) return;
    const b = blocks()[d.sel.index];
    if (b === undefined || b.kind === "divider") return;
    replaceRuns(d.sel.index, insertLink(runsOf(b), d.sel.start, d.sel.end, url, d.text));
    setLinkDialog(null);
    setFocused(d.sel.index);
    setPendingCaret({ index: d.sel.index, caret: d.sel.start + d.text.length });
  };

  const removeBlock = (index: number): void => {
    const next = blocks().filter((_, i) => i !== index);
    setBlocks(next.length > 0 ? next : [{ kind: "paragraph", runs: [] }]);
    setFocused(null);
  };

  const onKey = (index: number, e: KeyboardEvent & { currentTarget: HTMLTextAreaElement }): void => {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && (e.key === "b" || e.key === "B")) {
      e.preventDefault();
      rememberSelection(index, e.currentTarget);
      applyMark("bold");
      return;
    }
    if (mod && (e.key === "i" || e.key === "I")) {
      e.preventDefault();
      rememberSelection(index, e.currentTarget);
      applyMark("italic");
      return;
    }
    if (mod && (e.key === "k" || e.key === "K")) {
      e.preventDefault();
      rememberSelection(index, e.currentTarget);
      openLink();
      return;
    }
    const b = blocks()[index];
    if (b === undefined) return;
    if (e.key === "Tab" && b.kind === "list-item") {
      e.preventDefault();
      const depth = e.shiftKey ? 0 : 1;
      setBlocks(blocks().map((x, i) => (i === index ? { ...b, depth } : x)));
      setPendingCaret({ index, caret: e.currentTarget.selectionStart });
      return;
    }
    if (
      e.key === "Backspace" &&
      e.currentTarget.selectionStart === 0 &&
      e.currentTarget.selectionEnd === 0 &&
      index > 0
    ) {
      const prev = blocks()[index - 1];
      if (prev === undefined) return;
      e.preventDefault();
      if (b.kind === "list-item" || b.kind === "quote" || b.kind === "h1" || b.kind === "h2") {
        // First Backspace turns the block into plain text.
        setBlocks(blocks().map((x, i) => (i === index ? { kind: "paragraph", runs: runsOf(b) as ArticleRun[] } : x)));
        setPendingCaret({ index, caret: 0 });
        return;
      }
      if (prev.kind === "divider" || prev.kind === "image") {
        removeBlock(index - 1);
        setFocused(index - 1);
        setPendingCaret({ index: index - 1, caret: 0 });
        return;
      }
      // Merge into the previous text block.
      const caret = plainText(prev.runs).length;
      const merged = [...prev.runs, ...runsOf(b)];
      const next = blocks()
        .map((x, i) => (i === index - 1 ? { ...prev, runs: merged } : x))
        .filter((_, i) => i !== index);
      setBlocks(withEditedBlock(next, index - 1, applyTextEdit(merged, plainText(merged))));
      setFocused(index - 1);
      setPendingCaret({ index: index - 1, caret });
    }
  };

  const cancel = async (): Promise<void> => {
    if (!hasContent(title(), blocks())) {
      props.onCancel();
      return;
    }
    const discard = await requestConfirm({
      title: t("article.discard"),
      body: null,
      confirmLabel: t("editor.discard"),
      cancelLabel: t("editor.keep_editing"),
      tone: "danger",
    });
    if (discard) props.onCancel();
  };

  const publish = async (): Promise<void> => {
    if (!canPublish()) return;
    setBusy(true);
    try {
      await props.onPublish(out());
    } finally {
      setBusy(false);
    }
  };

  onMount(() => {
    const onEsc = (e: KeyboardEvent): void => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if (linkDialog() !== null) {
        e.preventDefault();
        setLinkDialog(null);
        return;
      }
      if (document.querySelector(".lk-confirm-dialog") !== null) return;
      e.preventDefault();
      void cancel();
    };
    document.addEventListener("keydown", onEsc);
    onCleanup(() => document.removeEventListener("keydown", onEsc));
  });

  const listNumber = (index: number): number => {
    let n = 0;
    const list = blocks();
    const b = list[index];
    if (b === undefined || b.kind !== "list-item") return 0;
    for (let i = index; i >= 0; i -= 1) {
      const x = list[i];
      if (x === undefined || x.kind !== "list-item" || x.ordered !== b.ordered) break;
      if (x.depth === b.depth) n += 1;
    }
    return n;
  };

  const headingLabel = (): string => {
    const b = focusedBlock();
    const next = nextHeadingKind(b?.kind ?? "paragraph");
    return next === "h1"
      ? t("article.format.heading")
      : next === "h2"
        ? t("article.format.subheading")
        : t("article.format.text");
  };

  const formatButton = (label: string, onPress: () => void, opts?: { pressed?: () => boolean; disabled?: () => boolean; glyph?: JSX.Element }): JSX.Element => (
    <button
      type="button"
      aria-pressed={opts?.pressed === undefined ? undefined : opts.pressed()}
      disabled={opts?.disabled?.() ?? false}
      title={label}
      aria-label={label}
      onMouseDown={(e) => e.preventDefault()}
      onClick={() => onPress()}
    >
      {opts?.glyph ?? label}
    </button>
  );

  return (
    <section class="lk-article-screen" role="dialog" aria-modal="true" aria-labelledby="lk-article-editor-title">
      <header class="lk-article-bar">
        <button
          type="button"
          class="lk-icon-button"
          aria-label={t("common.close")}
          title={t("common.close")}
          onClick={() => void cancel()}
        >
          <Icon name="close" />
        </button>
        <h2 id="lk-article-editor-title" class="lk-article-bar-meta" style={{ margin: 0, "font-size": "17px" }}>
          {t(props.editing === true ? "article.editor.edit_title" : "article.editor.title")}
        </h2>
        <Show when={props.editing !== true}>
          <button
            type="button"
            class="lk-ak-secondary-button"
            disabled={!hasContent(title(), blocks()) || busy()}
            onClick={() => props.onSaveDraft(out())}
          >
            {t("article.save_draft")}
          </button>
        </Show>
        <button type="button" class="lk-ak-primary-button" disabled={!canPublish()} onClick={() => void publish()}>
          {t("article.publish")}
        </button>
      </header>
      <div class="lk-article-format" role="toolbar" aria-label={t("article.editor.title")}>
        {formatButton(headingLabel(), cycleHeading, { disabled: () => !focusedIsText() })}
        {formatButton(t("article.format.bold"), () => applyMark("bold"), {
          pressed: () => markActive("bold"),
          disabled: () => !focusedIsText(),
          glyph: <strong>B</strong>,
        })}
        {formatButton(t("article.format.italic"), () => applyMark("italic"), {
          pressed: () => markActive("italic"),
          disabled: () => !focusedIsText(),
          glyph: <em>I</em>,
        })}
        {formatButton(t("article.format.bullets"), () => setKind({ list: false }), {
          pressed: () => {
            const b = focusedBlock();
            return b?.kind === "list-item" && !b.ordered;
          },
          disabled: () => !focusedIsText(),
        })}
        {formatButton(t("article.format.numbers"), () => setKind({ list: true }), {
          pressed: () => {
            const b = focusedBlock();
            return b?.kind === "list-item" && b.ordered;
          },
          disabled: () => !focusedIsText(),
        })}
        {formatButton(t("article.format.quote"), () => setKind("quote"), {
          pressed: () => focusedBlock()?.kind === "quote",
          disabled: () => !focusedIsText(),
        })}
        {formatButton(t("article.format.link"), openLink, { disabled: () => !focusedIsText() })}
        {formatButton(t("article.format.image"), pickImage, {
          disabled: () => imageCount(blocks()) >= props.maxImages,
        })}
        {formatButton(t("article.format.divider"), addDivider)}
        <input
          ref={fileInput}
          type="file"
          accept="image/*"
          hidden
          onChange={(e) => {
            onImagePicked(e.currentTarget.files?.[0]);
            e.currentTarget.value = "";
          }}
        />
      </div>
      <Show when={atLimit()}>
        <div class="lk-article-limit" role="status">{t("article.at_limit")}</div>
      </Show>
      <div class="lk-article-scroll">
        <div class="lk-article-column">
          <input
            class="lk-article-title-input"
            type="text"
            maxLength={TITLE_MAX_CHARS}
            placeholder={t("article.title")}
            aria-label={t("article.title")}
            value={title()}
            onInput={(e) => setTitle(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                setFocused(0);
                setPendingCaret({ index: 0, caret: 0 });
              }
            }}
          />
          <For each={blocks()}>
            {(block, index) => (
              <EditorBlockView
                block={block}
                index={index()}
                focused={focused() === index()}
                listNumber={listNumber(index())}
                imageUrl={block.kind === "image" ? imageUrls()[block.image] : undefined}
                pendingCaret={pendingCaret()}
                onCaretTaken={() => setPendingCaret(null)}
                onFocus={() => setFocused(index())}
                onInput={(el) => onTextInput(index(), el)}
                onSelect={(el) => rememberSelection(index(), el)}
                onKeyDown={(e) => onKey(index(), e)}
                onPaste={(e) => onPaste(index(), e)}
                onCaption={(text) => {
                  const b = blocks()[index()];
                  if (b?.kind !== "image") return;
                  replaceRuns(index(), applyTextEdit(b.caption, text.replace(/\r\n|\r|\n/g, " ")));
                }}
                onRemove={() => removeBlock(index())}
              />
            )}
          </For>
        </div>
      </div>
      <Show when={linkDialog()}>
        {(d) => (
          <div class="lk-article-dialog" onClick={(e) => { if (e.target === e.currentTarget) setLinkDialog(null); }}>
            <div class="lk-article-dialog-card" role="dialog" aria-modal="true" aria-label={t("article.format.link")}>
              <div class="lk-article-dialog-fields">
                <input
                  ref={(el) => queueMicrotask(() => el.focus())}
                  type="url"
                  inputMode="url"
                  aria-label={t("article.link.url")}
                  placeholder={t("article.link.url")}
                  value={d().url}
                  onInput={(e) => setLinkDialog({ ...d(), url: e.currentTarget.value })}
                  onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); commitLink(); } }}
                />
                <input
                  type="text"
                  aria-label={t("article.link.text")}
                  placeholder={t("article.link.text")}
                  value={d().text}
                  onInput={(e) => setLinkDialog({ ...d(), text: e.currentTarget.value })}
                  onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); commitLink(); } }}
                />
                <Show when={d().url.trim().length > "https://".length && !props.urlValid(d().url.trim())}>
                  <span class="lk-ak-error" role="status">{t("article.link.invalid")}</span>
                </Show>
              </div>
              <div class="lk-article-dialog-actions">
                <button type="button" class="lk-ak-secondary-button" onClick={() => setLinkDialog(null)}>
                  {t("common.cancel")}
                </button>
                <button
                  type="button"
                  class="lk-ak-primary-button"
                  disabled={!props.urlValid(d().url.trim()) || d().text.trim().length === 0}
                  onClick={commitLink}
                >
                  {t("article.link.insert")}
                </button>
              </div>
            </div>
          </div>
        )}
      </Show>
    </section>
  );
}

function EditorBlockView(props: {
  readonly block: EditorBlock;
  readonly index: number;
  readonly focused: boolean;
  readonly listNumber: number;
  readonly imageUrl: string | undefined;
  readonly pendingCaret: { index: number; caret: number } | null;
  readonly onCaretTaken: () => void;
  readonly onFocus: () => void;
  readonly onInput: (el: HTMLTextAreaElement) => void;
  readonly onSelect: (el: HTMLTextAreaElement) => void;
  readonly onKeyDown: (e: KeyboardEvent & { currentTarget: HTMLTextAreaElement }) => void;
  readonly onPaste: (e: ClipboardEvent) => void;
  readonly onCaption: (text: string) => void;
  readonly onRemove: () => void;
}): JSX.Element {
  const b = (): EditorBlock => props.block;
  const runs = (): readonly ArticleRun[] => {
    const x = b();
    return x.kind === "divider" ? [] : x.kind === "image" ? x.caption : x.runs;
  };
  const cls = (): string => {
    const x = b();
    const depth = x.kind === "list-item" && x.depth === 1 ? " depth-1" : "";
    return `lk-article-block ${x.kind}${depth}${props.focused ? " focused" : ""}`;
  };
  const textArea = (): JSX.Element => {
    let el: HTMLTextAreaElement | undefined;
    createEffect(() => {
      const pc = props.pendingCaret;
      if (el === undefined || pc === null || pc.index !== props.index) return;
      const caret = Math.min(pc.caret, el.value.length);
      el.focus();
      el.setSelectionRange(caret, caret);
      props.onCaretTaken();
    });
    return (
      <textarea
        ref={(x) => {
          el = x;
          queueMicrotask(() => {
            if (props.pendingCaret === null || props.pendingCaret.index !== props.index) x.focus();
          });
        }}
        rows={1}
        value={plainText(runs())}
        placeholder={props.index === 0 ? t("article.body") : ""}
        onInput={(e) => props.onInput(e.currentTarget)}
        onSelect={(e) => props.onSelect(e.currentTarget)}
        onKeyUp={(e) => props.onSelect(e.currentTarget)}
        onMouseUp={(e) => props.onSelect(e.currentTarget)}
        onKeyDown={(e) => props.onKeyDown(e)}
        onPaste={(e) => props.onPaste(e)}
      />
    );
  };
  const rendered = (): JSX.Element => (
    <div
      class={`lk-article-rendered${runs().length === 0 ? " empty" : ""}`}
      data-placeholder={props.index === 0 ? t("article.body") : ""}
      tabIndex={0}
      onClick={() => props.onFocus()}
      onFocus={() => props.onFocus()}
    >
      <For each={runs()}>
        {(r) => {
          const style = { "font-weight": r.bold ? "700" : undefined, "font-style": r.italic ? "italic" : undefined };
          return r.kind === "link" ? (
            <span class="lk-article-link" style={style}>{r.text}</span>
          ) : (
            <span style={style}>{r.text}</span>
          );
        }}
      </For>
    </div>
  );
  return (
    <div class={cls()}>
      <Switch fallback={props.focused ? textArea() : rendered()}>
        <Match when={b().kind === "divider"}>
          <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
            <hr style={{ flex: "1" }} />
            <button type="button" class="lk-icon-button" aria-label={t("common.remove")} title={t("common.remove")} onClick={() => props.onRemove()}>
              <Icon name="trash" size={16} />
            </button>
          </div>
        </Match>
        <Match when={b().kind === "image"}>
          <figure style={{ margin: "0", display: "flex", "flex-direction": "column", gap: "4px" }}>
            <Show when={props.imageUrl} fallback={<div class="lk-article-image-placeholder" />}>
              {(u) => <img src={u()} alt={plainText(runs()) || t("article.format.image")} style={{ width: "100%", "border-radius": "var(--radius-sm)" }} />}
            </Show>
            <div style={{ display: "flex", gap: "8px", "align-items": "center" }}>
              <input
                type="text"
                class="lk-article-title-input"
                style={{ "font-size": "0.875rem", "font-weight": "400" }}
                placeholder={t("article.caption")}
                aria-label={t("article.caption")}
                value={plainText(runs())}
                onFocus={() => props.onFocus()}
                onInput={(e) => props.onCaption(e.currentTarget.value)}
              />
              <button type="button" class="lk-icon-button" aria-label={t("common.remove")} title={t("common.remove")} onClick={() => props.onRemove()}>
                <Icon name="trash" size={16} />
              </button>
            </div>
          </figure>
        </Match>
        <Match when={b().kind === "list-item"}>
          <span class="lk-article-bullet" aria-hidden="true">
            {(() => {
              const x = b();
              return x.kind === "list-item" && x.ordered ? `${props.listNumber}.` : "•";
            })()}
          </span>
          <div style={{ flex: "1", "min-width": "0" }}>{props.focused ? textArea() : rendered()}</div>
        </Match>
      </Switch>
    </div>
  );
}
