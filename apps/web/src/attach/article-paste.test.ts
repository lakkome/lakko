// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { blocksFromPastedHtml, type PasteNode } from "./article-paste";

function el(name: string, children: PasteNode[] = [], attrs: Record<string, string> = {}): PasteNode {
  return { nodeType: 1, nodeName: name, textContent: null, childNodes: children, getAttribute: (n) => attrs[n] ?? null };
}
function text(s: string): PasteNode {
  return { nodeType: 3, nodeName: "#text", textContent: s, childNodes: [] };
}
const https = (u: string): boolean => u.startsWith("https://");

describe("pasting rich text into an article (UX 11.1)", () => {
  it("keeps headings, bold, italic, quotes, lists and https links", () => {
    const root = el("BODY", [
      el("H1", [text("Title")]),
      el("P", [text("Plain "), el("B", [text("bold")]), text(" and "), el("I", [text("it")])]),
      el("BLOCKQUOTE", [text("Quoted")]),
      el("UL", [el("LI", [text("one")]), el("LI", [text("two"), el("UL", [el("LI", [text("deep")])])])]),
      el("P", [el("A", [text("site")], { href: "https://example.org" })]),
    ]);
    const blocks = blocksFromPastedHtml(root, https);
    expect(blocks.map((b) => b.kind)).toEqual(["h1", "paragraph", "quote", "list-item", "list-item", "list-item"].concat(["paragraph"]));
    const para = blocks[1];
    expect(para?.kind === "paragraph" ? para.runs.map((r) => [r.text, r.bold, r.italic]) : null).toEqual([
      ["Plain ", false, false],
      ["bold", true, false],
      [" and ", false, false],
      ["it", false, true],
    ]);
    const deep = blocks[5];
    expect(deep?.kind === "list-item" ? deep.depth : null).toBe(1);
    const link = blocks[6];
    expect(link?.kind === "paragraph" ? link.runs[0] : null).toMatchObject({ kind: "link", url: "https://example.org", text: "site" });
  });

  it("turns everything else into plain text and drops scripts and styles", () => {
    const root = el("BODY", [
      el("SCRIPT", [text("alert(1)")]),
      el("STYLE", [text("p{}")]),
      el("P", [el("SPAN", [text("span ")]), el("U", [text("under")]), el("A", [text("bad")], { href: "javascript:x" })]),
    ]);
    const blocks = blocksFromPastedHtml(root, https);
    expect(blocks).toHaveLength(1);
    const b = blocks[0];
    expect(b?.kind === "paragraph" ? b.runs : null).toEqual([{ kind: "text", bold: false, italic: false, text: "span underbad" }]);
  });

  it("splits on line breaks and drops empty blocks", () => {
    const root = el("DIV", [text("a"), el("BR"), text("b"), el("P", [text("   ")])]);
    expect(blocksFromPastedHtml(root, https).length).toBe(2);
  });
});
