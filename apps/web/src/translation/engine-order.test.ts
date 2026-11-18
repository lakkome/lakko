// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0082 note of 2026-11-18 — the system engine where it has the language,
 * otherwise the next engine; every call for a language goes to its owner.
 */

import { describe, expect, it } from "vitest";

import { enginesInOrder, transcriptionInOrder, translationInOrder } from "./engine-order";
import type { EngineState, Engines, TranscriptionEngine, TranslationEngine } from "./engines";

function translator(name: string, states: Record<string, EngineState>, calls: string[]): TranslationEngine {
  return {
    present: true,
    state: (s, t) => Promise.resolve(states[`${s}>${t}`] ?? "unavailable"),
    translate: (s, t) => {
      calls.push(`${name}:${s}>${t}`);
      return Promise.resolve(`${name} text`);
    },
    packFetch: () => (name === "system" ? "system-settings" : "engine"),
    ...(name === "system" ? { openPackSettings: () => Promise.resolve() } : {}),
  };
}

function recognizer(name: string, states: Record<string, EngineState>, calls: string[]): TranscriptionEngine {
  return {
    present: true,
    state: (l) => Promise.resolve(states[l] ?? "unavailable"),
    install: (l) => {
      calls.push(`${name}:install:${l}`);
      return Promise.resolve(true);
    },
    transcribe: (_a, l) => {
      calls.push(`${name}:${l}`);
      return Promise.resolve({ text: name, complete: true });
    },
  };
}

describe("translation in order", () => {
  it("uses the system for its languages and the next engine for the rest", async () => {
    const calls: string[] = [];
    const t = translationInOrder([
      translator("system", { "es>en": "ready", "de>en": "needs-pack" }, calls),
      translator("own", { "es>en": "ready", "ru>en": "ready", "de>en": "ready" }, calls),
    ]);
    expect(await t.state("es", "en")).toBe("ready");
    expect(await t.state("ru", "en")).toBe("ready");
    // A system pack not yet on the device still belongs to the system.
    expect(await t.state("de", "en")).toBe("needs-pack");
    expect(await t.state("xx", "en")).toBe("unavailable");
    await t.translate("es", "en", "hola");
    await t.translate("ru", "en", "privet");
    expect(calls).toEqual(["system:es>en", "own:ru>en"]);
    expect(t.packFetch?.("de", "en")).toBe("system-settings");
    expect(t.packFetch?.("ru", "en")).toBe("engine");
    expect(t.openPackSettings).toBeTypeOf("function");
  });

  it("is absent when no engine in the list is present", () => {
    const absent: TranslationEngine = {
      present: false,
      state: () => Promise.resolve("unavailable"),
      translate: () => Promise.reject(new Error("none")),
    };
    expect(translationInOrder([absent]).present).toBe(false);
  });

  it("an engine whose state throws counts as unavailable", async () => {
    const calls: string[] = [];
    const broken: TranslationEngine = {
      present: true,
      state: () => Promise.reject(new Error("host gone")),
      translate: () => Promise.reject(new Error("host gone")),
    };
    const t = translationInOrder([broken, translator("own", { "es>en": "ready" }, calls)]);
    expect(await t.state("es", "en")).toBe("ready");
    await t.translate("es", "en", "hola");
    expect(calls).toEqual(["own:es>en"]);
  });
});

describe("transcription in order", () => {
  it("routes install and transcribe to the owner of the language", async () => {
    const calls: string[] = [];
    const s = transcriptionInOrder([
      recognizer("system", { en: "ready", es: "needs-pack" }, calls),
      recognizer("own", { ru: "ready", es: "ready" }, calls),
    ]);
    expect(await s.state("es")).toBe("needs-pack");
    expect(await s.state("ru")).toBe("ready");
    await s.install("es");
    await s.transcribe(new Blob(), "ru");
    await s.transcribe(new Blob(), "en");
    expect(calls).toEqual(["system:install:es", "own:ru", "system:en"]);
  });
});

describe("a whole set", () => {
  it("detects with the first engine that answers, and any blocker blocks", async () => {
    const quiet: Engines = {
      translation: translator("system", {}, []),
      detection: { present: true, detect: () => Promise.resolve(null) },
      transcription: recognizer("system", {}, []),
      packsBlocked: () => Promise.resolve(true),
    };
    const loud: Engines = {
      translation: translator("own", {}, []),
      detection: { present: true, detect: () => Promise.resolve({ tag: "es", permille: 900 }) },
      transcription: recognizer("own", {}, []),
    };
    const all = enginesInOrder([quiet, loud]);
    expect(await all.detection.detect("hola")).toEqual({ tag: "es", permille: 900 });
    expect(await all.packsBlocked?.()).toBe(true);
    expect(enginesInOrder([loud]).packsBlocked).toBeUndefined();
  });
});

describe("packs", () => {
  it("fetches packs when any engine in the list does", () => {
    const system = { ...translator("system", {}, []), fetchesPacks: false };
    expect(translationInOrder([system]).fetchesPacks).toBe(false);
    expect(translationInOrder([system, translator("own", {}, [])]).fetchesPacks).toBe(true);
  });

  it("lists the system's packs from every engine set that can, and nothing where none can", async () => {
    const mac: Engines = {
      translation: translator("system", {}, []),
      detection: { present: false, detect: () => Promise.resolve(null) },
      transcription: recognizer("system", {}, []),
      systemPacks: (target) => Promise.resolve({ translation: [`es>${target}`], transcription: ["en-US"] }),
    };
    const broken: Engines = { ...mac, systemPacks: () => Promise.reject(new Error("gone")) };
    const browser: Engines = {
      translation: translator("own", {}, []),
      detection: { present: false, detect: () => Promise.resolve(null) },
      transcription: recognizer("own", {}, []),
    };
    expect(await enginesInOrder([mac, broken, browser]).systemPacks?.("en")).toEqual({
      translation: ["es>en"],
      transcription: ["en-US"],
    });
    expect(enginesInOrder([browser]).systemPacks).toBeUndefined();
  });
});
