// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The pure half of `legal-pages.mjs` (docs/ux/legal-pages.md): turn the
 * Markdown sources in `docs/legal/` into standalone HTML pages. No I/O, no
 * dependencies, so the vitest suite can drive it.
 *
 * Supported Markdown, and nothing more: `#` and `##` headings, paragraphs,
 * `-` bullet lists (a continuation line indented under an item joins it),
 * `**bold**`, `[text](url)` and `` `code` ``. Every piece of text is HTML
 * escaped; a link URL is kept only when it is `https:`, `http:`, `mailto:`
 * or a same-site path, otherwise the link text is shown plain.
 */

/** The line a page shows while its source still has unfinished parts. */
export const STUB_LINE = "This page is being prepared.";

/** Markers that mean a source is not ready to publish. */
const UNFINISHED_MARKERS = ["TODO", "FIX BEFORE PUBLISHING"];

/** @param {string} text */
export function escapeHtml(text) {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/** @param {string} source */
export function stripHtmlComments(source) {
  return source.replace(/<!--[\s\S]*?-->/g, "");
}

/** Whether a source (comments removed) still carries an unfinished marker.
 *  @param {string} source */
export function isUnfinished(source) {
  const body = stripHtmlComments(source);
  return UNFINISHED_MARKERS.some((m) => body.includes(m));
}

/** @param {string} url */
function safeUrl(url) {
  const trimmed = url.trim();
  if (/^(https?:|mailto:)/i.test(trimmed)) return trimmed;
  if (trimmed.startsWith("/") && !trimmed.startsWith("//")) return trimmed;
  return null;
}

/**
 * Inline Markdown to HTML. Code spans first (their content is literal), then
 * links and bold over escaped text.
 * @param {string} text
 */
export function renderInline(text) {
  const parts = text.split(/(`[^`]*`)/);
  return parts
    .map((part, i) => {
      if (i % 2 === 1) return `<code>${escapeHtml(part.slice(1, -1))}</code>`;
      return renderLinksAndBold(part);
    })
    .join("");
}

/** @param {string} text */
function renderLinksAndBold(text) {
  let out = "";
  const re = /\[([^\]]*)\]\(([^)\s]*)\)/g;
  let last = 0;
  let m;
  while ((m = re.exec(text)) !== null) {
    out += renderBold(text.slice(last, m.index));
    const label = renderBold(m[1] ?? "");
    const url = safeUrl(m[2] ?? "");
    out += url === null ? label : `<a href="${escapeHtml(url)}">${label}</a>`;
    last = m.index + m[0].length;
  }
  return out + renderBold(text.slice(last));
}

/** @param {string} text */
function renderBold(text) {
  return escapeHtml(text).replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>");
}

/**
 * Block Markdown to HTML body content.
 * @param {string} source
 * @returns {{ title: string | null, html: string }}
 */
export function renderMarkdown(source) {
  const lines = stripHtmlComments(source).replace(/\r\n?/g, "\n").split("\n");
  /** @type {string[]} */
  const out = [];
  /** @type {string | null} */
  let title = null;
  /** @type {string[]} */
  let paragraph = [];
  /** @type {string[]} */
  let items = [];

  const flushParagraph = () => {
    if (paragraph.length > 0) out.push(`<p>${renderInline(paragraph.join(" "))}</p>`);
    paragraph = [];
  };
  const flushList = () => {
    if (items.length > 0) {
      out.push(`<ul>\n${items.map((it) => `<li>${renderInline(it)}</li>`).join("\n")}\n</ul>`);
    }
    items = [];
  };

  for (const raw of lines) {
    const line = raw.trimEnd();
    if (line.trim() === "") {
      flushParagraph();
      flushList();
      continue;
    }
    const heading = /^(#{1,2})\s+(.*)$/.exec(line);
    if (heading) {
      flushParagraph();
      flushList();
      const level = (heading[1] ?? "#").length;
      const text = (heading[2] ?? "").trim();
      if (level === 1 && title === null) title = text;
      out.push(`<h${level}>${renderInline(text)}</h${level}>`);
      continue;
    }
    const bullet = /^-\s+(.*)$/.exec(line);
    if (bullet) {
      flushParagraph();
      items.push((bullet[1] ?? "").trim());
      continue;
    }
    if (items.length > 0 && /^\s+\S/.test(line)) {
      items[items.length - 1] += ` ${line.trim()}`;
      continue;
    }
    flushList();
    paragraph.push(line.trim());
  }
  flushParagraph();
  flushList();
  return { title, html: out.join("\n") };
}

const STYLE = `
:root { color-scheme: light dark; --bg: #ffffff; --fg: #1b1d22; --muted: #5f6470; --link: #2856d6; --code: #f1f2f5; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #15171c; --fg: #e8e9ec; --muted: #9ba0ab; --link: #8aa8ff; --code: #23262e; }
}
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; }
body { margin: 0; background: var(--bg); color: var(--fg); font: 16px/1.6 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }
main { max-width: 42rem; margin: 0 auto; padding: 32px 16px 64px; overflow-wrap: anywhere; }
h1 { font-size: 1.75rem; line-height: 1.25; margin: 0 0 16px; }
h2 { font-size: 1.2rem; line-height: 1.3; margin: 32px 0 8px; }
p, ul { margin: 0 0 12px; }
ul { padding-inline-start: 1.25rem; }
li { margin: 4px 0; }
a { color: var(--link); }
code { background: var(--code); padding: 1px 4px; border-radius: 4px; font-size: 0.9em; }
.muted { color: var(--muted); }
`;

/**
 * A complete standalone page.
 * @param {{ title: string, bodyHtml: string }} page
 */
export function htmlPage({ title, bodyHtml }) {
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escapeHtml(title)}</title>
<style>${STYLE}</style>
</head>
<body>
<main>
${bodyHtml}
</main>
</body>
</html>
`;
}

/**
 * The page for one source: the rendered text, or the stub while the source
 * is unfinished.
 * @param {string} source
 * @param {string} fallbackTitle the title when the source has no `#` heading,
 *   and the stub's title
 * @returns {{ html: string, stub: boolean }}
 */
export function legalPage(source, fallbackTitle) {
  if (isUnfinished(source)) {
    return {
      stub: true,
      html: htmlPage({
        title: fallbackTitle,
        bodyHtml: `<h1>${escapeHtml(fallbackTitle)}</h1>\n<p class="muted">${escapeHtml(STUB_LINE)}</p>`,
      }),
    };
  }
  const { title, html } = renderMarkdown(source);
  return { stub: false, html: htmlPage({ title: title ?? fallbackTitle, bodyHtml: html }) };
}
