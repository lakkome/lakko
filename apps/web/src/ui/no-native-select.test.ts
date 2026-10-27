// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The guard that the browser's own pickers and dialogs do not come back
 * (design system section 6: no operating-system surface inside a Lakko
 * screen unless the platform leaves no other way).
 *
 * - `<select>` opens the operating system's picker on a phone; one value
 *   from a list is `LakkoPicker` (`ui/LakkoPicker.tsx`).
 * - `<datalist>` is drawn differently by every browser and as a system strip
 *   on a phone; the recovery-phrase suggestions are `ui/WordSuggest.tsx`.
 * - `window.alert` / `window.prompt` render in the browser's voice and can be
 *   switched off by the person (see `no-window-confirm.test.ts` for
 *   `confirm`); a notice is `requestConfirm({ notice: true })`.
 * - `<input type="date|time|color|...">` opens a system picker.
 *
 * Tags and calls are matched in source with comments and string literals
 * removed, so prose about them (this header included) is not a hit. The input
 * types are matched in source with comments removed only, since the type is a
 * string.
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const SRC_ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");

function stripComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, " ").replace(/(^|[^:])\/\/[^\n]*/g, "$1 ");
}

function stripCommentsAndStrings(src: string): string {
  return stripComments(src)
    .replace(/`(?:\\.|[^`\\])*`/g, " ")
    .replace(/"(?:\\.|[^"\\\n])*"/g, " ")
    .replace(/'(?:\\.|[^'\\\n])*'/g, " ");
}

/** Which forbidden native surfaces a source file uses. */
function nativeSurfacesIn(src: string): string[] {
  const code = stripCommentsAndStrings(src);
  const hits: string[] = [];
  if (/<\s*select\b/i.test(code)) hits.push("<select>");
  if (/<\s*datalist\b/i.test(code)) hits.push("<datalist>");
  if (/\bwindow\s*\.\s*alert\s*\(/.test(code)) hits.push("window.alert(");
  if (/\bwindow\s*\.\s*prompt\s*\(/.test(code)) hits.push("window.prompt(");
  if (/(^|[^.\w])alert\s*\(/.test(code)) hits.push("alert(");
  if (/(^|[^.\w])prompt\s*\(/.test(code)) hits.push("prompt(");
  const withStrings = stripComments(src);
  if (
    /\btype\s*=\s*\{?\s*["'`](date|time|datetime-local|month|week|color)["'`]/.test(withStrings)
  ) {
    hits.push('<input type="date|time|color|...">');
  }
  return hits;
}

/** Every `.ts` / `.tsx` under `src/` except this file, whose fixtures hold
 *  the forbidden input types as strings. */
function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      sourceFiles(full, out);
    } else if (/\.tsx?$/.test(entry) && entry !== "no-native-select.test.ts") {
      out.push(full);
    }
  }
  return out;
}

const FILES = sourceFiles(SRC_ROOT);

describe("no browser-native picker or dialog in apps/web/src", () => {
  it("scans a non-trivial number of files", () => {
    expect(FILES.length).toBeGreaterThan(50);
    expect(FILES.some((f) => f.endsWith("feed/FeedEditor.tsx"))).toBe(true);
  });

  it("has no native select, datalist, alert, prompt or system input picker", () => {
    const offenders = FILES.flatMap((f) => {
      const hits = nativeSurfacesIn(readFileSync(f, "utf8"));
      return hits.length === 0 ? [] : [`${f.slice(SRC_ROOT.length + 1)}: ${hits.join(" ")}`];
    });
    expect(offenders).toEqual([]);
  });

  it("flags the forbidden forms and not prose or look-alikes", () => {
    const bad = [
      "<select value={x()}>",
      "  < select\n onChange={f}>",
      '<datalist id="words">',
      "window.alert(msg);",
      "if (window.prompt('Name?') === null) return;",
      "alert(`done`);",
      'const p = prompt("x");',
      '<input type="date" />',
      "<input type={'color'} />",
    ];
    for (const line of bad) {
      expect(nativeSurfacesIn(line), `expected to flag: ${line}`).not.toEqual([]);
    }
    const good = [
      "<LakkoPicker<SuggestionsKey> value={v()} />",
      "const selected = rows.filter((r) => r.selected);",
      "aria-selected={x}",
      "user-select: none",
      "deps.alertRollbackRefused();",
      "props.prompt(); showPrompt(); onPrompt()",
      "// never a <select>, never window.alert(msg)",
      "/** the browser's `<datalist>` */",
      "const doc = '<select> used to run here';",
      '<input type="text" />',
      '<input type="checkbox" />',
    ];
    for (const line of good) {
      expect(nativeSurfacesIn(line), `expected to allow: ${line}`).toEqual([]);
    }
  });
});
