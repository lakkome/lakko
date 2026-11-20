// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX 11.1 — "Pasting rich text keeps only what the format can carry
 * (headings, bold, italic, lists, quotes, links); everything else becomes
 * plain text." The pasted HTML is parsed into an inert document by the
 * browser (never inserted, never run) and walked here into editor blocks.
 * No markup survives: only the closed set of marks and block kinds.
 *
 * Written over a minimal node shape so it is tested without a DOM.
 */
import { canonicalRuns, type EditorBlock } from "./article-model";
import type { ArticleRun } from "./types";

/** The part of a DOM node this reads. */
export interface PasteNode {
  readonly nodeType: number;
  readonly nodeName: string;
  readonly textContent: string | null;
  readonly childNodes: ArrayLike<PasteNode>;
  getAttribute?(name: string): string | null;
}

const TEXT = 3;
const ELEMENT = 1;

interface Marks {
  readonly bold: boolean;
  readonly italic: boolean;
  readonly url: string | null;
}

const BLOCKS = new Set(["P", "DIV", "H1", "H2", "H3", "H4", "H5", "H6", "BLOCKQUOTE", "LI", "UL", "OL", "PRE", "SECTION", "ARTICLE", "TABLE", "TR"]);

/** Blocks from pasted HTML. `urlValid` is the core's link rule: a link to
 *  anything else stays as its text. */
export function blocksFromPastedHtml(root: PasteNode, urlValid: (url: string) => boolean): EditorBlock[] {
  const out: EditorBlock[] = [];
  let runs: ArticleRun[] = [];
  let kind: "paragraph" | "h1" | "h2" | "quote" | "list-item" = "paragraph";
  let list: { ordered: boolean; depth: 0 | 1 } | null = null;

  const flush = (): void => {
    const canon = canonicalRuns(runs.map((r) => ({ ...r, text: r.text.replace(/\s+/g, " ") })));
    const trimmed = trimRuns(canon);
    if (trimmed.length > 0) {
      if (kind === "list-item" && list !== null) {
        out.push({ kind: "list-item", ordered: list.ordered, depth: list.depth, runs: trimmed });
      } else {
        out.push({ kind: kind === "list-item" ? "paragraph" : kind, runs: trimmed });
      }
    }
    runs = [];
  };

  const walk = (node: PasteNode, marks: Marks, listDepth: number, ordered: boolean): void => {
    if (node.nodeType === TEXT) {
      const text = node.textContent ?? "";
      if (text.length === 0) return;
      runs.push(
        marks.url !== null
          ? { kind: "link", url: marks.url, bold: marks.bold, italic: marks.italic, text }
          : { kind: "text", bold: marks.bold, italic: marks.italic, text },
      );
      return;
    }
    if (node.nodeType !== ELEMENT && node.nodeName !== "#document" && node.nodeName !== "#document-fragment") {
      return;
    }
    const name = node.nodeName.toUpperCase();
    if (name === "SCRIPT" || name === "STYLE" || name === "TEMPLATE" || name === "HEAD") return;
    if (name === "BR") {
      flush();
      return;
    }
    let next = marks;
    if (name === "B" || name === "STRONG") next = { ...next, bold: true };
    if (name === "I" || name === "EM") next = { ...next, italic: true };
    if (name === "A") {
      const href = node.getAttribute?.("href") ?? null;
      if (href !== null && urlValid(href)) next = { ...next, url: href };
    }
    const isBlock = BLOCKS.has(name);
    const prevKind = kind;
    const prevList = list;
    if (isBlock) flush();
    if (name === "H1") kind = "h1";
    else if (name === "H2" || name === "H3" || name === "H4" || name === "H5" || name === "H6") kind = "h2";
    else if (name === "BLOCKQUOTE") kind = "quote";
    let depth = listDepth;
    let ord = ordered;
    if (name === "UL" || name === "OL") {
      depth = listDepth + 1;
      ord = name === "OL";
    }
    if (name === "LI") {
      kind = "list-item";
      list = { ordered: ord, depth: depth >= 2 ? 1 : 0 };
    }
    for (let i = 0; i < node.childNodes.length; i++) {
      const child = node.childNodes[i];
      if (child !== undefined) walk(child, next, depth, ord);
    }
    if (isBlock) {
      flush();
      kind = prevKind;
      list = prevList;
    }
  };

  walk(root, { bold: false, italic: false, url: null }, 0, false);
  flush();
  return out;
}

function trimRuns(runs: ArticleRun[]): ArticleRun[] {
  const out = [...runs];
  while (out.length > 0 && out[0]!.text.trim().length === 0) out.shift();
  while (out.length > 0 && out[out.length - 1]!.text.trim().length === 0) out.pop();
  if (out.length === 0) return out;
  out[0] = { ...out[0]!, text: out[0]!.text.replace(/^\s+/, "") };
  const last = out.length - 1;
  out[last] = { ...out[last]!, text: out[last]!.text.replace(/\s+$/, "") };
  return canonicalRuns(out);
}
