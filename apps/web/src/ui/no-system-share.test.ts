// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The guard that the system share sheet does not come back.
 *
 * A Lakko screen never hands the person to the browser's or the operating
 * system's own element when the app can draw its own. `navigator.share`
 * opened the system sheet from the call's Invite button and from three link
 * rows (found live 2026-10-02): a foreign surface, different on every
 * platform, offering a dozen destinations where a link needs two - copy it,
 * or send it to a contact. Each was replaced with a control of the app's own,
 * and this test keeps it that way. `no-window-confirm.test.ts` is the same
 * rule for the browser's confirmation dialog.
 *
 * The scan runs over source with comments and string literals removed, so a
 * sentence about the forbidden call - this header included - is not a hit.
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const SRC_ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");

function stripCommentsAndStrings(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, " ")
    .replace(/(^|[^:])\/\/[^\n]*/g, "$1 ")
    .replace(/`(?:\\.|[^`\\])*`/g, " ")
    .replace(/"(?:\\.|[^"\\\n])*"/g, " ")
    .replace(/'(?:\\.|[^'\\\n])*'/g, " ");
}

/** Whether a source file reaches for the system share sheet. */
function usesSystemShare(src: string): boolean {
  const code = stripCommentsAndStrings(src);
  return /\.\s*share\s*\(/.test(code) || /\.\s*canShare\s*\(/.test(code) || /\bnavigator\s*\.\s*share\b/.test(code);
}

function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) sourceFiles(full, out);
    else if (/\.tsx?$/.test(entry)) out.push(full);
  }
  return out;
}

const FILES = sourceFiles(SRC_ROOT);

describe("the system share sheet does not survive in apps/web/src", () => {
  it("scans a non-trivial number of files", () => {
    expect(FILES.length).toBeGreaterThan(50);
    expect(FILES.some((f) => f.endsWith("calls/CallHostContext.tsx"))).toBe(true);
  });

  it("has no call to it left in the app", () => {
    const offenders = FILES.filter((f) => usesSystemShare(readFileSync(f, "utf8"))).map((f) =>
      f.slice(SRC_ROOT.length + 1),
    );
    expect(offenders).toEqual([]);
  });

  it("scans code and not prose", () => {
    expect(usesSystemShare("await navigator.share({ url });")).toBe(true);
    expect(usesSystemShare("if (typeof nav.share === 'function') await nav . share ( x );")).toBe(true);
    expect(usesSystemShare("const ok = typeof navigator.share === 'function';")).toBe(true);
    expect(usesSystemShare("// navigator.share is gone\nconst x = 1;")).toBe(false);
    expect(usesSystemShare("const label = \"navigator.share()\";")).toBe(false);
  });
});
