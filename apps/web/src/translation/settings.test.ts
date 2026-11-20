// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** UX section 6 — the `Language packs on this device` list. */

import { describe, expect, it } from "vitest";

import { packList } from "./settings";

const lang = (tag: string): string => tag.split("-")[0] ?? tag;
const same = (a: string, b: string): boolean => lang(a) === lang(b);
const names: Record<string, string> = { en: "English", es: "Spanish", de: "German" };
const name = (tag: string): string => names[tag] ?? tag;

describe("packList", () => {
  it("lists the system's packs, one row per language, by name", () => {
    expect(
      packList([], { translation: ["es"], transcription: ["en-US", "en-GB", "es-ES"] }, same, lang, name),
    ).toEqual([
      { language: "en", uses: ["transcription"] },
      { language: "es", uses: ["translation", "transcription"] },
    ]);
  });

  it("keeps a pack Lakko asked for and adds what the system holds to it", () => {
    expect(
      packList([{ language: "de", uses: ["transcription"] }], { translation: ["de"], transcription: [] }, same, lang, name),
    ).toEqual([{ language: "de", uses: ["transcription", "translation"] }]);
  });

  it("is the recorded list where the system lists nothing", () => {
    expect(packList([{ language: "es", uses: ["translation"] }], null, same, lang, name)).toEqual([
      { language: "es", uses: ["translation"] },
    ]);
    expect(packList([], null, same, lang, name)).toEqual([]);
  });
});
