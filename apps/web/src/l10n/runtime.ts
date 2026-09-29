// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Interface localization runtime (ADR-0058, `docs/ux/localization.md`).
 *
 * The rules live in `lakko-l10n` (Rust, over wasm): which language to show,
 * which translated string fits its English source, how a plural is chosen.
 * This module only holds the current dictionary as Solid signals and swaps
 * it when the language changes.
 *
 * - The choice is per device, kept in local storage under
 *   {@link LANGUAGE_STORAGE_KEY}, and never sent to a server. No stored
 *   value means "Same as system".
 * - Switching loads the language's compiled catalog first and swaps the
 *   whole dictionary at once, so a screen is never half translated. If the
 *   load fails nothing changes and the caller shows the error.
 * - Until the wasm core has loaded, `t()` answers from the English source
 *   catalog parsed here, so the first paint never shows a raw key.
 *
 * Everything the runtime touches outside itself (the wasm namespace, the
 * catalog loader, storage, the system language list, the document) is
 * injected, so the whole thing runs under vitest in plain node.
 */
import { batch, createSignal, type Accessor } from "solid-js";

/** Local-storage key of the chosen language. Absent = follow the system. */
export const LANGUAGE_STORAGE_KEY = "lakko.language";

/** One supported language, copied out of the wasm `LanguageInfo`. */
export interface LanguageInfo {
  readonly code: string;
  readonly ownName: string;
  readonly englishName: string;
  readonly rtl: boolean;
}

interface WasmLanguageInfo {
  readonly code: string;
  readonly ownName: string;
  readonly englishName: string;
  readonly rtl: boolean;
  free?(): void;
}

/** The source catalog handle (`L10nSource`). */
export interface L10nSourceLike {
  free?(): void;
}

/** One language's catalog handle (`L10nCatalog`). */
export interface L10nCatalogLike {
  readonly language: string;
  readonly rtl: boolean;
  plainStrings(): Record<string, string>;
  format(key: string, texts: object, counts: object): string | undefined;
  free?(): void;
}

/** The slice of the wasm namespace this runtime uses. */
export interface L10nWasm {
  l10nSupportedLanguages(): WasmLanguageInfo[];
  l10nResolveLanguage(selected: string | null | undefined, systemPreferred: string[]): string;
  L10nSource: new (sourceJson: Uint8Array) => L10nSourceLike;
  L10nCatalog: new (
    source: L10nSourceLike,
    language: string,
    builtIn?: Uint8Array | null,
  ) => L10nCatalogLike;
}

/** The storage calls the runtime makes (a subset of `Storage`). */
export type L10nStorage = Pick<Storage, "getItem" | "setItem" | "removeItem">;

export interface L10nDeps {
  /** Resolves the wasm namespace (cached by the caller). */
  loadWasm(): Promise<L10nWasm>;
  /** The compiled `source.json`, as text. */
  readonly sourceJson: string;
  /** Loads the compiled catalog of a non-English language. Rejects when the
   *  file cannot be fetched. */
  loadCatalog(code: string): Promise<Uint8Array>;
  /** Local storage, or `null` where there is none. */
  readonly storage: L10nStorage | null;
  /** `navigator.languages`, read each time it is needed. */
  systemLanguages(): readonly string[];
  /** Sets `lang` and `dir` on the document. */
  applyToDocument?(language: string, rtl: boolean): void;
  /** Logs a failure that does not reach the person. */
  warn?(message: string, err: unknown): void;
}

/** A `t()` argument: text is inserted as is, a number selects plural forms
 *  and is written with the language's number format. */
export type L10nArgs = Readonly<Record<string, string | number>>;

export interface L10nRuntime {
  /** The language the interface is rendered in. */
  readonly language: Accessor<string>;
  /** Whether that language is written right to left. */
  readonly rtl: Accessor<boolean>;
  /** The stored choice; `null` = "Same as system". */
  readonly selected: Accessor<string | null>;
  /** The language "Same as system" resolves to right now. */
  readonly systemLanguage: Accessor<string>;
  /** Every supported language (empty until the core has loaded). */
  readonly languages: Accessor<readonly LanguageInfo[]>;
  /** Render a message. Unknown keys render as the key itself. */
  t(key: string, args?: L10nArgs): string;
  /** Resolve the stored choice and load it. Never rejects: a failure keeps
   *  English and is logged. */
  init(): Promise<void>;
  /** Choose a language (`null` = follow the system). Rejects, changing
   *  nothing, when the language file cannot be loaded. */
  select(code: string | null): Promise<void>;
  /** Re-resolve after the system language list changed. */
  systemLanguagesChanged(): Promise<void>;
}

interface SourceFile {
  readonly messages: Record<string, { readonly text: string }>;
}

function parseEnglish(sourceJson: string): Record<string, string> {
  const out: Record<string, string> = {};
  try {
    const parsed = JSON.parse(sourceJson) as SourceFile;
    for (const [key, message] of Object.entries(parsed.messages)) {
      out[key] = message.text;
    }
  } catch {
    // A broken source file is a build defect; `t()` then renders keys.
  }
  return out;
}

/** Index of the brace closing the one opened at `open`, or -1. */
function matchingBrace(s: string, open: number): number {
  let depth = 0;
  for (let i = open; i < s.length; i += 1) {
    if (s[i] === "{") depth += 1;
    else if (s[i] === "}") {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return -1;
}

/** English plural selection for `{n, plural, =N {..} one {..} other {..}}`:
 *  the body after `plural,`, the count. `#` becomes the count. */
function naivePlural(body: string, count: number): string | null {
  const forms = new Map<string, string>();
  let i = 0;
  while (i < body.length) {
    const open = body.indexOf("{", i);
    if (open < 0) break;
    const selector = body.slice(i, open).trim();
    const close = matchingBrace(body, open);
    if (close < 0) return null;
    forms.set(selector, body.slice(open + 1, close));
    i = close + 1;
  }
  const chosen =
    forms.get(`=${count}`) ?? (count === 1 ? forms.get("one") : undefined) ?? forms.get("other");
  return chosen === undefined ? null : chosen.replace(/#/g, String(count));
}

/** Last-resort rendering of an English template: `{name}` replaced and an
 *  English plural chosen. Used only before the core has loaded. */
function naiveFormat(template: string, args: L10nArgs): string {
  let out = "";
  let i = 0;
  while (i < template.length) {
    const open = template.indexOf("{", i);
    if (open < 0) break;
    const close = matchingBrace(template, open);
    if (close < 0) break;
    out += template.slice(i, open);
    const inner = template.slice(open + 1, close);
    const plural = /^\s*([A-Za-z0-9_]+)\s*,\s*plural\s*,([\s\S]*)$/.exec(inner);
    const whole = template.slice(open, close + 1);
    if (plural !== null) {
      const v = args[plural[1] ?? ""];
      const rendered = typeof v === "number" ? naivePlural(plural[2] ?? "", v) : null;
      out += rendered === null ? whole : naiveFormat(rendered, args);
    } else {
      const v = args[inner.trim()];
      out += v === undefined ? whole : String(v);
    }
    i = close + 1;
  }
  return out + template.slice(i);
}

function readStored(storage: L10nStorage | null): string | null {
  if (storage === null) return null;
  try {
    const v = storage.getItem(LANGUAGE_STORAGE_KEY);
    return v === null || v.length === 0 ? null : v;
  } catch {
    return null;
  }
}

function writeStored(storage: L10nStorage | null, code: string | null): void {
  if (storage === null) return;
  try {
    if (code === null) storage.removeItem(LANGUAGE_STORAGE_KEY);
    else storage.setItem(LANGUAGE_STORAGE_KEY, code);
  } catch {
    // Private mode or a full quota: the choice lasts for this page only.
  }
}

/** Build a runtime over the given dependencies. */
export function createL10n(deps: L10nDeps): L10nRuntime {
  const english = parseEnglish(deps.sourceJson);
  const [strings, setStrings] = createSignal<Record<string, string>>(english);
  const [catalog, setCatalog] = createSignal<L10nCatalogLike | null>(null);
  const [language, setLanguage] = createSignal("en");
  const [rtl, setRtl] = createSignal(false);
  const [selected, setSelected] = createSignal<string | null>(readStored(deps.storage));
  const [systemLanguage, setSystemLanguage] = createSignal("en");
  const [languages, setLanguages] = createSignal<readonly LanguageInfo[]>([]);

  let wasm: L10nWasm | null = null;
  let source: L10nSourceLike | null = null;
  // Each switch takes a ticket; only the newest one may apply its result, so
  // two quick taps never end on the older language.
  let ticket = 0;

  async function core(): Promise<{ wasm: L10nWasm; source: L10nSourceLike }> {
    if (wasm === null || source === null) {
      const w = await deps.loadWasm();
      const s = new w.L10nSource(new TextEncoder().encode(deps.sourceJson));
      wasm = w;
      source = s;
      setLanguages(
        w.l10nSupportedLanguages().map((l) => {
          const info: LanguageInfo = {
            code: l.code,
            ownName: l.ownName,
            englishName: l.englishName,
            rtl: l.rtl,
          };
          l.free?.();
          return info;
        }),
      );
    }
    return { wasm, source };
  }

  function resolve(w: L10nWasm, choice: string | null): string {
    return w.l10nResolveLanguage(choice, [...deps.systemLanguages()]);
  }

  async function switchTo(code: string): Promise<void> {
    const mine = ++ticket;
    const { wasm: w, source: s } = await core();
    const builtIn = code === "en" ? null : await deps.loadCatalog(code);
    if (mine !== ticket) return;
    const next = new w.L10nCatalog(s, code, builtIn);
    const plain = next.plainStrings();
    const previous = catalog();
    batch(() => {
      setCatalog(next);
      setStrings(plain);
      setLanguage(next.language);
      setRtl(next.rtl);
    });
    previous?.free?.();
    deps.applyToDocument?.(next.language, next.rtl);
  }

  function t(key: string, args?: L10nArgs): string {
    if (args === undefined || Object.keys(args).length === 0) {
      return strings()[key] ?? english[key] ?? key;
    }
    const cat = catalog();
    if (cat !== null) {
      const texts: Record<string, string> = {};
      const counts: Record<string, number> = {};
      const numberFormat = new Intl.NumberFormat(language());
      for (const [name, value] of Object.entries(args)) {
        if (typeof value === "number") {
          counts[name] = value;
          texts[name] = numberFormat.format(value);
        } else {
          texts[name] = value;
        }
      }
      const out = cat.format(key, texts, counts);
      if (out !== undefined) return out;
    }
    const template = english[key];
    return template === undefined ? key : naiveFormat(template, args);
  }

  async function init(): Promise<void> {
    try {
      const { wasm: w } = await core();
      setSystemLanguage(resolve(w, null));
      await switchTo(resolve(w, selected()));
    } catch (err) {
      deps.warn?.("[l10n] could not load the interface language; staying in English", err);
      deps.applyToDocument?.(language(), rtl());
    }
  }

  async function select(code: string | null): Promise<void> {
    const { wasm: w } = await core();
    await switchTo(resolve(w, code));
    writeStored(deps.storage, code);
    setSelected(code);
  }

  async function systemLanguagesChanged(): Promise<void> {
    if (wasm === null) return;
    setSystemLanguage(resolve(wasm, null));
    if (selected() !== null) return;
    try {
      await switchTo(resolve(wasm, null));
    } catch (err) {
      deps.warn?.("[l10n] could not follow the system language", err);
    }
  }

  return { language, rtl, selected, systemLanguage, languages, t, init, select, systemLanguagesChanged };
}
