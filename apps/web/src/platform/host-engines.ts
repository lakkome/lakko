// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0082 note of 2026-11-18 — the operating system's on-device engines in
 * the desktop app, reached through the host.
 *
 * The desktop app's webview has no `Translator`, no `LanguageDetector` and no
 * local `SpeechRecognition`. On macOS 26.4 and later the host runs Apple's
 * own instead, in a helper next to it (`apps/desktop/src-tauri/src/
 * engines.rs`, `apps/desktop/macos-engines/main.swift`):
 * `TranslationSession(installedSource:...)` with the `lowLatency` model,
 * `SpeechAnalyzer` + `SpeechTranscriber`, `NLLanguageRecognizer`, the same
 * engines the iOS client uses. The host tells the page which it has through
 * a read-only global set before any page script runs; Windows and Linux have
 * none yet, and neither does a macOS too old for them.
 *
 * What leaves this page: the text of a message to detect or translate, and
 * the audio of a voice or video message as 16 kHz mono PCM, both to the
 * host, which is this app's own process; nothing goes to a server. A
 * translation pack is downloaded by macOS in System Settings, never by Lakko
 * (`packFetch` answers `system-settings`).
 *
 * This module lives with the other host routes rather than under
 * `translation/`, whose code may reach no network at all
 * (`translation/fail-closed.test.ts`): every request here goes to
 * {@link HOST_ENGINES_PATH} on the page's own app scheme, which the host
 * answers itself and never forwards (`proxy.rs`).
 */

import { isDesktop } from "./index";
import type {
  Detection,
  DetectionEngine,
  EngineState,
  Engines,
  SystemPacks,
  TranscriptionEngine,
  TranscriptOutcome,
  TranslationEngine,
} from "../translation/engines";
import type { HypothesisRanker } from "../translation/browser-engines";

/** Path prefix of the host's engine routes. */
export const HOST_ENGINES_PATH = "/__host/engines";

/** The global the host sets (`engines.rs`, `GLOBAL`). */
export const SYSTEM_ENGINES_GLOBAL = "__LAKKO_SYSTEM_ENGINES__";

/** What the host's system engines can do at all. */
export interface SystemEngineCapabilities {
  readonly translation: boolean;
  readonly transcription: boolean;
}

/** Read the host's global; anything malformed is no engines. */
export function systemEngineCapabilities(value: unknown): SystemEngineCapabilities {
  const o = (typeof value === "object" && value !== null ? value : {}) as Record<string, unknown>;
  return { translation: o.translation === true, transcription: o.transcription === true };
}

/** The host's capabilities in this page; none outside the desktop app. */
export function hostCapabilities(): SystemEngineCapabilities {
  if (!isDesktop()) return { translation: false, transcription: false };
  return systemEngineCapabilities((globalThis as Record<string, unknown>)[SYSTEM_ENGINES_GLOBAL]);
}

type Fetch = (input: string, init?: RequestInit) => Promise<Response>;

/** The sample rate the helper takes (`main.swift`). */
export const PCM_SAMPLE_RATE = 16_000;

/** The longest recording sent: 16 minutes, the helper's bound. */
export const MAX_PCM_SECONDS = 16 * 60;

/** A host answer: `{ ok: true, ... }` or `{ ok: false, error }`. */
interface Answer {
  readonly ok?: unknown;
  readonly error?: unknown;
  readonly [field: string]: unknown;
}

class HostEngineError extends Error {
  constructor(code: string) {
    super(code);
    this.name = "HostEngineError";
  }
}

async function post(fetchImpl: Fetch, op: string, body: unknown, signal?: AbortSignal): Promise<Answer> {
  const res = await fetchImpl(`${HOST_ENGINES_PATH}/${op}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    ...(signal !== undefined ? { signal } : {}),
  });
  if (!res.ok) throw new HostEngineError(`http-${res.status}`);
  return (await res.json()) as Answer;
}

function stateOf(answer: Answer): EngineState {
  if (answer.ok !== true) return "unavailable";
  switch (answer.state) {
    case "ready":
      return "ready";
    case "needs-pack":
      return "needs-pack";
    default:
      return "unavailable";
  }
}

function textOf(answer: Answer): string {
  if (answer.ok !== true || typeof answer.text !== "string") {
    throw new HostEngineError(typeof answer.error === "string" ? answer.error : "failed");
  }
  return answer.text;
}

function absentTranslation(): TranslationEngine {
  return {
    present: false,
    state: () => Promise.resolve("unavailable"),
    translate: () => Promise.reject(new Error("no on-device translator")),
  };
}

/** Apple's `TranslationSession` through the host. */
export function hostTranslation(present: boolean, fetchImpl: Fetch = fetch): TranslationEngine {
  if (!present) return absentTranslation();
  return {
    present: true,
    async state(source, target): Promise<EngineState> {
      try {
        return stateOf(await post(fetchImpl, "translation-state", { source, target }));
      } catch {
        return "unavailable";
      }
    },
    async translate(source, target, text, options = {}): Promise<string> {
      return textOf(await post(fetchImpl, "translate", { source, target, text }, options.signal));
    },
    // macOS downloads a translation language in System Settings, with its own
    // approval; no API lets a program without a SwiftUI window start it.
    packFetch: () => "system-settings",
    fetchesPacks: false,
    async openPackSettings(): Promise<void> {
      await fetchImpl(`${HOST_ENGINES_PATH}/open-pack-settings`, { method: "POST" });
    },
  };
}

/** `NLLanguageRecognizer` through the host. Its ranked list goes through the
 *  shared close-candidate rule when `rank` is given, as the browser's does. */
export function hostDetection(
  present: boolean,
  rank: HypothesisRanker | null,
  fetchImpl: Fetch = fetch,
): DetectionEngine {
  if (!present) return { present: false, detect: () => Promise.resolve(null) };
  return {
    present: true,
    async detect(text): Promise<Detection | null> {
      if (text.trim().length === 0) return null;
      try {
        const answer = await post(fetchImpl, "detect", { text });
        if (answer.ok !== true || !Array.isArray(answer.hypotheses)) return null;
        const list: Array<{ tag: string; permille: number }> = [];
        for (const h of answer.hypotheses as unknown[]) {
          const o = (typeof h === "object" && h !== null ? h : {}) as Record<string, unknown>;
          if (typeof o.tag !== "string" || typeof o.permille !== "number") continue;
          list.push({ tag: o.tag, permille: Math.max(0, Math.min(1000, Math.round(o.permille))) });
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

/** Decodes a recording to 16 kHz mono 16-bit little-endian PCM. */
export type PcmDecoder = (audio: Blob) => Promise<Uint8Array<ArrayBuffer>>;

/** Float samples in [-1, 1] as 16-bit little-endian PCM. */
export function int16le(samples: Float32Array): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(samples.length * 2);
  const view = new DataView(out.buffer);
  for (let i = 0; i < samples.length; i++) {
    const s = Math.max(-1, Math.min(1, samples[i] ?? 0));
    view.setInt16(i * 2, Math.round(s < 0 ? s * 0x8000 : s * 0x7fff), true);
  }
  return out;
}

/**
 * The page's decoder: Web Audio decodes the recording (AAC in MP4 for a
 * voice message, the audio track of a video message) and an offline context
 * resamples it to 16 kHz mono, in memory. Nothing is played.
 */
export const webAudioPcm: PcmDecoder = async (audio) => {
  const decoder = new OfflineAudioContext(1, 1, PCM_SAMPLE_RATE);
  const decoded = await decoder.decodeAudioData(await audio.arrayBuffer());
  if (decoded.duration > MAX_PCM_SECONDS) throw new HostEngineError("too-long");
  const frames = Math.max(1, Math.ceil(decoded.duration * PCM_SAMPLE_RATE));
  const mixer = new OfflineAudioContext(1, frames, PCM_SAMPLE_RATE);
  const source = mixer.createBufferSource();
  source.buffer = decoded;
  source.connect(mixer.destination);
  source.start();
  const rendered = await mixer.startRendering();
  return int16le(rendered.getChannelData(0));
};

/** `SpeechAnalyzer` + `SpeechTranscriber` through the host. */
export function hostTranscription(
  present: boolean,
  fetchImpl: Fetch = fetch,
  decode: PcmDecoder = webAudioPcm,
): TranscriptionEngine {
  if (!present) {
    return {
      present: false,
      state: () => Promise.resolve("unavailable"),
      install: () => Promise.resolve(false),
      transcribe: () => Promise.reject(new Error("no on-device recognizer")),
    };
  }
  return {
    present: true,
    async state(language): Promise<EngineState> {
      try {
        return stateOf(await post(fetchImpl, "speech-state", { language }));
      } catch {
        return "unavailable";
      }
    },
    async install(language): Promise<boolean> {
      try {
        return (await post(fetchImpl, "speech-install", { language })).ok === true;
      } catch {
        return false;
      }
    },
    async transcribe(audio, language, options = {}): Promise<TranscriptOutcome> {
      const pcm = await decode(audio);
      if (options.signal?.aborted === true) throw new DOMException("transcription cancelled", "AbortError");
      const res = await fetchImpl(`${HOST_ENGINES_PATH}/transcribe?language=${encodeURIComponent(language)}`, {
        method: "POST",
        headers: { "content-type": "application/octet-stream" },
        body: pcm,
        ...(options.signal !== undefined ? { signal: options.signal } : {}),
      });
      if (!res.ok) throw new HostEngineError(`http-${res.status}`);
      // The helper reads the whole recording, so a transcript it returns is complete.
      return { text: textOf((await res.json()) as Answer), complete: true };
    },
  };
}

/** Language tags in a host answer's `field`; anything malformed is dropped. */
function tagsOf(answer: Answer, field: string): string[] {
  const v = answer[field];
  if (!Array.isArray(v)) return [];
  return v.filter((t): t is string => typeof t === "string" && t.length > 0 && t.length <= 35);
}

/**
 * The packs macOS holds for the host's engines (UX section 6): the languages
 * that translate into `target` with a pack on this Mac, and the speech models
 * installed. Read-only; nothing is downloaded. An engine the host lacks, or
 * any failure, lists nothing.
 */
export async function hostSystemPacks(
  caps: SystemEngineCapabilities,
  target: string,
  fetchImpl: Fetch = fetch,
): Promise<SystemPacks> {
  try {
    const answer = await post(fetchImpl, "installed", { target });
    if (answer.ok !== true) return { translation: [], transcription: [] };
    return {
      translation: caps.translation ? tagsOf(answer, "translation") : [],
      transcription: caps.transcription ? tagsOf(answer, "transcription") : [],
    };
  } catch {
    return { translation: [], transcription: [] };
  }
}

/** The host's engines, or `null` when this page has none. */
export function hostEngines(rank: HypothesisRanker | null, fetchImpl: Fetch = fetch): Engines | null {
  const caps = hostCapabilities();
  if (!caps.translation && !caps.transcription) return null;
  return {
    translation: hostTranslation(caps.translation, fetchImpl),
    detection: hostDetection(caps.translation, rank, fetchImpl),
    transcription: hostTranscription(caps.transcription, fetchImpl),
    systemPacks: (target) => hostSystemPacks(caps, target, fetchImpl),
  };
}
