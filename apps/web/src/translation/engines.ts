// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0082 section 1 — what the web client asks of an on-device engine.
 *
 * Three engines, each behind an interface so the rules and the UI can be
 * tested with fakes and so the browser surface is reached in exactly one
 * module (`browser-engines.ts`). Nothing here sends anything anywhere: every
 * engine runs in the browser, on text or audio this device already decrypted.
 *
 * An engine that cannot promise to stay on the device reports `unavailable`,
 * and the control it would drive is then not offered (UX section 7). There is
 * no second, remote engine behind any of these interfaces.
 */

/** What a device can do for one language, as its engine reports it. */
export type EngineState = "ready" | "needs-pack" | "unavailable";

/** A language a detector found, and how sure it was in thousandths. */
export interface Detection {
  readonly tag: string;
  readonly permille: number;
  /** The other languages the detection was a near tie with, nearest first
   *  (ADR-0082 note of 2026-11-15); absent or empty when confident. */
  readonly alternatives?: readonly string[];
}

/** Progress of a pack download, 0..1. */
export type ProgressListener = (fraction: number) => void;

/**
 * Who fetches a pack a pair needs (ADR-0082 note of 2026-11-18):
 * - `engine`: the engine itself, when `translate()` is called after the
 *   person agreed (the browser `Translator`, Lakko's own engine);
 * - `system-settings`: only the operating system's own settings can, as on
 *   macOS, where a translation language is downloaded in System Settings; the
 *   engine opens them with `openPackSettings()` and translates once the pack
 *   is there.
 */
export type PackFetch = "engine" | "system-settings";

/** On-device translation (the browser `Translator` API on web). */
export interface TranslationEngine {
  /** Whether this browser has an on-device translator at all. */
  readonly present: boolean;
  /** Whether `source` -> `target` works now, after a pack, or not at all. */
  state(source: string, target: string): Promise<EngineState>;
  /**
   * Translate `text`. When the pair needs a pack the call downloads it; call
   * it only after the person agreed to that (UX 3.5), from their gesture.
   */
  translate(
    source: string,
    target: string,
    text: string,
    options?: { readonly onProgress?: ProgressListener; readonly signal?: AbortSignal },
  ): Promise<string>;
  /** Who fetches the pack of a `needs-pack` pair; absent means `engine`. */
  packFetch?(source: string, target: string): PackFetch;
  /**
   * Whether this engine ever downloads a pack itself (absent means it does).
   * `false` where every pack comes from the system's own settings (macOS):
   * then nothing Lakko does touches the connection, and the mobile-data
   * setting has nothing to govern (UX section 6).
   */
  readonly fetchesPacks?: boolean;
  /** Open the system's settings where its packs are downloaded. */
  openPackSettings?(): Promise<void>;
}

/** On-device language detection (the browser `LanguageDetector` API). */
export interface DetectionEngine {
  /** Whether a detector is usable now, without a download. */
  readonly present: boolean;
  /** The most likely language, with its close candidates, or `null` when
   *  there is none or no detector. */
  detect(text: string): Promise<Detection | null>;
}

/** The outcome of transcribing one recording. */
export interface TranscriptOutcome {
  /** The words heard; empty when nothing was heard. */
  readonly text: string;
  /** Whether the engine finished the whole recording. */
  readonly complete: boolean;
}

/** On-device speech recognition over a recorded message. */
export interface TranscriptionEngine {
  /** Whether this browser can recognise a recording locally at all. */
  readonly present: boolean;
  state(language: string): Promise<EngineState>;
  /** Download the pack for `language`; `true` when it is on the device. */
  install(language: string): Promise<boolean>;
  transcribe(
    audio: Blob,
    language: string,
    options?: { readonly onPartial?: (text: string) => void; readonly signal?: AbortSignal },
  ): Promise<TranscriptOutcome>;
}

/**
 * The languages whose packs are on this device and managed by its operating
 * system, as the system reports them (UX section 6: listed with `Managed by
 * your system` and no Remove). Tags are the system's own; the caller merges
 * them with the shared language rules.
 */
export interface SystemPacks {
  /** Languages that translate into the target with a pack already here. */
  readonly translation: readonly string[];
  /** Languages whose speech model is here. */
  readonly transcription: readonly string[];
}

/** The three engines a client runs with. */
export interface Engines {
  readonly translation: TranslationEngine;
  readonly detection: DetectionEngine;
  readonly transcription: TranscriptionEngine;
  /**
   * The packs the system holds for these engines, where it can list them
   * (the desktop app on macOS). Absent where it cannot: a browser lists no
   * pack, and only the ones Lakko asked for are recorded (`settings.ts`).
   */
  systemPacks?(target: string): Promise<SystemPacks>;
}

/**
 * Map a browser `availability()` / `available()` answer to an engine state.
 *
 * The four values the Translator, LanguageDetector and SpeechRecognition
 * APIs share. Anything else - a value a later browser adds, a non-string - is
 * `unavailable`: an answer this code does not understand is not a promise to
 * stay on the device.
 */
export function engineStateOf(answer: unknown): EngineState {
  switch (answer) {
    case "available":
      return "ready";
    case "downloadable":
    case "downloading":
      return "needs-pack";
    default:
      return "unavailable";
  }
}

/** A detector's 0..1 confidence in thousandths, clamped. */
export function permilleOf(confidence: unknown): number {
  if (typeof confidence !== "number" || !Number.isFinite(confidence)) return 0;
  return Math.max(0, Math.min(1000, Math.round(confidence * 1000)));
}

/** Engines that can do nothing: the web client's answer when a browser has none. */
export const NO_ENGINES: Engines = {
  translation: {
    present: false,
    state: () => Promise.resolve("unavailable"),
    translate: () => Promise.reject(new Error("no on-device translator")),
  },
  detection: { present: false, detect: () => Promise.resolve(null) },
  transcription: {
    present: false,
    state: () => Promise.resolve("unavailable"),
    install: () => Promise.resolve(false),
    transcribe: () => Promise.reject(new Error("no on-device recognizer")),
  },
};
