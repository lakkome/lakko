// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The translation controller against fake on-device engines and the REAL
 * shared rules (`lakko-ui-rules::translation` over wasm), so what is asserted
 * here is this client's wiring of the rule, not a copy of it.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { beforeAll, beforeEach, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { TranslationController, type MessageRef } from "./controller";
import type { Detection, EngineState, Engines, TranscriptOutcome } from "./engines";
import { MemoryResultBackend, ResultStore } from "./result-store";
import type { TranslationRules } from "./rules";
import { DEFAULT_SETTINGS, type PackUse, type TranslationSettings } from "./settings";

let rules: TranslationRules;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
  rules = wasm;
});

const flush = async (): Promise<void> => {
  for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
};

interface Fakes {
  engines: Engines;
  translateCalls: Array<{ source: string; target: string; text: string }>;
  progress: ((f: number) => void) | null;
  translationState: Map<string, EngineState>;
  detections: Map<string, Detection | null>;
  transcriptionState: Map<string, EngineState>;
  installed: string[];
  transcribeOutcome: TranscriptOutcome | Error;
  translateResult: string | Error;
}

function fakes(): Fakes {
  const f: Fakes = {
    translateCalls: [],
    progress: null,
    translationState: new Map(),
    detections: new Map(),
    transcriptionState: new Map(),
    installed: [],
    transcribeOutcome: { text: "hola amigos", complete: true },
    translateResult: "hello friends",
    engines: undefined as unknown as Engines,
  };
  f.engines = {
    translation: {
      present: true,
      state: (s, t) => Promise.resolve(f.translationState.get(`${s}>${t}`) ?? "unavailable"),
      translate: (source, target, text, opts) => {
        f.translateCalls.push({ source, target, text });
        f.progress = opts?.onProgress ?? null;
        return f.translateResult instanceof Error
          ? Promise.reject(f.translateResult)
          : Promise.resolve(f.translateResult);
      },
    },
    detection: {
      present: true,
      detect: (text) => Promise.resolve(f.detections.get(text) ?? null),
    },
    transcription: {
      present: true,
      state: (l) => Promise.resolve(f.transcriptionState.get(l) ?? "unavailable"),
      install: (l) => {
        f.installed.push(l);
        return Promise.resolve(true);
      },
      transcribe: () =>
        f.transcribeOutcome instanceof Error
          ? Promise.reject(f.transcribeOutcome)
          : Promise.resolve(f.transcribeOutcome),
    },
  };
  return f;
}

const ACCOUNT = "YWNjb3VudA==";
const CONV = "Z3JvdXA=";

/** `lastOpId` is the DS id of the latest edit op (0 = never edited). */
function ref(messageId: number, lastOpId = 0): MessageRef {
  return { conversation: CONV, messageId, lastOpId };
}

let f: Fakes;
let backend: MemoryResultBackend;
let settings: TranslationSettings;
let packs: Array<[string, PackUse]>;

function controller(): TranslationController {
  return new TranslationController({
    engines: f.engines,
    store: new ResultStore(backend),
    rules: () => rules,
    accountId: () => ACCOUNT,
    interfaceLanguage: () => "en",
    settings: () => settings,
    recordPack: (l, u) => packs.push([l, u]),
    blockTranscription: () => {
      settings = { ...settings, transcriptionBlocked: true };
    },
    setTimer: () => undefined,
  });
}

beforeEach(() => {
  f = fakes();
  backend = new MemoryResultBackend();
  settings = DEFAULT_SETTINGS;
  packs = [];
  f.detections.set("hola amigos", { tag: "es", permille: 950 });
  f.detections.set("hello there", { tag: "en", permille: 950 });
});

describe("the Translate row (UX 3.1, shared rule)", () => {
  it("is offered on someone else's message in a language the device translates", async () => {
    f.translationState.set("es>en", "ready");
    const c = controller();
    c.offer(ref(1), "hola amigos", false);
    await flush();
    expect(c.offer(ref(1), "hola amigos", false)).toBe("translate");
  });

  it("is not offered on own messages, in a reading language, or with the setting off", async () => {
    f.translationState.set("es>en", "ready");
    const c = controller();
    c.offer(ref(1), "hola amigos", false);
    c.offer(ref(2), "hello there", false);
    await flush();
    expect(c.offer(ref(1), "hola amigos", true)).toBe("none");
    expect(c.offer(ref(2), "hello there", false)).toBe("none");
    settings = { ...DEFAULT_SETTINGS, showTranslate: false };
    expect(c.offer(ref(1), "hola amigos", false)).toBe("none");
  });

  it("opens with the picker when the language could not be told", async () => {
    f.translationState.set("de>en", "needs-pack");
    const c = controller();
    c.offer(ref(3), "???", false);
    await flush();
    expect(c.offer(ref(3), "???", false)).toBe("ask");
    c.translate("text", ref(3), "???");
    await flush();
    expect(c.pickerRequest()?.use).toBe("translation");
    expect(f.translateCalls).toEqual([]);
  });

  it("translates from an unsure guess at once, and Not X? corrects it", async () => {
    f.detections.set("goedemorgen", { tag: "nl", permille: 400 });
    f.translationState.set("nl>en", "ready");
    f.translationState.set("af>en", "ready");
    const c = controller();
    c.translate("text", ref(6), "goedemorgen");
    await flush();
    expect(c.pickerRequest()).toBeNull();
    expect(f.translateCalls).toEqual([{ source: "nl", target: "en", text: "goedemorgen" }]);
    expect(c.textView("text", ref(6))).toEqual({ phase: "translated", source: "nl", text: "hello friends" });
    // `Not Dutch?` opens the picker on the guess; a pick translates again.
    c.openSourcePicker("text", ref(6), "goedemorgen");
    expect(c.pickerRequest()?.current).toBe("nl");
    c.pickerRequest()?.choose("af");
    await flush();
    expect(f.translateCalls.at(-1)?.source).toBe("af");
    expect(c.textView("text", ref(6))).toMatchObject({ phase: "translated", source: "af" });
  });

  it("an unsure guess yields to the language chosen for the conversation", async () => {
    f.detections.set("goedemorgen", { tag: "nl", permille: 400 });
    f.detections.set("goedenavond", { tag: "af", permille: 300 });
    f.translationState.set("nl>en", "ready");
    f.translationState.set("af>en", "ready");
    const c = controller();
    c.translate("text", ref(6), "goedemorgen");
    await flush();
    c.openSourcePicker("text", ref(6), "goedemorgen");
    c.pickerRequest()?.choose("nl");
    await flush();
    c.translate("text", ref(7), "goedenavond");
    await flush();
    expect(f.translateCalls.at(-1)).toEqual({ source: "nl", target: "en", text: "goedenavond" });
  });

  it("opens the picker on a guess the device cannot translate from", async () => {
    f.detections.set("sugeng enjing", { tag: "jv", permille: 300 });
    f.translationState.set("de>en", "needs-pack");
    const c = controller();
    c.translate("text", ref(8), "sugeng enjing");
    await flush();
    expect(c.pickerRequest()?.current).toBe("jv");
    expect(f.translateCalls).toEqual([]);
    expect(c.textView("text", ref(8)).phase).toBe("none");
  });

  it("opens the picker on an unsure guess in the language translated into", async () => {
    f.detections.set("ok sure", { tag: "en", permille: 300 });
    const c = controller();
    c.translate("text", ref(9), "ok sure");
    await flush();
    expect(c.pickerRequest()?.current).toBe("en");
    expect(c.textView("text", ref(9)).phase).toBe("none");
  });

  it("offers nothing at all on a browser without a translator", async () => {
    f.engines = { ...f.engines, translation: { ...f.engines.translation, present: false } };
    const c = controller();
    c.offer(ref(1), "hola amigos", false);
    await flush();
    expect(c.offer(ref(1), "hola amigos", false)).toBe("none");
  });
});

describe("translating a message (UX 3.2 / 3.3)", () => {
  it("shows the translation, stores it, and puts the original back", async () => {
    f.translationState.set("es>en", "ready");
    const c = controller();
    c.detected(ref(1), "hola amigos");
    await flush();
    c.translate("text", ref(1), "hola amigos");
    await flush();
    expect(c.textView("text", ref(1))).toEqual({ phase: "translated", source: "es", text: "hello friends" });
    expect(f.translateCalls).toEqual([{ source: "es", target: "en", text: "hola amigos" }]);
    expect(await c.count()).toBe(1);
    c.showOriginal("text", ref(1));
    expect(c.textView("text", ref(1))).toEqual({ phase: "original", source: "es" });
  });

  it("restores a stored translation only while the message is unedited", async () => {
    f.translationState.set("es>en", "ready");
    const first = controller();
    first.detected(ref(1), "hola amigos");
    await flush();
    first.translate("text", ref(1), "hola amigos");
    await flush();

    const again = controller();
    again.textView("text", ref(1));
    await flush();
    expect(again.textView("text", ref(1)).phase).toBe("translated");

    const edited = controller();
    edited.textView("text", ref(1, 57));
    await flush();
    expect(edited.textView("text", ref(1, 57)).phase).toBe("none");
  });

  it("asks before a pack is downloaded, and downloads only on Download", async () => {
    f.translationState.set("es>en", "needs-pack");
    const c = controller();
    c.detected(ref(1), "hola amigos");
    await flush();
    c.translate("text", ref(1), "hola amigos");
    await flush();
    const request = c.packRequest();
    expect(request?.language).toBe("es");
    expect(request?.use).toBe("translation");
    expect(f.translateCalls).toEqual([]);

    request?.download();
    await flush();
    expect(f.translateCalls.length).toBe(1);
    expect(c.textView("text", ref(1)).phase).toBe("translated");
    expect(packs).toEqual([["es", "translation"]]);
  });

  it("downloads nothing when the dialog is dismissed", async () => {
    f.translationState.set("es>en", "needs-pack");
    const c = controller();
    c.detected(ref(1), "hola amigos");
    await flush();
    c.translate("text", ref(1), "hola amigos");
    await flush();
    c.packRequest()?.dismiss();
    await flush();
    expect(f.translateCalls).toEqual([]);
    expect(c.packRequest()).toBeNull();
  });

  it("says so when the engine fails, and keeps the original", async () => {
    f.translationState.set("es>en", "ready");
    f.translateResult = new Error("boom");
    const c = controller();
    c.detected(ref(1), "hola amigos");
    await flush();
    c.translate("text", ref(1), "hola amigos");
    await flush();
    expect(c.textView("text", ref(1))).toEqual({ phase: "failed", source: "es" });
    expect(await c.count()).toBe(0);
  });

  it("says a message is already in the target when the picker chose it", async () => {
    const c = controller();
    c.translate("text", ref(4), "???", "en-GB");
    await flush();
    expect(c.textView("text", ref(4))).toEqual({ phase: "same", language: "en" });
    expect(f.translateCalls).toEqual([]);
  });

  it("names a language this device cannot translate from", async () => {
    const c = controller();
    c.translate("text", ref(5), "kumusta", "tl");
    await flush();
    expect(c.textView("text", ref(5))).toEqual({ phase: "unsupported", language: "tl" });
  });

  it("forgets everything made from a deleted message", async () => {
    f.translationState.set("es>en", "ready");
    const c = controller();
    c.detected(ref(1), "hola amigos");
    await flush();
    c.translate("text", ref(1), "hola amigos");
    await flush();
    c.forgetMessage(CONV, 1);
    await flush();
    expect(await c.count()).toBe(0);
    expect(c.textView("text", ref(1)).phase).toBe("none");
  });

  it("an edit after the translation, even back to the same words, needs a new one", async () => {
    f.translationState.set("es>en", "ready");
    const c = controller();
    c.detected(ref(1, 30), "hola amigos");
    await flush();
    c.translate("text", ref(1, 30), "hola amigos");
    await flush();
    expect(c.textView("text", ref(1, 30)).phase).toBe("translated");
    // Same text, a later edit op: the stored result no longer describes it.
    expect(c.textView("text", ref(1, 31)).phase).toBe("none");
  });

  it("a deleted conversation leaves nothing behind", async () => {
    f.translationState.set("es>en", "ready");
    const c = controller();
    c.setAutoTranslate(CONV, true);
    c.detected(ref(1), "hola amigos");
    await flush();
    c.translate("text", ref(1), "hola amigos");
    await flush();
    await c.forgetConversation(CONV);
    expect(backend.rows.size).toBe(0);
    expect(c.textView("text", ref(1)).phase).toBe("none");
    expect(c.prefs(CONV).autoTranslate).toBe(false);
  });

  it("deletes every result on request, keeping the conversation choices", async () => {
    f.translationState.set("es>en", "ready");
    const c = controller();
    c.setAutoTranslate(CONV, true);
    c.detected(ref(1), "hola amigos");
    await flush();
    c.translate("text", ref(1), "hola amigos");
    await flush();
    expect(await c.count()).toBe(1);
    await c.deleteAll();
    expect(await c.count()).toBe(0);
    expect([...backend.rows.values()].filter((r) => r.cls === "prefs").length).toBe(1);
  });
});

describe("the conversation bar and automatic translation (UX 3.6 / 3.7)", () => {
  const spanish: Detection[] = Array.from({ length: 6 }, () => ({ tag: "es", permille: 900 }));

  it("names the language most recent messages are in, until dismissed", async () => {
    f.translationState.set("es>en", "ready");
    const c = controller();
    c.barLanguage(CONV, spanish);
    await flush();
    expect(c.barLanguage(CONV, spanish)).toBe("es");
    c.dismissBar(CONV, "es");
    await flush();
    expect(c.barLanguage(CONV, spanish)).toBeNull();
  });

  it("is not shown once automatic translation is on, nor for a language the device cannot do", async () => {
    const c = controller();
    c.barLanguage(CONV, spanish);
    await flush();
    expect(c.barLanguage(CONV, spanish)).toBeNull();
    f.translationState.set("es>en", "ready");
    const d = controller();
    d.setAutoTranslate(CONV, true);
    await flush();
    expect(d.barLanguage(CONV, spanish)).toBeNull();
  });

  it("translates as shown when on, and never a message put back to its original", async () => {
    f.translationState.set("es>en", "ready");
    const c = controller();
    c.setAutoTranslate(CONV, true);
    c.detected(ref(1), "hola amigos");
    c.translationState("es");
    await flush();
    c.autoTranslate(ref(1), "hola amigos", false);
    await flush();
    expect(c.textView("text", ref(1)).phase).toBe("translated");
    c.showOriginal("text", ref(1));
    c.autoTranslate(ref(1), "hola amigos", false);
    await flush();
    expect(f.translateCalls.length).toBe(1);
  });

  it("the bar's Translate asks before fetching a pack", async () => {
    f.translationState.set("es>en", "needs-pack");
    const c = controller();
    c.translationState("es");
    await flush();
    c.enableAutoFromBar(CONV, "es");
    await flush();
    expect(c.prefs(CONV).autoTranslate).toBe(true);
    expect(c.packRequest()?.language).toBe("es");
    expect(f.translateCalls).toEqual([]);
    c.packRequest()?.download();
    await flush();
    expect(f.translateCalls).toEqual([{ source: "es", target: "en", text: "" }]);
    expect(packs).toEqual([["es", "translation"]]);
  });

  it("does not download a pack on its own", async () => {
    f.translationState.set("es>en", "needs-pack");
    const c = controller();
    c.setAutoTranslate(CONV, true);
    c.detected(ref(1), "hola amigos");
    c.translationState("es");
    await flush();
    c.autoTranslate(ref(1), "hola amigos", false);
    await flush();
    expect(f.translateCalls).toEqual([]);
    expect(c.packRequest()).toBeNull();
  });
});

describe("transcripts (UX section 4)", () => {
  const audio = (): Promise<Blob | null> => Promise.resolve(new Blob([new Uint8Array([1])]));

  it("offers the button only where the engine can, and with the setting on", async () => {
    const c = controller();
    c.transcriptOffered(CONV, true);
    await flush();
    expect(c.transcriptOffered(CONV, true)).toBe(false);
    f.transcriptionState.set("en", "ready");
    const d = controller();
    d.transcriptOffered(CONV, true);
    await flush();
    expect(d.transcriptOffered(CONV, true)).toBe(true);
    settings = { ...DEFAULT_SETTINGS, showTranscript: false };
    expect(d.transcriptOffered(CONV, true)).toBe(false);
  });

  it("transcribes, stores, hides and reopens", async () => {
    f.transcriptionState.set("en", "ready");
    const c = controller();
    c.toggleTranscript(ref(7), audio);
    await flush();
    expect(c.transcriptView(ref(7))).toEqual({ phase: "shown", text: "hola amigos", language: "en", complete: true });
    c.toggleTranscript(ref(7), audio);
    expect(c.transcriptView(ref(7)).phase).toBe("hidden");
    const again = controller();
    again.transcriptView(ref(7));
    await flush();
    expect(again.transcriptView(ref(7)).phase).toBe("hidden");
  });

  it("says when nothing was heard, and when the file is gone", async () => {
    f.transcriptionState.set("en", "ready");
    f.transcribeOutcome = { text: "  ", complete: true };
    const c = controller();
    c.toggleTranscript(ref(8), audio);
    await flush();
    expect(c.transcriptView(ref(8))).toEqual({ phase: "no-speech", language: "en" });
    c.toggleTranscript(ref(9), () => Promise.resolve(null));
    await flush();
    expect(c.transcriptView(ref(9)).phase).toBe("unavailable-file");
  });

  it("says it is downloading a pack whose progress is not reported, and Cancel stops what follows", async () => {
    f.transcriptionState.set("en", "needs-pack");
    let finish: (ok: boolean) => void = () => undefined;
    f.engines = {
      ...f.engines,
      transcription: {
        ...f.engines.transcription,
        install: () => new Promise<boolean>((r) => (finish = r)),
      },
    };
    const c = controller();
    c.toggleTranscript(ref(7), audio);
    await flush();
    c.packRequest()?.download();
    await flush();
    expect(c.transcriptView(ref(7))).toEqual({ phase: "installing", language: "en" });
    c.cancelTranscriptInstall(ref(7));
    finish(true);
    await flush();
    expect(c.transcriptView(ref(7)).phase).toBe("none");
  });

  it("turns transcription off for good when the microphone guard fires", async () => {
    f.transcriptionState.set("en", "ready");
    const guard = new Error("guard");
    guard.name = "MicrophoneGuard";
    f.transcribeOutcome = guard;
    const c = controller();
    c.transcriptOffered(CONV, true);
    await flush();
    expect(c.transcriptOffered(CONV, true)).toBe(true);
    c.toggleTranscript(ref(7), audio);
    await flush();
    expect(settings.transcriptionBlocked).toBe(true);
    expect(c.transcriptionPresent).toBe(false);
    expect(c.transcriptOffered(CONV, true)).toBe(false);
    expect(c.transcriptView(ref(7)).phase).toBe("none");
  });

  it("installs a pack only after Download", async () => {
    f.transcriptionState.set("en", "needs-pack");
    const c = controller();
    c.toggleTranscript(ref(7), audio);
    await flush();
    expect(c.packRequest()?.use).toBe("transcription");
    expect(f.installed).toEqual([]);
    c.packRequest()?.download();
    await flush();
    expect(f.installed).toEqual(["en"]);
    expect(c.transcriptView(ref(7)).phase).toBe("shown");
    expect(packs).toEqual([["en", "transcription"]]);
  });

  it("a transcript can be translated, and the menu text is then the transcript", async () => {
    f.transcriptionState.set("en", "ready");
    f.translationState.set("es>en", "ready");
    const c = controller();
    c.toggleTranscript(ref(7), audio);
    await flush();
    c.detected(ref(7), "hola amigos");
    await flush();
    c.toggle("transcript-translation", ref(7), "hola amigos");
    await flush();
    expect(c.textView("transcript-translation", ref(7))).toEqual({
      phase: "translated",
      source: "es",
      text: "hello friends",
    });
  });
});
