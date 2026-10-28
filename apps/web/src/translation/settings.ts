// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX section 6 — Settings > Language > Translation and transcription.
 *
 * Per device and never sent anywhere, like the interface language
 * (`docs/ux/localization.md` section 4): one `localStorage` entry, not keyed
 * by account, holding the switches, the extra reading languages, the target
 * language and the packs this client asked the browser to download.
 *
 * The packs list is the only record of which packs exist: the browser offers
 * no way to list or remove them, so the settings screen shows the ones Lakko
 * caused to be downloaded, each `Managed by your browser`, without `Remove`.
 */

import { createSignal } from "solid-js";

export const TRANSLATION_SETTINGS_KEY = "lakko.translation.settings.v1";

export type PackUse = "translation" | "transcription";

export interface PackRecord {
  readonly language: string;
  readonly uses: readonly PackUse[];
}

export interface TranslationSettings {
  readonly showTranslate: boolean;
  readonly showTranscript: boolean;
  /** Reading languages beyond the interface language. */
  readonly extraReading: readonly string[];
  /** The target; `null` = the interface language. */
  readonly target: string | null;
  readonly mobileData: boolean;
  readonly packs: readonly PackRecord[];
  /** Set for good when the microphone guard fired (`capture-watch.ts`):
   *  transcription is then unavailable on this device. */
  readonly transcriptionBlocked: boolean;
}

export const DEFAULT_SETTINGS: TranslationSettings = {
  showTranslate: true,
  showTranscript: true,
  extraReading: [],
  target: null,
  mobileData: false,
  packs: [],
  transcriptionBlocked: false,
};

function strings(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string" && x.length > 0) : [];
}

/** Read settings from their stored JSON; anything unreadable is the default. */
export function decodeSettings(raw: string | null): TranslationSettings {
  if (raw === null) return DEFAULT_SETTINGS;
  try {
    const o = JSON.parse(raw) as Record<string, unknown>;
    const packs: PackRecord[] = Array.isArray(o.packs)
      ? o.packs.flatMap((p) => {
          if (typeof p !== "object" || p === null) return [];
          const r = p as Record<string, unknown>;
          if (typeof r.language !== "string") return [];
          const uses = strings(r.uses).filter(
            (u): u is PackUse => u === "translation" || u === "transcription",
          );
          return uses.length === 0 ? [] : [{ language: r.language, uses }];
        })
      : [];
    return {
      showTranslate: o.showTranslate !== false,
      showTranscript: o.showTranscript !== false,
      extraReading: strings(o.extraReading),
      target: typeof o.target === "string" && o.target.length > 0 ? o.target : null,
      mobileData: o.mobileData === true,
      packs,
      transcriptionBlocked: o.transcriptionBlocked === true,
    };
  } catch {
    return DEFAULT_SETTINGS;
  }
}

/** The settings with a pack recorded for `use`. */
export function withPack(s: TranslationSettings, language: string, use: PackUse): TranslationSettings {
  const existing = s.packs.find((p) => p.language === language);
  if (existing !== undefined && existing.uses.includes(use)) return s;
  const packs =
    existing === undefined
      ? [...s.packs, { language, uses: [use] }]
      : s.packs.map((p) => (p.language === language ? { language, uses: [...p.uses, use] } : p));
  return { ...s, packs };
}

function storage(): Storage | null {
  try {
    // Node without a storage file exposes a `localStorage` with no methods.
    return typeof localStorage === "undefined" || typeof localStorage.getItem !== "function"
      ? null
      : localStorage;
  } catch {
    return null;
  }
}

const [settings, setSettingsSignal] = createSignal<TranslationSettings>(
  decodeSettings(storage()?.getItem(TRANSLATION_SETTINGS_KEY) ?? null),
);

/** The current settings (a Solid signal). */
export function translationSettings(): TranslationSettings {
  return settings();
}

/** Replace the settings and store them. */
export function updateTranslationSettings(
  change: (s: TranslationSettings) => TranslationSettings,
): void {
  const next = change(settings());
  setSettingsSignal(next);
  try {
    storage()?.setItem(TRANSLATION_SETTINGS_KEY, JSON.stringify(next));
  } catch {
    // A full or blocked storage keeps the choice for this tab only.
  }
}
