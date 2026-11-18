// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 8 — the article editor's model, pure and testable.
 *
 * The editor keeps a flat list of {@link EditorBlock}s (one per editable
 * element) whose text is a list of runs. Everything the core requires of an
 * article is produced here, never by the view: runs are canonical (no empty
 * run, adjacent runs with the same marks merged), no text contains a line
 * break (a break splits the block), consecutive list items become one list
 * block, and image blocks index the images in the order they appear.
 */
import type { Article, ArticleBlock, ArticleRun } from "../../wasm/lakko_ffi_wasm";

export type { Article, ArticleBlock, ArticleRun };

/** One editable element of the body. */
export type EditorBlock =
  | { readonly kind: "paragraph" | "h1" | "h2" | "quote"; readonly runs: readonly ArticleRun[] }
  | {
      readonly kind: "list-item";
      readonly ordered: boolean;
      readonly depth: 0 | 1;
      readonly runs: readonly ArticleRun[];
    }
  /** `image` indexes the editor's image list (not the article's). */
  | { readonly kind: "image"; readonly image: number; readonly caption: readonly ArticleRun[] }
  | { readonly kind: "divider" };

export type Mark = "bold" | "italic";

const LINE_BREAK = /\r\n|\r|\n|\u2028|\u2029/;

function sameMarks(a: ArticleRun, b: ArticleRun): boolean {
  if (a.bold !== b.bold || a.italic !== b.italic) return false;
  if (a.kind === "text" && b.kind === "text") return true;
  if (a.kind === "link" && b.kind === "link") return a.url === b.url;
  return false;
}

/** Drop empty runs and merge adjacent runs with the same marks (and URL). */
export function canonicalRuns(runs: readonly ArticleRun[]): ArticleRun[] {
  const out: ArticleRun[] = [];
  for (const run of runs) {
    if (run.text.length === 0) continue;
    const last = out[out.length - 1];
    if (last !== undefined && sameMarks(last, run)) {
      out[out.length - 1] = { ...last, text: last.text + run.text };
    } else {
      out.push({ ...run });
    }
  }
  return out;
}

/** Split runs at line breaks: one canonical run list per line. */
export function splitRunsOnNewlines(runs: readonly ArticleRun[]): ArticleRun[][] {
  const lines: ArticleRun[][] = [[]];
  for (const run of runs) {
    const parts = run.text.split(LINE_BREAK);
    parts.forEach((part, i) => {
      if (i > 0) lines.push([]);
      (lines[lines.length - 1] as ArticleRun[]).push({ ...run, text: part });
    });
  }
  return lines.map(canonicalRuns);
}

/** The plain text of a run list. */
export function plainText(runs: readonly ArticleRun[]): string {
  return runs.map((r) => r.text).join("");
}

/** UTF-8 byte length. */
export function utf8Bytes(s: string): number {
  return new TextEncoder().encode(s).length;
}

/** The text bytes of every run of an article (title excluded, URLs excluded:
 *  the core's bound is on run text). */
export function articleTextBytes(article: Article): number {
  let n = 0;
  const add = (runs: readonly ArticleRun[]): void => {
    for (const r of runs) n += utf8Bytes(r.text);
  };
  for (const b of article.blocks) {
    if (b.kind === "paragraph" || b.kind === "heading" || b.kind === "quote") add(b.runs);
    else if (b.kind === "list") for (const it of b.items) add(it.runs);
    else if (b.kind === "image") add(b.caption);
  }
  return n;
}

/** The text bytes of the editor's blocks. */
export function blocksTextBytes(blocks: readonly EditorBlock[]): number {
  let n = 0;
  for (const b of blocks) {
    if (b.kind === "divider") continue;
    const runs = b.kind === "image" ? b.caption : b.runs;
    for (const r of runs) n += utf8Bytes(r.text);
  }
  return n;
}

/** Split runs at a character offset (UTF-16 code units). */
function splitAt(runs: readonly ArticleRun[], offset: number): [ArticleRun[], ArticleRun[]] {
  const left: ArticleRun[] = [];
  const right: ArticleRun[] = [];
  let pos = 0;
  for (const run of runs) {
    const end = pos + run.text.length;
    if (end <= offset) left.push(run);
    else if (pos >= offset) right.push(run);
    else {
      left.push({ ...run, text: run.text.slice(0, offset - pos) });
      right.push({ ...run, text: run.text.slice(offset - pos) });
    }
    pos = end;
  }
  return [left, right];
}

/** Split a run list into before / within / after a range. */
function splitRange(
  runs: readonly ArticleRun[],
  start: number,
  end: number,
): [ArticleRun[], ArticleRun[], ArticleRun[]] {
  const [left, rest] = splitAt(runs, start);
  const [mid, right] = splitAt(rest, end - start);
  return [left, mid, right];
}

/**
 * Apply a plain-text edit to runs, keeping marks: the common prefix and
 * suffix of the old and new text stay; the replaced middle takes the marks of
 * the run before it (or after it at the start). Line breaks are kept here;
 * the caller splits them into blocks.
 */
export function applyTextEdit(runs: readonly ArticleRun[], next: string): ArticleRun[] {
  const prev = plainText(runs);
  if (prev === next) return canonicalRuns(runs);
  let p = 0;
  while (p < prev.length && p < next.length && prev[p] === next[p]) p += 1;
  let s = 0;
  while (
    s < prev.length - p &&
    s < next.length - p &&
    prev[prev.length - 1 - s] === next[next.length - 1 - s]
  ) {
    s += 1;
  }
  const [left, , right] = splitRange(runs, p, prev.length - s);
  const inserted = next.slice(p, next.length - s);
  if (inserted.length === 0) return canonicalRuns([...left, ...right]);
  const style: ArticleRun | undefined = left[left.length - 1] ?? right[0];
  const run: ArticleRun =
    style === undefined
      ? { kind: "text", bold: false, italic: false, text: inserted }
      : { ...style, text: inserted };
  return canonicalRuns([...left, run, ...right]);
}

/** Toggle a mark over `[start, end)`: removed when every character in the
 *  range has it, added otherwise. An empty range changes nothing. */
export function toggleMark(
  runs: readonly ArticleRun[],
  start: number,
  end: number,
  mark: Mark,
): ArticleRun[] {
  if (end <= start) return canonicalRuns(runs);
  const [left, mid, right] = splitRange(runs, start, end);
  const all = mid.length > 0 && mid.every((r) => r[mark]);
  const changed = mid.map((r) => ({ ...r, [mark]: !all }));
  return canonicalRuns([...left, ...changed, ...right]);
}

/** Replace `[start, end)` with a link run of `text` to `url`. */
export function insertLink(
  runs: readonly ArticleRun[],
  start: number,
  end: number,
  url: string,
  text: string,
): ArticleRun[] {
  const [left, , right] = splitRange(runs, start, Math.max(start, end));
  const clean = text.replace(new RegExp(LINE_BREAK.source, "g"), " ");
  if (clean.length === 0) return canonicalRuns([...left, ...right]);
  return canonicalRuns([...left, { kind: "link", url, bold: false, italic: false, text: clean }, ...right]);
}

/** Whether every character of `[start, end)` carries `mark`. */
export function rangeHasMark(
  runs: readonly ArticleRun[],
  start: number,
  end: number,
  mark: Mark,
): boolean {
  if (end <= start) return false;
  const [, mid] = splitRange(runs, start, end);
  return mid.length > 0 && mid.every((r) => r[mark]);
}

/** Text runs from plain text (a pasted string keeps no formatting). */
export function plainRuns(text: string): ArticleRun[] {
  return canonicalRuns([{ kind: "text", bold: false, italic: false, text }]);
}

/** The block a line break inside `block` continues with. */
export function continuationOf(block: EditorBlock, runs: readonly ArticleRun[]): EditorBlock {
  if (block.kind === "list-item") return { ...block, runs };
  if (block.kind === "quote") return { kind: "quote", runs };
  return { kind: "paragraph", runs };
}

/**
 * Replace block `index` after a text edit that may contain line breaks: the
 * first line stays in the block, every further line becomes a block of the
 * continuation kind. Returns the new list.
 */
export function withEditedBlock(
  blocks: readonly EditorBlock[],
  index: number,
  runs: readonly ArticleRun[],
): EditorBlock[] {
  const block = blocks[index];
  if (block === undefined || block.kind === "divider") return [...blocks];
  const lines = splitRunsOnNewlines(runs);
  const first = lines[0] ?? [];
  const head: EditorBlock = block.kind === "image" ? { ...block, caption: first } : { ...block, runs: first };
  if (block.kind === "image") {
    // A caption has no lines: join them with spaces.
    const joined = canonicalRuns(lines.flatMap((l, i) => (i === 0 ? l : [{ kind: "text", bold: false, italic: false, text: " " } as ArticleRun, ...l])));
    return blocks.map((b, i) => (i === index ? { ...block, caption: joined } : b));
  }
  const rest = lines.slice(1).map((l) => continuationOf(block, l));
  return [...blocks.slice(0, index), head, ...rest, ...blocks.slice(index + 1)];
}

function runsOfBlock(block: EditorBlock): readonly ArticleRun[] {
  if (block.kind === "divider") return [];
  return block.kind === "image" ? block.caption : block.runs;
}

/**
 * Build the article from the editor: canonical runs, empty text blocks
 * dropped, consecutive list items of one kind merged into one list, images
 * renumbered in order of appearance (unused images dropped, each used once).
 */
export function buildArticle(
  title: string,
  blocks: readonly EditorBlock[],
  images: readonly Blob[],
): { article: Article; images: Blob[] } {
  const out: ArticleBlock[] = [];
  const usedImages: Blob[] = [];
  const seen = new Map<number, number>();
  for (const block of blocks) {
    if (block.kind === "divider") {
      out.push({ kind: "divider" });
      continue;
    }
    if (block.kind === "image") {
      const blob = images[block.image];
      if (blob === undefined || seen.has(block.image)) continue;
      seen.set(block.image, usedImages.length);
      out.push({ kind: "image", attachmentIndex: usedImages.length, caption: canonicalRuns(block.caption) });
      usedImages.push(blob);
      continue;
    }
    const runs = canonicalRuns(runsOfBlock(block));
    if (runs.length === 0) continue;
    if (block.kind === "list-item") {
      const last = out[out.length - 1];
      if (last !== undefined && last.kind === "list" && last.ordered === block.ordered) {
        // A list starts at depth 0.
        out[out.length - 1] = { ...last, items: [...last.items, { depth: block.depth, runs }] };
      } else {
        out.push({ kind: "list", ordered: block.ordered, items: [{ depth: 0, runs }] });
      }
      continue;
    }
    if (block.kind === "h1" || block.kind === "h2") {
      out.push({ kind: "heading", level: block.kind, runs });
      continue;
    }
    out.push({ kind: block.kind, runs });
  }
  return { article: { title: title.trim(), blocks: out }, images: usedImages };
}

/** The editor blocks of an existing article (an edit, a draft). Image blocks
 *  keep their attachment index as the editor image index. */
export function editorBlocksOf(article: Article): EditorBlock[] {
  const out: EditorBlock[] = [];
  for (const b of article.blocks) {
    switch (b.kind) {
      case "paragraph":
      case "quote":
        out.push({ kind: b.kind, runs: b.runs });
        break;
      case "heading":
        out.push({ kind: b.level, runs: b.runs });
        break;
      case "list":
        for (const it of b.items) {
          out.push({ kind: "list-item", ordered: b.ordered, depth: it.depth === 1 ? 1 : 0, runs: it.runs });
        }
        break;
      case "image":
        out.push({ kind: "image", image: b.attachmentIndex, caption: b.caption });
        break;
      case "divider":
        out.push({ kind: "divider" });
        break;
    }
  }
  return out;
}

/** The kind the Heading button moves a block to: Heading -> Subheading ->
 *  Text -> Heading. */
export function nextHeadingKind(kind: EditorBlock["kind"]): "h1" | "h2" | "paragraph" {
  if (kind === "h1") return "h2";
  if (kind === "h2") return "paragraph";
  return "h1";
}

/** Number of image blocks. */
export function imageCount(blocks: readonly EditorBlock[]): number {
  return blocks.filter((b) => b.kind === "image").length;
}

/** Whether the editor holds anything typed or added. */
export function hasContent(title: string, blocks: readonly EditorBlock[]): boolean {
  if (title.trim().length > 0) return true;
  return blocks.some((b) => b.kind === "divider" || b.kind === "image" || plainText(b.runs).trim().length > 0);
}
