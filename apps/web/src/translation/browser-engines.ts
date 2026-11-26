// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0082 section 1, web row — the browser's own on-device engines, and the
 * only module in the client that touches them.
 *
 * - **Translation**: the `Translator` API. Chrome documents it as running the
 *   model in the browser (Chrome 138+ desktop, Edge 148; not Firefox, not
 *   Safari, not mobile, not in workers - developer.chrome.com, checked
 *   2026-10-25).
 * - **Detection**: the `LanguageDetector` API, same availability, used only
 *   when its model is already on the device: detection runs on every
 *   incoming message as it is shown, and nothing is downloaded without the
 *   person choosing to (UX 3.5). Otherwise, which in practice is most of the
 *   time, the core detector `detectTextLanguageCandidates` (wasm,
 *   `lakko_ui_rules::language_detect`; ADR-0082 note of 2026-11-15) answers.
 *   Either answer carries its close candidates: the browser's ranked list
 *   goes through the shared rule `languageCandidatesFromHypotheses`.
 *   It runs in this page on text already decrypted here and sends nothing.
 * - **Transcription**: `SpeechRecognition` with `processLocally = true`, fed
 *   the decoded recording through a Web Audio `MediaStreamTrack`
 *   (`start(audioTrack)`). The draft specification defines `processLocally`
 *   as "must be done locally" and has the session refuse (`service-not-
 *   allowed`) rather than fall back when it cannot be. MDN's compatibility
 *   data (2026-10-25): unprefixed `SpeechRecognition`, `processLocally`,
 *   `available()` and `install()` in Chrome 139, `start(audioTrack)` in Chrome
 *   135; Firefox "preview"; Safari none. The prefixed `webkitSpeechRecognition`
 *   is NEVER used: it has no `processLocally`, so it cannot be told to stay on
 *   the device.
 *
 * ## Fail closed (ADR-0082 section 2)
 *
 * A recognizer is created in one place, {@link createLocalRecognizer}, which
 * sets `processLocally = true` and refuses to return one where the browser
 * did not keep the value. `translation/fail-closed.test.ts` scans the source
 * and fails on any other construction.
 *
 * Not established by any of the above, and stated: whether a browser that
 * implements `processLocally` but not the `start(audioTrack)` overload would
 * ignore the track and listen to the microphone instead (WebIDL drops an
 * argument an overload does not take). The gate here requires the static
 * `available()` and the `processLocally` attribute, which per MDN arrived in
 * Chrome 139, four versions after the track overload; Firefox's on-device
 * intent says it accepts a same-origin track. No browser was run for this.
 * What can be seen at run time is acted on (`capture-watch.ts`): a capture
 * this app starts during a transcription, or the recognizer asking for the
 * microphone, ends it with {@link MICROPHONE_GUARD} and the controller turns
 * transcription off on this device for good.
 */

import { loadWasm } from "../wasm";
import {
  engineStateOf,
  permilleOf,
  type Detection,
  type DetectionEngine,
  type EngineState,
  type Engines,
  type TranscriptionEngine,
  type TranscriptOutcome,
  type TranslationEngine,
} from "./engines";
import { onCaptureStart } from "./capture-watch";

/** The error a transcription ends with when the microphone guard fired. */
export const MICROPHONE_GUARD = "MicrophoneGuard";

function guardError(): Error {
  const e = new Error("a microphone capture may have been involved");
  e.name = MICROPHONE_GUARD;
  return e;
}

// ----- the browser shapes this module reads ---------------------------------
//
// Declared here rather than taken from `lib.dom.d.ts`: the TypeScript DOM
// library does not carry these APIs yet, and declaring the minimum keeps a
// typo in a field name a type error.

interface BrowserTranslator {
  translate(text: string): Promise<string>;
  destroy?(): void;
}

interface DownloadMonitor {
  addEventListener(type: "downloadprogress", listener: (e: { loaded?: number }) => void): void;
}

interface TranslatorStatic {
  availability(options: { sourceLanguage: string; targetLanguage: string }): Promise<unknown>;
  create(options: {
    sourceLanguage: string;
    targetLanguage: string;
    monitor?: (m: DownloadMonitor) => void;
    signal?: AbortSignal;
  }): Promise<BrowserTranslator>;
}

interface BrowserDetector {
  detect(text: string): Promise<ReadonlyArray<{ detectedLanguage?: string; confidence?: number }>>;
}

interface DetectorStatic {
  availability(): Promise<unknown>;
  create(): Promise<BrowserDetector>;
}

interface RecognitionResultList {
  readonly length: number;
  [index: number]: { readonly isFinal: boolean; readonly 0?: { readonly transcript: string } };
}

/** The part of a `SpeechRecognition` instance used here. */
export interface LocalRecognizer {
  processLocally: boolean;
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  maxAlternatives: number;
  onresult: ((e: { results: RecognitionResultList }) => void) | null;
  onerror: ((e: { error?: string }) => void) | null;
  onend: (() => void) | null;
  start(track?: MediaStreamTrack): void;
  stop(): void;
  abort(): void;
}

interface RecognitionOptions {
  langs: string[];
  processLocally: boolean;
}

export interface RecognitionStatic {
  new (): LocalRecognizer;
  available?(options: RecognitionOptions): Promise<unknown>;
  install?(options: RecognitionOptions): Promise<unknown>;
  readonly prototype: object;
}

function globalOf<T>(name: string): T | null {
  const value = (globalThis as Record<string, unknown>)[name];
  return value === undefined || value === null ? null : (value as T);
}

/**
 * How long a browser's availability question may take before the engine is
 * taken as unavailable. Seen 2026-11-26: in a Chromium 156 embedding (the
 * Claude app's browser pane) `Translator.availability()` and
 * `LanguageDetector.availability()` never settle, so waiting for them hid
 * Translate for every pair and the next engine in the order (Lakko's own,
 * ADR-0100) was never asked. Chrome answers well inside this; the value is
 * chosen, not measured.
 */
export const BROWSER_ANSWER_MS = 3000;
// An API that did not answer once is not asked again until the page reloads:
// otherwise every question would wait the whole bound again.

/** Rejection of {@link answerWithin} when the browser did not answer. */
class NoAnswer extends Error {}

/** `answer`, or a {@link NoAnswer} once `ms` passed without one. */
function answerWithin<T>(answer: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new NoAnswer("no answer")), ms);
    answer.then(
      (v) => {
        clearTimeout(timer);
        resolve(v);
      },
      (e: unknown) => {
        clearTimeout(timer);
        reject(e instanceof Error ? e : new Error("refused"));
      },
    );
  });
}

// ----- translation ----------------------------------------------------------

/**
 * The tag Chrome's `Translator` takes for a language Lakko names otherwise.
 *
 * Chrome's documented list (developer.chrome.com/docs/ai/translator-api,
 * read 2026-11-15) names Norwegian `no`; the core detector answers `nb`
 * (Norwegian Bokmal, ADR-0082), so `nb` is asked as `no`. The same list has
 * neither `tl` nor `fil`, so Tagalog is passed through unchanged and the
 * browser answers for it whatever it answers. Every other tag is unchanged.
 */
export function translatorTag(tag: string): string {
  const lower = tag.toLowerCase();
  if (lower === "nb" || lower.startsWith("nb-")) return "no";
  return tag;
}

/** The browser `Translator` API, or an engine that offers nothing. */
export function browserTranslation(
  api: TranslatorStatic | null = globalOf("Translator"),
  answerMs: number = BROWSER_ANSWER_MS,
): TranslationEngine {
  if (api === null || typeof api.availability !== "function" || typeof api.create !== "function") {
    return {
      present: false,
      state: () => Promise.resolve("unavailable"),
      translate: () => Promise.reject(new Error("no on-device translator")),
    };
  }
  const translators = new Map<string, Promise<BrowserTranslator>>();
  let silent = false;
  return {
    present: true,
    async state(source, target): Promise<EngineState> {
      if (silent) return "unavailable";
      try {
        return engineStateOf(
          await answerWithin(
            api.availability({ sourceLanguage: translatorTag(source), targetLanguage: translatorTag(target) }),
            answerMs,
          ),
        );
      } catch (e) {
        if (e instanceof NoAnswer) silent = true;
        return "unavailable";
      }
    },
    async translate(sourceTag, targetTag, text, options = {}): Promise<string> {
      const source = translatorTag(sourceTag);
      const target = translatorTag(targetTag);
      const key = `${source}>${target}`;
      let pending = translators.get(key);
      if (pending === undefined) {
        pending = api.create({
          sourceLanguage: source,
          targetLanguage: target,
          ...(options.signal !== undefined ? { signal: options.signal } : {}),
          monitor: (m) => {
            m.addEventListener("downloadprogress", (e) => {
              if (typeof e.loaded === "number") options.onProgress?.(e.loaded);
            });
          },
        });
        translators.set(key, pending);
        // A refused or cancelled create is not cached: the next attempt asks again.
        pending.catch(() => {
          if (translators.get(key) === pending) translators.delete(key);
        });
      }
      const translator = await pending;
      return translator.translate(text);
    },
  };
}

// ----- detection ------------------------------------------------------------

/** Turns a detector's ranked hypotheses into a detection with its close
 *  candidates, through the shared rule (`languageCandidatesFromHypotheses`). */
export type HypothesisRanker = (
  text: string,
  tags: readonly string[],
  permilles: readonly number[],
) => Promise<Detection | null>;

/** The browser `LanguageDetector` API, used only when its model is present.
 *  `rank` turns its ranked list into the close candidates; without it the top
 *  hypothesis alone is answered. */
export function browserDetection(
  api: DetectorStatic | null = globalOf("LanguageDetector"),
  rank: HypothesisRanker | null = null,
  answerMs: number = BROWSER_ANSWER_MS,
): DetectionEngine {
  if (api === null || typeof api.availability !== "function" || typeof api.create !== "function") {
    return { present: false, detect: () => Promise.resolve(null) };
  }
  let detector: Promise<BrowserDetector | null> | null = null;
  let silent = false;
  const open = (): Promise<BrowserDetector | null> => {
    if (silent) return Promise.resolve(null);
    if (detector === null) {
      detector = (async () => {
        try {
          // Only a model already on the device: see the module doc.
          if (engineStateOf(await answerWithin(api.availability(), answerMs)) !== "ready") return null;
          return await api.create();
        } catch (e) {
          if (e instanceof NoAnswer) silent = true;
          return null;
        }
      })();
    }
    return detector;
  };
  return {
    present: true,
    async detect(text): Promise<Detection | null> {
      const d = await open();
      if (d === null) {
        // Asked again next time: a model the browser fetched on its own since
        // (for another site, say) becomes usable without a reload.
        detector = null;
        return null;
      }
      try {
        const list: Array<{ tag: string; permille: number }> = [];
        for (const h of await d.detect(text)) {
          if (typeof h.detectedLanguage !== "string" || h.detectedLanguage === "und") continue;
          list.push({ tag: h.detectedLanguage, permille: permilleOf(h.confidence) });
        }
        const [best] = list;
        if (best === undefined) return null;
        const top: Detection = { tag: best.tag, permille: best.permille };
        if (rank === null) return top;
        const ranked = await rank(
          text,
          list.map((h) => h.tag),
          list.map((h) => h.permille),
        ).catch(() => null);
        return ranked ?? top;
      } catch {
        return null;
      }
    },
  };
}

/** A wasm answer: a tag, a confidence and, for candidates, the alternatives. */
interface WasmDetected {
  readonly tag: string;
  readonly confidencePermille: number;
  readonly alternatives?: string[];
  free(): void;
}

/** The part of the wasm namespace the core detector needs. */
export interface CoreLanguageDetector {
  detectTextLanguage(text: string): WasmDetected | undefined;
  detectTextLanguageCandidates?(text: string): WasmDetected | undefined;
  languageCandidatesFromHypotheses?(text: string, tags: string[], permilles: Uint16Array): WasmDetected | undefined;
}

/** The core detector, which can also rank another detector's hypotheses. */
export interface CoreDetectionEngine extends DetectionEngine {
  readonly rank: HypothesisRanker;
}

/**
 * The core detector over wasm (`lakko_ui_rules::language_detect`). It reads
 * at most the first 2048 characters, offline; the controller asks it once per
 * message version. The answer carries the close candidates
 * (`detectTextLanguageCandidates`). `null` when wasm did not load or the text
 * has no letters.
 */
export function coreDetection(
  load: () => Promise<CoreLanguageDetector | null> = loadWasm,
): CoreDetectionEngine {
  let core: Promise<CoreLanguageDetector | null> | null = null;
  const ask = async (call: (wasm: CoreLanguageDetector) => WasmDetected | undefined): Promise<Detection | null> => {
    core ??= load().catch(() => null);
    const wasm = await core;
    if (wasm === null) {
      core = null;
      return null;
    }
    try {
      const found = call(wasm);
      if (found === undefined) return null;
      try {
        const alternatives = found.alternatives ?? [];
        return {
          tag: found.tag,
          permille: Math.max(0, Math.min(1000, found.confidencePermille)),
          ...(alternatives.length > 0 ? { alternatives } : {}),
        };
      } finally {
        found.free();
      }
    } catch {
      return null;
    }
  };
  return {
    present: true,
    detect: (text) =>
      ask((wasm) =>
        wasm.detectTextLanguageCandidates !== undefined
          ? wasm.detectTextLanguageCandidates(text)
          : wasm.detectTextLanguage(text),
      ),
    rank: (text, tags, permilles) =>
      ask((wasm) =>
        wasm.languageCandidatesFromHypotheses === undefined
          ? undefined
          : wasm.languageCandidatesFromHypotheses(
              text,
              [...tags],
              Uint16Array.from(permilles.map((p) => Math.max(0, Math.min(1000, Math.round(p))))),
            ),
      ),
  };
}

/** The browser's detector when it has an answer, else the core one. */
export function layeredDetection(
  browserEngine?: DetectionEngine,
  core: DetectionEngine & { readonly rank?: HypothesisRanker } = coreDetection(),
): DetectionEngine {
  // The browser's ranked list goes through the same close-candidate rule as
  // the core detector's scores.
  const browser = browserEngine ?? browserDetection(globalOf("LanguageDetector"), core.rank ?? null);
  return {
    present: true,
    async detect(text): Promise<Detection | null> {
      if (browser.present) {
        const d = await browser.detect(text).catch(() => null);
        if (d !== null) return d;
      }
      return core.detect(text);
    },
  };
}

// ----- transcription --------------------------------------------------------

/**
 * Whether the browser can be told to recognise speech on the device only.
 *
 * The prefixed constructor is deliberately not consulted (module doc).
 */
export function localRecognitionSupported(api: RecognitionStatic | null): api is RecognitionStatic {
  return (
    api !== null &&
    typeof api === "function" &&
    typeof api.available === "function" &&
    typeof api.install === "function" &&
    "processLocally" in api.prototype
  );
}

/**
 * The ONE place a recognizer is constructed. It is told to process locally
 * before anything else, and refused when the browser did not keep that.
 */
export function createLocalRecognizer(api: RecognitionStatic, language: string): LocalRecognizer {
  const recognizer = new api();
  recognizer.processLocally = true;
  if (recognizer.processLocally !== true) {
    throw new Error("this browser cannot keep speech recognition on the device");
  }
  recognizer.lang = language;
  recognizer.continuous = true;
  recognizer.interimResults = true;
  recognizer.maxAlternatives = 1;
  return recognizer;
}

/** How long recognition keeps listening after the recording ended, so the
 *  last word is not cut. Chosen, not measured. */
const TAIL_MS = 600;

/** The words in a result list: the final ones, then the interim tail. */
export function transcriptOf(results: RecognitionResultList): { text: string; final: string } {
  let final = "";
  let interim = "";
  for (let i = 0; i < results.length; i++) {
    const r = results[i];
    const words = r?.[0]?.transcript ?? "";
    if (r?.isFinal === true) final += words;
    else interim += words;
  }
  return { text: (final + interim).trim(), final: final.trim() };
}

/** A minimal Web Audio context: what decoding and the track need. */
interface AudioGraph {
  readonly state: string;
  resume(): Promise<void>;
  close(): Promise<void>;
  decodeAudioData(data: ArrayBuffer): Promise<AudioBuffer>;
  createBufferSource(): AudioBufferSourceNode;
  createMediaStreamDestination(): MediaStreamAudioDestinationNode;
}

/** SpeechRecognition with `processLocally`, or an engine that offers nothing. */
export function browserTranscription(
  api: RecognitionStatic | null = globalOf("SpeechRecognition"),
  audioContext: (() => AudioGraph) | null = typeof AudioContext === "undefined"
    ? null
    : () => new AudioContext(),
  captureStarts: (listener: () => void) => () => void = onCaptureStart,
): TranscriptionEngine {
  if (!localRecognitionSupported(api) || audioContext === null) {
    return {
      present: false,
      state: () => Promise.resolve("unavailable"),
      install: () => Promise.resolve(false),
      transcribe: () => Promise.reject(new Error("no on-device recognizer")),
    };
  }
  const options = (language: string): RecognitionOptions => ({ langs: [language], processLocally: true });
  return {
    present: true,
    async state(language): Promise<EngineState> {
      try {
        return engineStateOf(await api.available?.(options(language)));
      } catch {
        return "unavailable";
      }
    },
    async install(language): Promise<boolean> {
      try {
        return (await api.install?.(options(language))) === true;
      } catch {
        return false;
      }
    },
    async transcribe(audio, language, opts = {}): Promise<TranscriptOutcome> {
      const ctx = audioContext();
      try {
        if (ctx.state === "suspended") await ctx.resume();
        const buffer = await ctx.decodeAudioData(await audio.arrayBuffer());
        const source = ctx.createBufferSource();
        source.buffer = buffer;
        // Into a stream and nowhere else: the recording is not played aloud.
        const destination = ctx.createMediaStreamDestination();
        source.connect(destination);
        const [track] = destination.stream.getAudioTracks();
        if (track === undefined) throw new Error("no audio track");
        const recognizer = createLocalRecognizer(api, language);
        let stopWatching = (): void => undefined;
        try {
        return await new Promise<TranscriptOutcome>((resolve, reject) => {
          let heard = "";
          let failed: string | null = null;
          let ended = false;
          let guarded = false;
          // A capture started while this runs: stop at once (module doc of
          // `capture-watch.ts`).
          stopWatching = captureStarts(() => {
            guarded = true;
            source.stop();
            recognizer.abort();
          });
          recognizer.onresult = (e) => {
            const { text, final } = transcriptOf(e.results);
            heard = final.length > 0 ? final : text;
            opts.onPartial?.(text);
          };
          recognizer.onerror = (e) => {
            failed = e.error ?? "unknown";
          };
          recognizer.onend = () => {
            track.stop();
            // A session that reads a supplied track has no reason to ask for
            // the microphone; one that did is treated as having listened to it.
            if (guarded || failed === "not-allowed" || failed === "audio-capture") {
              reject(guardError());
            } else if (failed === null || failed === "no-speech") {
              resolve({ text: heard, complete: ended });
            } else if (failed === "aborted") {
              reject(new DOMException("transcription cancelled", "AbortError"));
            } else {
              reject(new Error(`recognition-${failed}`));
            }
          };
          source.onended = () => {
            ended = true;
            setTimeout(() => recognizer.stop(), TAIL_MS);
          };
          opts.signal?.addEventListener("abort", () => {
            source.stop();
            recognizer.abort();
          });
          recognizer.start(track);
          source.start();
        });
        } finally {
          stopWatching();
        }
      } finally {
        void ctx.close().catch(() => undefined);
      }
    },
  };
}

/** The engines of this browser. */
export function browserEngines(): Engines {
  return {
    translation: browserTranslation(),
    detection: layeredDetection(),
    transcription: browserTranscription(),
  };
}
