// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { DESKTOP_COPY, keyForPlatform } from "./desktop-copy";

interface SourceMessage {
  readonly text: string;
}

const source = JSON.parse(
  readFileSync(fileURLToPath(new URL("./catalog/source.json", import.meta.url)), "utf8"),
) as { readonly messages: Readonly<Record<string, SourceMessage>> };

const saysBrowser = (key: string): boolean => /browser/i.test(source.messages[key]?.text ?? "");

describe("desktop copy (docs/ux/desktop.md section 2.2)", () => {
  it("maps every catalog key that says browser", () => {
    const unmapped = Object.keys(source.messages).filter(
      (key) => saysBrowser(key) && !Object.hasOwn(DESKTOP_COPY, key),
    );
    expect(unmapped).toEqual([]);
  });

  it("maps only to keys that exist and never say browser", () => {
    for (const [web, desktop] of Object.entries(DESKTOP_COPY)) {
      expect(source.messages[web], web).toBeDefined();
      expect(source.messages[desktop], desktop).toBeDefined();
      expect(saysBrowser(desktop), desktop).toBe(false);
    }
  });

  it("keeps the browser key in the browser", () => {
    expect(keyForPlatform("settings.translation.unavailable.translation_web", false)).toBe(
      "settings.translation.unavailable.translation_web",
    );
  });

  it("swaps the key in the desktop app and leaves other keys alone", () => {
    expect(keyForPlatform("settings.translation.unavailable.translation_web", true)).toBe(
      "settings.translation.unavailable.translation",
    );
    expect(keyForPlatform("common.cancel", true)).toBe("common.cancel");
    expect(keyForPlatform("constructor", true)).toBe("constructor");
  });
});
