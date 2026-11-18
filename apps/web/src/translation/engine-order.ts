// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The engine order of ADR-0082 (note of 2026-11-18): the system's on-device
 * engine where it has the language, otherwise the next one.
 *
 * Per language (per pair for translation), the first engine that does not
 * answer `unavailable` owns it, and every later call for that language goes
 * to the owner: its state, its pack, its translation or transcript. Today the
 * list is the system engine of the desktop app and the browser's own; Lakko's
 * own on-device engine (unbuilt) is meant to join at the end of the list, and
 * no screen or controller logic changes when it does.
 *
 * Every engine in the list is an on-device one (`engines.ts`); this module
 * only chooses between them and sends nothing anywhere.
 */

import type {
  DetectionEngine,
  EngineState,
  Engines,
  PackFetch,
  TranscriptionEngine,
  TranslationEngine,
} from "./engines";

/** The first engine in `list` that does not answer `unavailable`, with its answer. */
async function firstOwner<E>(
  list: readonly E[],
  ask: (engine: E) => Promise<EngineState>,
): Promise<{ engine: E; state: EngineState } | null> {
  for (const engine of list) {
    let state: EngineState;
    try {
      state = await ask(engine);
    } catch {
      state = "unavailable";
    }
    if (state !== "unavailable") return { engine, state };
  }
  return null;
}

/** Translation over `list`, in order. */
export function translationInOrder(list: readonly TranslationEngine[]): TranslationEngine {
  const present = list.filter((e) => e.present);
  const owners = new Map<string, TranslationEngine>();
  const key = (source: string, target: string): string => `${source}>${target}`;
  const ownerOf = async (source: string, target: string): Promise<TranslationEngine | null> => {
    const found = await firstOwner(present, (e) => e.state(source, target));
    if (found === null) {
      owners.delete(key(source, target));
      return null;
    }
    owners.set(key(source, target), found.engine);
    return found.engine;
  };
  const opener = present.find((e) => e.openPackSettings !== undefined);
  return {
    present: present.length > 0,
    fetchesPacks: present.some((e) => e.fetchesPacks !== false),
    async state(source, target): Promise<EngineState> {
      // Asked afresh each time: a pack the system fetched since may move a
      // pair from one engine to another.
      const found = await firstOwner(present, (e) => e.state(source, target));
      if (found === null) {
        owners.delete(key(source, target));
        return "unavailable";
      }
      owners.set(key(source, target), found.engine);
      return found.state;
    },
    async translate(source, target, text, options): Promise<string> {
      const engine = owners.get(key(source, target)) ?? (await ownerOf(source, target));
      if (engine === null) throw new Error("no on-device translator for this pair");
      return engine.translate(source, target, text, options);
    },
    packFetch(source, target): PackFetch {
      return owners.get(key(source, target))?.packFetch?.(source, target) ?? "engine";
    },
    ...(opener?.openPackSettings !== undefined
      ? { openPackSettings: () => opener.openPackSettings?.() ?? Promise.resolve() }
      : {}),
  };
}

/** Transcription over `list`, in order. */
export function transcriptionInOrder(list: readonly TranscriptionEngine[]): TranscriptionEngine {
  const present = list.filter((e) => e.present);
  const owners = new Map<string, TranscriptionEngine>();
  const ownerOf = async (language: string): Promise<TranscriptionEngine | null> => {
    const known = owners.get(language);
    if (known !== undefined) return known;
    const found = await firstOwner(present, (e) => e.state(language));
    if (found !== null) owners.set(language, found.engine);
    return found?.engine ?? null;
  };
  return {
    present: present.length > 0,
    async state(language): Promise<EngineState> {
      const found = await firstOwner(present, (e) => e.state(language));
      if (found === null) {
        owners.delete(language);
        return "unavailable";
      }
      owners.set(language, found.engine);
      return found.state;
    },
    async install(language): Promise<boolean> {
      const engine = await ownerOf(language);
      return engine === null ? false : engine.install(language);
    },
    async transcribe(audio, language, options) {
      const engine = await ownerOf(language);
      if (engine === null) throw new Error("no on-device recognizer for this language");
      return engine.transcribe(audio, language, options);
    },
  };
}

/** Detection: the first engine with an answer. */
export function detectionInOrder(list: readonly DetectionEngine[]): DetectionEngine {
  const present = list.filter((e) => e.present);
  return {
    present: present.length > 0,
    async detect(text) {
      for (const engine of present) {
        const found = await engine.detect(text).catch(() => null);
        if (found !== null) return found;
      }
      return null;
    },
  };
}

/** A whole set of engines over `list`, in order. */
export function enginesInOrder(list: readonly Engines[]): Engines {
  const blockers = list.flatMap((e) => (e.packsBlocked === undefined ? [] : [e.packsBlocked.bind(e)]));
  const listers = list.flatMap((e) => (e.systemPacks === undefined ? [] : [e.systemPacks.bind(e)]));
  return {
    translation: translationInOrder(list.map((e) => e.translation)),
    detection: detectionInOrder(list.map((e) => e.detection)),
    transcription: transcriptionInOrder(list.map((e) => e.transcription)),
    ...(blockers.length > 0
      ? {
          packsBlocked: async () => {
            for (const b of blockers) if (await b().catch(() => true)) return true;
            return false;
          },
        }
      : {}),
    ...(listers.length > 0
      ? {
          systemPacks: async (target: string) => {
            const translation: string[] = [];
            const transcription: string[] = [];
            for (const l of listers) {
              // A system that cannot answer lists nothing; it never stops the screen.
              const p = await l(target).catch(() => ({ translation: [], transcription: [] }));
              translation.push(...p.translation);
              transcription.push(...p.transcription);
            }
            return { translation, transcription };
          },
        }
      : {}),
  };
}
