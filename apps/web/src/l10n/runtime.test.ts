// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0058 web runtime: resolution order, the stored choice, `lang` / `dir`
 * switching and the English fallback — driven through the real wasm core and
 * the committed compiled catalogs.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { createL10n, LANGUAGE_STORAGE_KEY, type L10nStorage, type L10nWasm } from "./runtime";
import { languageRows } from "./language-list";

function catalogPath(name: string): string {
  return fileURLToPath(new URL(`./catalog/${name}.json`, import.meta.url));
}

const SOURCE = readFileSync(catalogPath("source"), "utf8");

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

class MemoryStorage implements L10nStorage {
  readonly map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, v);
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
}

interface Harness {
  readonly storage: MemoryStorage;
  readonly applied: Array<{ language: string; rtl: boolean }>;
  system: string[];
  readonly runtime: ReturnType<typeof createL10n>;
}

function harness(opts: {
  system?: string[];
  stored?: string;
  loadCatalog?: (code: string) => Promise<Uint8Array>;
}): Harness {
  const storage = new MemoryStorage();
  if (opts.stored !== undefined) storage.setItem(LANGUAGE_STORAGE_KEY, opts.stored);
  const applied: Array<{ language: string; rtl: boolean }> = [];
  const h = { storage, applied, system: opts.system ?? [] } as Harness;
  (h as { runtime: ReturnType<typeof createL10n> }).runtime = createL10n({
    loadWasm: () => Promise.resolve(wasm as unknown as L10nWasm),
    sourceJson: SOURCE,
    loadCatalog:
      opts.loadCatalog ??
      ((code) => Promise.resolve(new Uint8Array(readFileSync(catalogPath(code))))),
    storage,
    systemLanguages: () => h.system,
    applyToDocument: (language, rtl) => applied.push({ language, rtl }),
  });
  return h;
}

describe("language resolution", () => {
  it("renders English source text before the core has loaded", () => {
    const h = harness({ system: ["ru-RU"] });
    expect(h.runtime.language()).toBe("en");
    expect(h.runtime.t("nav.chats")).toBe("Chats");
  });

  it("chooses an English plural and fills arguments before the core has loaded", () => {
    const source = JSON.stringify({
      format: 1,
      version: 1,
      messages: {
        "x.tries": {
          text: "{name}: {n, plural, =0 {none left} one {# try left} other {# tries left}}.",
          comment: "test",
        },
      },
    });
    const rt = createL10n({
      loadWasm: () => new Promise<L10nWasm>(() => undefined),
      sourceJson: source,
      loadCatalog: () => Promise.reject(new Error("unused")),
      storage: null,
      systemLanguages: () => [],
    });
    expect(rt.t("x.tries", { name: "Ann", n: 0 })).toBe("Ann: none left.");
    expect(rt.t("x.tries", { name: "Ann", n: 1 })).toBe("Ann: 1 try left.");
    expect(rt.t("x.tries", { name: "Ann", n: 7 })).toBe("Ann: 7 tries left.");
  });

  it("follows the first supported system language when nothing is stored", async () => {
    const h = harness({ system: ["de-DE", "fa-IR", "ru-RU"] });
    await h.runtime.init();
    expect(h.runtime.language()).toBe("fa");
    expect(h.runtime.rtl()).toBe(true);
    expect(h.runtime.selected()).toBeNull();
    expect(h.runtime.systemLanguage()).toBe("fa");
    expect(h.applied.at(-1)).toEqual({ language: "fa", rtl: true });
  });

  it("falls back to English when no system language is supported", async () => {
    const h = harness({ system: ["de-DE", "ja-JP"] });
    await h.runtime.init();
    expect(h.runtime.language()).toBe("en");
    expect(h.runtime.rtl()).toBe(false);
    expect(h.applied.at(-1)).toEqual({ language: "en", rtl: false });
  });

  it("a stored choice beats the system language", async () => {
    const h = harness({ system: ["fa-IR"], stored: "ru" });
    await h.runtime.init();
    expect(h.runtime.language()).toBe("ru");
    expect(h.runtime.selected()).toBe("ru");
    expect(h.runtime.systemLanguage()).toBe("fa");
    expect(h.runtime.t("nav.chats")).not.toBe("Chats");
  });

  it("an unknown stored choice reads as Same as system", async () => {
    const h = harness({ system: ["uk-UA"], stored: "xx" });
    await h.runtime.init();
    expect(h.runtime.language()).toBe("uk");
  });
});

describe("choosing a language", () => {
  it("persists the choice, switches lang and dir, and returns to the system", async () => {
    const h = harness({ system: ["es-ES"] });
    await h.runtime.init();
    expect(h.runtime.language()).toBe("es");

    await h.runtime.select("he");
    expect(h.storage.getItem(LANGUAGE_STORAGE_KEY)).toBe("he");
    expect(h.runtime.selected()).toBe("he");
    expect(h.runtime.language()).toBe("he");
    expect(h.runtime.rtl()).toBe(true);
    expect(h.applied.at(-1)).toEqual({ language: "he", rtl: true });

    await h.runtime.select("en");
    expect(h.storage.getItem(LANGUAGE_STORAGE_KEY)).toBe("en");
    expect(h.runtime.t("nav.chats")).toBe("Chats");
    expect(h.applied.at(-1)).toEqual({ language: "en", rtl: false });

    await h.runtime.select(null);
    expect(h.storage.getItem(LANGUAGE_STORAGE_KEY)).toBeNull();
    expect(h.runtime.selected()).toBeNull();
    expect(h.runtime.language()).toBe("es");
  });

  it("a failed catalog load changes nothing", async () => {
    const h = harness({
      system: ["en-US"],
      loadCatalog: () => Promise.reject(new Error("offline")),
    });
    await h.runtime.init();
    await expect(h.runtime.select("ar")).rejects.toThrow("offline");
    expect(h.runtime.language()).toBe("en");
    expect(h.runtime.selected()).toBeNull();
    expect(h.storage.getItem(LANGUAGE_STORAGE_KEY)).toBeNull();
    expect(h.runtime.t("nav.chats")).toBe("Chats");
  });

  it("follows a change of the system language while on Same as system", async () => {
    const h = harness({ system: ["ru-RU"] });
    await h.runtime.init();
    expect(h.runtime.language()).toBe("ru");
    h.system = ["tr-TR"];
    await h.runtime.systemLanguagesChanged();
    expect(h.runtime.language()).toBe("tr");
  });
});

describe("t()", () => {
  it("falls back to English for a key the translation lacks", async () => {
    const ar = JSON.parse(readFileSync(catalogPath("ar"), "utf8")) as {
      messages: Record<string, string>;
    };
    const translatedTitle = ar.messages["settings.language.title"];
    delete ar.messages["nav.chats"];
    const h = harness({
      system: ["ar"],
      loadCatalog: () => Promise.resolve(new TextEncoder().encode(JSON.stringify(ar))),
    });
    await h.runtime.init();
    expect(h.runtime.language()).toBe("ar");
    expect(h.runtime.t("nav.chats")).toBe("Chats");
    expect(h.runtime.t("settings.language.title")).toBe(translatedTitle);
  });

  it("renders an unknown key as the key", async () => {
    const h = harness({});
    await h.runtime.init();
    expect(h.runtime.t("no.such.key")).toBe("no.such.key");
  });

  it("inserts arguments", async () => {
    const h = harness({});
    await h.runtime.init();
    const out = h.runtime.t("settings.language.same_as_system_detail", { language: "Deutsch" });
    expect(out).toContain("Same as system");
    expect(out).toContain("Deutsch");
  });
});

describe("the language list", () => {
  it("lists every language, sorted by the translated name, and searches both names", async () => {
    const h = harness({});
    await h.runtime.init();
    const all = languageRows(h.runtime.languages(), h.runtime.t, "", "en");
    expect(all.map((r) => r.code)).toEqual([
      "ar",
      "zh-Hans",
      "en",
      "he",
      "hi",
      "fa",
      "pt-BR",
      "ru",
      "es",
      "tr",
      "uk",
    ]);
    expect(languageRows(h.runtime.languages(), h.runtime.t, "PORTUGUES", "en").map((r) => r.code)).toEqual([
      "pt-BR",
    ]);
    expect(languageRows(h.runtime.languages(), h.runtime.t, "русск", "en").map((r) => r.code)).toEqual([
      "ru",
    ]);
    expect(languageRows(h.runtime.languages(), h.runtime.t, "klingon", "en")).toEqual([]);
  });
});
