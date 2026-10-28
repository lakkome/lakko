// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Every string the translation UI asks the catalog for exists in the shared
 * source catalog (`l10n/catalog/en.json`), with the arguments it is given.
 * Read from the source catalog, not the compiled one, so a key the
 * coordinator added but has not compiled yet is still checked.
 */

import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const CATALOG = join(HERE, "../../../../l10n/catalog/en.json");

const messages = (JSON.parse(readFileSync(CATALOG, "utf8")) as { messages: Record<string, { text: string }> })
  .messages;

/** `t("key")` and `t("key", { a: …, b: … })` calls, with the argument names. */
function calls(src: string): Array<{ key: string; args: string[] }> {
  const out: Array<{ key: string; args: string[] }> = [];
  for (const m of src.matchAll(/\bt\(\s*"([a-z0-9_.]+)"\s*(?:,\s*\{([^}]*)\})?/g)) {
    const args = (m[2] ?? "")
      .split(",")
      .map((a) => a.split(":")[0]?.trim() ?? "")
      .filter((a) => /^[a-z_]+$/.test(a));
    out.push({ key: m[1] ?? "", args });
  }
  return out;
}

describe("the translation UI's catalog keys", () => {
  const files = readdirSync(HERE).filter((f) => f.endsWith(".tsx") && !f.endsWith(".test.tsx"));

  it("finds the calls", () => {
    const all = files.flatMap((f) => calls(readFileSync(join(HERE, f), "utf8")));
    expect(all.some((c) => c.key === "translate.pack.downloading_unsized")).toBe(true);
  });

  it("are all in the source catalog with the arguments they are given", () => {
    const missing: string[] = [];
    for (const f of files) {
      for (const c of calls(readFileSync(join(HERE, f), "utf8"))) {
        const entry = messages[c.key];
        if (entry === undefined) {
          missing.push(`${f}: ${c.key}`);
          continue;
        }
        for (const a of c.args) {
          if (!entry.text.includes(`{${a}`)) missing.push(`${f}: ${c.key} {${a}}`);
        }
      }
    }
    expect(missing).toEqual([]);
  });
});
