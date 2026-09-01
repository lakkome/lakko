// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §14 — the guard that `window.confirm` does not come back.
 *
 * ## Why this is worth a test rather than a review habit
 *
 * `window.confirm` is the path of least resistance: it is one call, it needs
 * no host, no promise and no markup, and every future destructive control
 * will be tempted by it. The migration is only worth doing once, so the rule
 * is executable.
 *
 * ## Why it is not merely a style rule
 *
 * Two reasons, and only the second is about looks.
 *
 * 1. **It can be switched off.** A browser that has shown several dialogs
 *    offers *"prevent this page from creating additional dialogs"*, and once
 *    the user takes it `window.confirm` returns `false` without rendering
 *    anything. Every call site here read that as *cancel*, so the control
 *    went silently dead — the user presses `Leave group` and nothing happens,
 *    with no dialog, no error and no notice. The failure direction is safe
 *    (nothing destructive fires) but the control is a lie.
 * 2. **It renders in the browser's voice.** §6 and §6.2.4 write irreversibility
 *    copy that has to read as the app stating a fact about the user's data;
 *    a chrome-supplied title above it competes with exactly that, and the
 *    destructive action cannot be styled or named at all — `window.confirm`
 *    renders a browser OK/Cancel pair, which is why §6.1's "the action button
 *    names the person" is unimplementable on it.
 *
 * ## Why the scan strips comments and strings first
 *
 * The first draft of this guard matched raw text and flagged eight files, of
 * which only four were calls. The other four were PROSE — this module's own
 * header, and `remove-member-confirm.test.ts` correctly recording that
 * *"`window.confirm` versus an in-page dialog is §14's business"*. A guard
 * that forces the removal of an accurate sentence about the thing it forbids
 * is a guard that will be deleted. So the scan runs over source with comments
 * and string literals removed, and what is left is code.
 *
 * ## Why the bare-`confirm(` arm needs a local-declaration escape
 *
 * `Signup.tsx` and `Recover.tsx` legitimately hold a signal called `confirm`
 * — the password-confirmation FIELD, nothing to do with a dialog — and read
 * it as `confirm()`. The guard therefore treats a bare `confirm(` as a global
 * dialog call only in a file that does not declare a binding of that name.
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

/** `src/`, resolved from this file rather than from the process cwd. */
const SRC_ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");

/**
 * Remove comments and string / template literals, so what remains is code.
 *
 * Deliberately simple: it feeds a substring search, and the only way it can
 * be wrong in the dangerous direction is by stripping something that was
 * really a call. `scans code and not prose` below runs known-bad source
 * through this exact pipeline, so over-stripping fails the suite rather than
 * silently disarming the guard.
 */
function stripCommentsAndStrings(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, " ")
    .replace(/(^|[^:])\/\/[^\n]*/g, "$1 ")
    .replace(/`(?:\\.|[^`\\])*`/g, " ")
    .replace(/"(?:\\.|[^"\\\n])*"/g, " ")
    .replace(/'(?:\\.|[^'\\\n])*'/g, " ");
}

/** Which forbidden forms a source file actually CALLS. */
function confirmCallsIn(src: string): string[] {
  const code = stripCommentsAndStrings(src);
  const hits: string[] = [];
  if (/\bwindow\s*\.\s*confirm\s*\(/.test(code)) hits.push("window.confirm(");
  // A local binding named `confirm` — the password-confirmation signal and
  // the prop it is threaded through.
  const declaresLocal =
    /\[\s*confirm\s*,/.test(code) || /\bconfirm\s*:\s*\(\)\s*=>/.test(code);
  // Not preceded by `.` or a word character, so `props.confirm(` and
  // `requestConfirm(` are never matches.
  if (!declaresLocal && /(^|[^.\w])confirm\s*\(/.test(code)) {
    hits.push("confirm(");
  }
  return hits;
}

/** Every `.ts` / `.tsx` under `src/`, tests included — the rule holds for
 *  tests too, since a test that CALLS `window.confirm` would be exercising
 *  the widget this replaced. Prose about it is fine; the stripper is what
 *  tells the two apart. */
function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      sourceFiles(full, out);
    } else if (/\.tsx?$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

const FILES = sourceFiles(SRC_ROOT);

describe("window.confirm does not survive in apps/web/src", () => {
  it("scans a non-trivial number of files", () => {
    // Guards that silently scan nothing pass forever. Pin that the walk
    // actually found the tree, and the file that held three of the seven.
    expect(FILES.length).toBeGreaterThan(50);
    expect(FILES.some((f) => f.endsWith("routes/Chat.tsx"))).toBe(true);
  });

  it("has no blocking browser confirmation left in the app", () => {
    const offenders = FILES.flatMap((f) => {
      const hits = confirmCallsIn(readFileSync(f, "utf8"));
      return hits.length === 0
        ? []
        : [`${f.slice(SRC_ROOT.length + 1)}: ${hits.join(" ")}`];
    });
    expect(offenders).toEqual([]);
  });

  it("scans code and not prose", () => {
    // The arm that keeps the guard honest without a deliberate regression in
    // the tree: known-bad source must be flagged and known-good source must
    // not, through the SAME function the file scan uses.
    const bad = [
      "if (!window.confirm('Delete?')) return;",
      "if (!confirm('Delete?')) return;",
      "  confirm(`Remove ${name}?`)",
      "const ok = window . confirm ( msg );",
    ];
    for (const line of bad) {
      expect(confirmCallsIn(line), `expected to flag: ${line}`).not.toEqual([]);
    }

    const good = [
      // The signal, in a file that declares it.
      "const [confirm, setConfirm] = createSignal('');\nif (p() !== confirm()) return;",
      // The prop it is threaded through.
      "readonly confirm: () => string;\nvalue={props.confirm()}",
      // The replacement, whose name must never trip the bare-call regex.
      "const ok = await requestConfirm({ title: 'Delete?' });",
      "await deleteChatConfirmation({ chatLabel });",
      // Prose: the reason the stripper exists.
      "// window.confirm is replaced by an in-page dialog (§14).",
      "/** Was `window.confirm(msg)`; now `requestConfirm`. */",
      "const doc = 'window.confirm(msg) used to run here';",
    ];
    for (const line of good) {
      expect(confirmCallsIn(line), `expected to allow: ${line}`).toEqual([]);
    }
  });
});
