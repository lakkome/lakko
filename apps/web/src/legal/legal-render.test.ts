// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The renderer behind the static Terms of Service and Privacy Policy pages
 * (`scripts/legal-pages.mjs`, docs/ux/legal-pages.md section 1).
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  STUB_LINE,
  isUnfinished,
  legalPage,
  renderInline,
  renderMarkdown,
} from "../../scripts/legal-render.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));

describe("legal page renderer", () => {
  it("renders headings, paragraphs and lists", () => {
    const { title, html } = renderMarkdown(
      "# Lakko Terms\n\nFirst line\nsame paragraph.\n\n## 1. Who\n\n- one\n  continued\n- two\n\nAfter.",
    );
    expect(title).toBe("Lakko Terms");
    expect(html).toBe(
      [
        "<h1>Lakko Terms</h1>",
        "<p>First line same paragraph.</p>",
        "<h2>1. Who</h2>",
        "<ul>\n<li>one continued</li>\n<li>two</li>\n</ul>",
        "<p>After.</p>",
      ].join("\n"),
    );
  });

  it("renders bold, links and code, escaping everything else", () => {
    expect(renderInline("**VVG** & <b> [Privacy](https://lakko.me/privacy) `a<b>`")).toBe(
      '<strong>VVG</strong> &amp; &lt;b&gt; <a href="https://lakko.me/privacy">Privacy</a> <code>a&lt;b&gt;</code>',
    );
  });

  it("keeps no link with an unsafe scheme", () => {
    expect(renderInline("[x](javascript:alert(1))")).not.toContain("href");
    expect(renderInline('[x](https://a.example/"onload=)')).toBe(
      '<a href="https://a.example/&quot;onload=">x</a>',
    );
  });

  it("drops HTML comments", () => {
    expect(renderMarkdown("<!-- secret\nnote -->\n# T\n\nBody").html).toBe("<h1>T</h1>\n<p>Body</p>");
  });

  it("writes a complete standalone page with no script", () => {
    const { html, stub } = legalPage("# Terms <x>\n\nBody.", "Terms of Service");
    expect(stub).toBe(false);
    expect(html.startsWith("<!doctype html>")).toBe(true);
    expect(html).toContain('<html lang="en">');
    expect(html).toContain('<meta charset="utf-8">');
    expect(html).toContain('name="viewport"');
    expect(html).toContain("<title>Terms &lt;x&gt;</title>");
    expect(html).toContain("prefers-color-scheme: dark");
    expect(html).not.toContain("<script");
  });

  it("writes the stub while the source still has TODO or FIX BEFORE PUBLISHING", () => {
    expect(isUnfinished("# T\n\nLast updated: TODO")).toBe(true);
    expect(isUnfinished("# T\n\n[FIX BEFORE PUBLISHING: x]")).toBe(true);
    // A marker inside a comment alone does not count.
    expect(isUnfinished("<!-- Fill every TODO. -->\n# T\n\nDone.")).toBe(false);
    const { html, stub } = legalPage("# T\n\nTODO", "Privacy Policy");
    expect(stub).toBe(true);
    expect(html).toContain("<title>Privacy Policy</title>");
    expect(html).toContain(STUB_LINE);
    expect(html).not.toContain("TODO");
  });

  it("the current sources render without throwing", () => {
    for (const name of ["terms.md", "privacy.md"]) {
      const source = readFileSync(join(HERE, "../../../../docs/legal", name), "utf8");
      const { html } = renderMarkdown(source);
      expect(html).toContain("<h1>");
      expect(html).not.toContain("<!--");
    }
  });
});
