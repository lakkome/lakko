// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The app's one localization runtime (ADR-0058), wired to the real wasm
 * core, the compiled catalogs in `./catalog`, local storage and the
 * document. `main.tsx` calls {@link bootstrapL10n} before the first paint;
 * components call {@link t}.
 *
 * Each non-English catalog is its own chunk (`import.meta.glob`, lazy), so
 * only the active language is downloaded and held in memory.
 */
import sourceJson from "./catalog/source.json?raw";
import { loadWasm } from "../wasm";
import { createL10n, type L10nArgs, type L10nWasm } from "./runtime";

export type { L10nArgs, LanguageInfo } from "./runtime";
export { LANGUAGE_STORAGE_KEY } from "./runtime";

// `source.json` is imported eagerly above; the glob covers the per-language
// files, each loaded on demand.
const catalogLoaders = import.meta.glob<string>(["./catalog/*.json", "!./catalog/source.json"], {
  query: "?raw",
  import: "default",
});

async function loadCatalog(code: string): Promise<Uint8Array> {
  const loader = catalogLoaders[`./catalog/${code}.json`];
  if (loader === undefined) throw new Error(`no compiled catalog for ${code}`);
  return new TextEncoder().encode(await loader());
}

function storage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

const runtime = createL10n({
  loadWasm: async () => (await loadWasm()) as unknown as L10nWasm,
  sourceJson,
  loadCatalog,
  storage: storage(),
  systemLanguages: () => (typeof navigator === "undefined" ? [] : navigator.languages ?? []),
  applyToDocument: (language, rtl) => {
    if (typeof document === "undefined") return;
    document.documentElement.lang = language;
    document.documentElement.dir = rtl ? "rtl" : "ltr";
  },
  warn: (message, err) => console.warn(message, err),
});

/** Render a catalog message in the current language. */
export function t(key: string, args?: L10nArgs): string {
  return runtime.t(key, args);
}

export const currentLanguage = runtime.language;
export const isRtl = runtime.rtl;
export const selectedLanguage = runtime.selected;
export const systemLanguage = runtime.systemLanguage;
export const supportedLanguages = runtime.languages;
export const selectLanguage = runtime.select;

let booted = false;

/** Load the chosen language and start following the system list. */
export function bootstrapL10n(): Promise<void> {
  if (!booted && typeof window !== "undefined") {
    booted = true;
    window.addEventListener("languagechange", () => {
      void runtime.systemLanguagesChanged();
    });
  }
  return runtime.init();
}
