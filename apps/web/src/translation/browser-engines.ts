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
 * - **Detection**: the `LanguageDetector` API, same availability. Used only
 *   when its model is already on the device: detection runs on every
 *   incoming message as it is shown, and nothing is downloaded without the
 *   person choosing to (UX 3.5), so a detector that would need a download is
 *   treated as absent and the menu row falls back to the language picker.
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

// ----- translation ----------------------------------------------------------

/** The browser `Translator` API, or an engine that offers nothing. */
export function browserTranslation(api: TranslatorStatic | null = globalOf("Translator")): TranslationEngine {
  if (api === null || typeof api.availability !== "function" || typeof api.create !== "function") {
    return {
      present: false,
      state: () => Promise.resolve("unavailable"),
      translate: () => Promise.reject(new Error("no on-device translator")),
    };
  }
  const translators = new Map<string, Promise<BrowserTranslator>>();
  return {
    present: true,
    async state(source, target): Promise<EngineState> {
      try {
        return engineStateOf(await api.availability({ sourceLanguage: source, targetLanguage: target }));
      } catch {
        return "unavailable";
      }
    },
    async translate(source, target, text, options = {}): Promise<string> {
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

/** The browser `LanguageDetector` API, used only when its model is present. */
export function browserDetection(api: DetectorStatic | null = globalOf("LanguageDetector")): DetectionEngine {
  if (api === null || typeof api.availability !== "function" || typeof api.create !== "function") {
    return { present: false, detect: () => Promise.resolve(null) };
  }
  let detector: Promise<BrowserDetector | null> | null = null;
  const open = (): Promise<BrowserDetector | null> => {
    if (detector === null) {
      detector = (async () => {
        try {
          // Only a model already on the device: see the module doc.
          if (engineStateOf(await api.availability()) !== "ready") return null;
          return await api.create();
        } catch {
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
        const [best] = await d.detect(text);
        if (best === undefined || typeof best.detectedLanguage !== "string") return null;
        if (best.detectedLanguage === "und") return null;
        return { tag: best.detectedLanguage, permille: permilleOf(best.confidence) };
      } catch {
        return null;
      }
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
    detection: browserDetection(),
    transcription: browserTranscription(),
  };
}
