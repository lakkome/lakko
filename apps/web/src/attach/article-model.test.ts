// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  applyTextEdit,
  articleTextBytes,
  buildArticle,
  canonicalRuns,
  editorBlocksOf,
  hasContent,
  insertLink,
  nextHeadingKind,
  plainText,
  splitRunsOnNewlines,
  toggleMark,
  withEditedBlock,
  type ArticleRun,
  type EditorBlock,
} from "./article-model";

const txt = (text: string, bold = false, italic = false): ArticleRun => ({ kind: "text", bold, italic, text });

describe("canonicalRuns", () => {
  it("drops empty runs and merges equal marks", () => {
    expect(canonicalRuns([txt("a"), txt(""), txt("b"), txt("c", true), txt("d", true)])).toEqual([
      txt("ab"),
      txt("cd", true),
    ]);
  });
  it("does not merge links with different URLs or a link with text", () => {
    const l1: ArticleRun = { kind: "link", url: "https://a", bold: false, italic: false, text: "x" };
    const l2: ArticleRun = { kind: "link", url: "https://b", bold: false, italic: false, text: "y" };
    expect(canonicalRuns([l1, l2, txt("z")])).toHaveLength(3);
    expect(canonicalRuns([l1, { ...l1, text: "q" }])).toEqual([{ ...l1, text: "xq" }]);
  });
});

describe("splitRunsOnNewlines", () => {
  it("splits every break kind and keeps marks", () => {
    const lines = splitRunsOnNewlines([txt("a\nb", true), txt("c\r\nd")]);
    expect(lines).toEqual([[txt("a", true)], [txt("b", true), txt("c")], [txt("d")]]);
  });
  it("leaves no line break inside any run", () => {
    for (const line of splitRunsOnNewlines([txt("x\u2028y\rz")])) {
      for (const r of line) expect(/[\n\r\u2028\u2029]/.test(r.text)).toBe(false);
    }
  });
});

describe("applyTextEdit", () => {
  it("inserts with the marks of the run before", () => {
    const runs = [txt("ab"), txt("cd", true)];
    expect(applyTextEdit(runs, "abcXd")).toEqual([txt("ab"), txt("cXd", true)]);
  });
  it("deletes across runs", () => {
    expect(applyTextEdit([txt("ab"), txt("cd", true)], "ad")).toEqual([txt("a"), txt("d", true)]);
  });
  it("types into an empty block as plain text", () => {
    expect(applyTextEdit([], "hi")).toEqual([txt("hi")]);
  });
});

describe("toggleMark", () => {
  it("adds when part of the range lacks it, removes when all have it", () => {
    const on = toggleMark([txt("hello")], 1, 3, "bold");
    expect(on).toEqual([txt("h"), txt("el", true), txt("lo")]);
    expect(toggleMark(on, 1, 3, "bold")).toEqual([txt("hello")]);
  });
  it("ignores an empty range", () => {
    expect(toggleMark([txt("a")], 1, 1, "italic")).toEqual([txt("a")]);
  });
});

describe("insertLink", () => {
  it("replaces the selection with one link run", () => {
    const out = insertLink([txt("see here now")], 4, 8, "https://x.example", "this");
    expect(out).toEqual([
      txt("see "),
      { kind: "link", url: "https://x.example", bold: false, italic: false, text: "this" },
      txt(" now"),
    ]);
  });
});

describe("withEditedBlock", () => {
  it("splits a paragraph on a line break", () => {
    const blocks: EditorBlock[] = [{ kind: "h1", runs: [txt("Title")] }];
    const out = withEditedBlock(blocks, 0, [txt("Ti\ntle")]);
    expect(out).toEqual([
      { kind: "h1", runs: [txt("Ti")] },
      { kind: "paragraph", runs: [txt("tle")] },
    ]);
  });
  it("continues a list item as a list item", () => {
    const blocks: EditorBlock[] = [{ kind: "list-item", ordered: true, depth: 1, runs: [] }];
    const out = withEditedBlock(blocks, 0, [txt("a\nb")]);
    expect(out[1]).toEqual({ kind: "list-item", ordered: true, depth: 1, runs: [txt("b")] });
  });
});

describe("buildArticle", () => {
  it("merges list items, drops empty blocks and renumbers images", () => {
    const a = new Blob(["a"]);
    const b = new Blob(["b"]);
    const blocks: EditorBlock[] = [
      { kind: "paragraph", runs: [] },
      { kind: "image", image: 1, caption: [txt("cap")] },
      { kind: "list-item", ordered: false, depth: 0, runs: [txt("one")] },
      { kind: "list-item", ordered: false, depth: 1, runs: [txt("two")] },
      { kind: "list-item", ordered: true, depth: 0, runs: [txt("n")] },
      { kind: "divider" },
      { kind: "image", image: 1, caption: [] },
    ];
    const { article, images } = buildArticle("  T ", blocks, [a, b]);
    expect(article.title).toBe("T");
    expect(images).toEqual([b]);
    expect(article.blocks).toEqual([
      { kind: "image", attachmentIndex: 0, caption: [txt("cap")] },
      { kind: "list", ordered: false, items: [{ depth: 0, runs: [txt("one")] }, { depth: 1, runs: [txt("two")] }] },
      { kind: "list", ordered: true, items: [{ depth: 0, runs: [txt("n")] }] },
      { kind: "divider" },
    ]);
  });
  it("starts every list at depth 0", () => {
    const { article } = buildArticle("t", [{ kind: "list-item", ordered: false, depth: 1, runs: [txt("x")] }], []);
    expect(article.blocks[0]).toEqual({ kind: "list", ordered: false, items: [{ depth: 0, runs: [txt("x")] }] });
  });
  it("round-trips through editorBlocksOf", () => {
    const { article } = buildArticle(
      "t",
      [
        { kind: "h2", runs: [txt("s", true)] },
        { kind: "quote", runs: [txt("q")] },
      ],
      [],
    );
    expect(buildArticle("t", editorBlocksOf(article), []).article).toEqual(article);
  });
});

describe("bytes and helpers", () => {
  it("counts UTF-8 bytes of run text", () => {
    const { article } = buildArticle("t", [{ kind: "paragraph", runs: [txt("é€")] }], []);
    expect(articleTextBytes(article)).toBe(5);
  });
  it("cycles heading kinds", () => {
    expect(nextHeadingKind("paragraph")).toBe("h1");
    expect(nextHeadingKind("h1")).toBe("h2");
    expect(nextHeadingKind("h2")).toBe("paragraph");
  });
  it("knows when something was typed", () => {
    expect(hasContent(" ", [{ kind: "paragraph", runs: [] }])).toBe(false);
    expect(hasContent("", [{ kind: "divider" }])).toBe(true);
    expect(plainText([txt("a"), txt("b", true)])).toBe("ab");
  });
});
