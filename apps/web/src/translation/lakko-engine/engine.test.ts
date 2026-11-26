// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0100 — Lakko's own translation engine over fakes: the manifest rules,
 * pair states, the groups a download fetches, what the dialog is told, the
 * worker calls, and that the engine fails closed.
 */

import { describe, expect, it } from "vitest";

import { translationInOrder } from "../engine-order";
import type { EngineState, TranslationEngine } from "../engines";
import { directionLanguages, type EnginePort, lakkoTranslation, type LakkoEngineDeps, type Slice } from "./engine";
import { isPhoneOrTabletBrowser } from "./device";
import { groupOf, modelLanguage, readManifest, routeFor, type Manifest } from "./manifest";
import shipped from "./models.json";

const H = (c: string): string => c.repeat(64);

function file(offset: number, length: number, tag: string): Record<string, unknown> {
  return {
    name: `model.${tag}.intgemm.alphas.bin`,
    offset,
    length,
    sha256: H(tag[0] ?? "a"),
    decompressedSha256: H("f"),
    decompressedBytes: length * 2,
  };
}

/** Two groups: {es, pt} and {ru, uk}; every language to and from English. */
function fixture(): Manifest {
  const dir = (group: number, base: number): Record<string, unknown> => ({
    group,
    architecture: "base-memory",
    version: "3.0",
    files: { model: file(base, 10, "a"), lex: file(base + 10, 5, "b"), vocab: file(base + 15, 5, "c") },
  });
  const m = readManifest({
    format: 1,
    groups: [
      { id: 1, languages: ["es", "pt"], file: H("1"), bytes: 80 },
      { id: 2, languages: ["ru", "uk"], file: H("2"), bytes: 80 },
    ],
    directions: {
      "en-es": dir(1, 0),
      "es-en": dir(1, 20),
      "en-pt": dir(1, 40),
      "pt-en": dir(1, 60),
      "en-ru": dir(2, 0),
      "ru-en": dir(2, 20),
      "en-uk": dir(2, 40),
      "uk-en": dir(2, 60),
    },
  });
  if (m === null) throw new Error("fixture does not read");
  return m;
}

interface Recorder {
  readonly calls: string[];
  readonly downloads: string[];
  stored: Set<string>;
}

function deps(over: Partial<LakkoEngineDeps> = {}, rec?: Recorder): LakkoEngineDeps & { rec: Recorder } {
  const r: Recorder = rec ?? { calls: [], downloads: [], stored: new Set() };
  const newPort = (): EnginePort => {
    let key = "";
    return {
      call(request) {
        if (request.type === "load") key = request.key;
        r.calls.push(request.type === "init" ? "init" : `${request.type}:${request.type === "load" ? request.key : key}`);
        if (request.type === "translate") return Promise.resolve(`[${key}:${request.text}]`);
        return Promise.resolve(null);
      },
      terminate() {
        r.calls.push(`terminate:${key}`);
      },
    };
  };
  return {
    manifest: fixture(),
    supported: true,
    storedGroups: () => Promise.resolve([...r.stored]),
    readSlice: (group) => Promise.resolve(r.stored.has(group) ? new ArrayBuffer(4) : null),
    downloadGroup: (group: string, _bytes: number, _slices: readonly Slice[], options) => {
      r.downloads.push(group);
      options.onProgress?.(1);
      r.stored.add(group);
      return Promise.resolve();
    },
    removeGroup: (group) => {
      r.stored.delete(group);
      return Promise.resolve();
    },
    engineBinary: () => Promise.resolve(new ArrayBuffer(8)),
    newPort,
    ...over,
    rec: r,
  };
}

describe("manifest rules", () => {
  it("maps client tags to the manifest's codes", () => {
    expect(modelLanguage("zh")).toBe("zh-Hans");
    expect(modelLanguage("zh-TW")).toBe("zh-Hant");
    expect(modelLanguage("zh-Hant-HK")).toBe("zh-Hant");
    expect(modelLanguage("no")).toBe("nb");
    expect(modelLanguage("nn")).toBeNull();
    expect(modelLanguage("pt-BR")).toBe("pt");
    expect(modelLanguage("iw")).toBe("he");
    expect(modelLanguage("fil")).toBe("tl");
    expect(modelLanguage("en-GB")).toBe("en");
    expect(modelLanguage("")).toBeNull();
  });

  it("routes directly through a direction, else through English", () => {
    const m = fixture();
    expect(routeFor(m, "es", "en")).toEqual(["es-en"]);
    expect(routeFor(m, "en", "ru")).toEqual(["en-ru"]);
    expect(routeFor(m, "es", "ru")).toEqual(["es-en", "en-ru"]);
    expect(routeFor(m, "es", "es")).toEqual([]);
    expect(routeFor(m, "es", "fr")).toEqual([]);
    expect(groupOf(m, "uk")?.id).toBe(2);
  });

  it("refuses a malformed manifest whole", () => {
    expect(readManifest(null)).toBeNull();
    expect(readManifest({ format: 2, groups: [], directions: {} })).toBeNull();
    // A file that runs past its group file.
    expect(
      readManifest({
        format: 1,
        groups: [{ id: 1, languages: ["es"], file: H("1"), bytes: 10 }],
        directions: { "es-en": { group: 1, files: { model: file(0, 8, "a"), lex: file(8, 5, "b"), vocab: file(0, 1, "c") } } },
      }),
    ).toBeNull();
    // A direction without a vocabulary.
    expect(
      readManifest({
        format: 1,
        groups: [{ id: 1, languages: ["es"], file: H("1"), bytes: 20 }],
        directions: { "es-en": { group: 1, files: { model: file(0, 8, "a"), lex: file(8, 5, "b") } } },
      }),
    ).toBeNull();
  });

  it("splits direction keys", () => {
    expect(directionLanguages("en-zh-Hans")).toEqual({ source: "en", target: "zh-Hans" });
    expect(directionLanguages("zh-Hant-en")).toEqual({ source: "zh-Hant", target: "en" });
  });
});

describe("the shipped manifest (ADR-0100 sections 2 and 4)", () => {
  const m = readManifest(shipped);

  it("reads, with 19 groups of three and 114 directions", () => {
    expect(m).not.toBeNull();
    if (m === null) return;
    expect(m.groups.length).toBe(19);
    for (const g of m.groups) expect(g.languages.length).toBe(3);
    expect(Object.keys(m.directions).length).toBe(114);
    const all = m.groups.flatMap((g) => g.languages);
    expect(new Set(all).size).toBe(57);
  });

  it("covers each group file exactly with its model files", () => {
    if (m === null) return;
    for (const g of m.groups) {
      const slices = Object.values(m.directions)
        .filter((d) => d.group === g.id)
        .flatMap((d) => Object.values(d.files))
        .sort((a, b) => a.offset - b.offset);
      let at = 0;
      for (const s of slices) {
        expect(s.offset).toBe(at);
        at += s.length;
      }
      expect(at).toBe(g.bytes);
    }
  });

  it("puts every interface language in a group", () => {
    if (m === null) return;
    for (const tag of ["ar", "es", "fa", "he", "hi", "pt-BR", "ru", "tr", "uk", "zh-Hans"]) {
      expect(groupOf(m, tag), tag).not.toBeNull();
    }
  });
});

describe("lakkoTranslation", () => {
  it("is absent where the browser cannot run it or the manifest is unreadable", async () => {
    for (const engine of [lakkoTranslation(deps({ supported: false })), lakkoTranslation(deps({ manifest: null }))]) {
      expect(engine.present).toBe(false);
      expect(await engine.state("es", "en")).toBe("unavailable");
      await expect(engine.translate("es", "en", "hola")).rejects.toThrow();
    }
  });

  it("reports needs-pack until every group of the route is here, then ready", async () => {
    const d = deps();
    const engine = lakkoTranslation(d);
    expect(await engine.state("es", "en")).toBe("needs-pack");
    expect(await engine.state("es", "fr")).toBe("unavailable");
    d.rec.stored.add(H("1"));
    const fresh = lakkoTranslation(deps({}, d.rec));
    expect(await fresh.state("es", "en")).toBe("ready");
    expect(await fresh.state("es", "ru")).toBe("needs-pack");
  });

  it("tells the dialog the groups still missing and their size", async () => {
    const d = deps();
    d.rec.stored.add(H("2"));
    const engine = lakkoTranslation(d);
    expect(await engine.packDisclosure?.("es", "ru")).toEqual({ groups: [{ languages: ["es", "pt"], bytes: 80 }] });
    expect(await engine.packDisclosure?.("es", "fr")).toBeNull();
  });

  it("downloads only inside translate, then loads and translates through English", async () => {
    const d = deps();
    const engine = lakkoTranslation(d);
    await engine.state("es", "ru");
    expect(d.rec.downloads).toEqual([]);
    const progress: number[] = [];
    const out = await engine.translate("es", "ru", "hola", { onProgress: (f) => progress.push(f) });
    // Spanish -> English in one worker, its output English -> Russian in another.
    expect(out).toBe("[en-ru:[es-en:hola]]");
    expect(d.rec.downloads).toEqual([H("1"), H("2")]);
    expect(progress.at(-1)).toBe(1);
    expect(d.rec.calls).toEqual(["init", "load:es-en", "init", "load:en-ru", "translate:es-en", "translate:en-ru"]);
    expect(await engine.state("es", "ru")).toBe("ready");
    // A second translation loads nothing again.
    await engine.translate("es", "ru", "adiós");
    expect(d.rec.calls.slice(6)).toEqual(["translate:es-en", "translate:en-ru"]);
    // A third direction stops the least recently used worker first.
    await engine.translate("en", "es", "hi");
    expect(d.rec.calls.slice(8)).toEqual(["terminate:es-en", "init", "load:en-es", "translate:en-es"]);
  });

  it("lists and removes the groups it downloaded", async () => {
    const d = deps();
    const engine = lakkoTranslation(d);
    await engine.translate("es", "en", "hola");
    expect(await engine.ownPacks?.()).toEqual([{ id: H("1"), languages: ["es", "pt"], bytes: 80 }]);
    await engine.removeOwnPack?.(H("1"));
    expect(await engine.ownPacks?.()).toEqual([]);
    expect(await engine.state("es", "en")).toBe("needs-pack");
    expect(d.rec.calls).toContain("terminate:es-en");
  });

  it("fails the translation when a download fails, and stores nothing", async () => {
    const d = deps({ downloadGroup: () => Promise.reject(new Error("network")) });
    const engine = lakkoTranslation(d);
    await expect(engine.translate("es", "en", "hola")).rejects.toThrow("network");
    expect(await engine.state("es", "en")).toBe("needs-pack");
    expect(d.rec.calls).toEqual([]);
  });

  it("fails when the worker refuses a model, translating nothing", async () => {
    const stopped: string[] = [];
    const port: EnginePort = {
      call: (r) => (r.type === "load" ? Promise.reject(new Error("model file hash mismatch")) : Promise.resolve(null)),
      terminate: () => stopped.push("stopped"),
    };
    const d = deps({ newPort: () => port });
    d.rec.stored.add(H("1"));
    await expect(lakkoTranslation(d).translate("es", "en", "hola")).rejects.toThrow("hash mismatch");
    expect(stopped).toEqual(["stopped"]);
  });
});

describe("in the engine order (ADR-0100 section 6)", () => {
  function fixed(state: EngineState): TranslationEngine {
    return {
      present: true,
      state: () => Promise.resolve(state),
      translate: () => Promise.resolve("browser"),
    };
  }

  it("owns a pair only where the engine before it does not", async () => {
    const d = deps();
    const lakko = lakkoTranslation(d);
    const withBrowser = translationInOrder([fixed("ready"), lakko]);
    expect(await withBrowser.state("es", "en")).toBe("ready");
    expect(await withBrowser.translate("es", "en", "hola")).toBe("browser");
    expect(withBrowser.keepsOwnPacks?.("es", "en")).toBe(false);
    const without = translationInOrder([fixed("unavailable"), lakko]);
    expect(await without.state("es", "en")).toBe("needs-pack");
    expect(without.keepsOwnPacks?.("es", "en")).toBe(true);
    expect(await without.packDisclosure?.("es", "en")).toEqual({ groups: [{ languages: ["es", "pt"], bytes: 80 }] });
    expect(d.rec.downloads).toEqual([]);
  });
});

describe("isPhoneOrTabletBrowser (ADR-0100 open question 4)", () => {
  it("is told by the browser where it says, else by the user agent", () => {
    expect(isPhoneOrTabletBrowser({ userAgentData: { mobile: true }, userAgent: "" })).toBe(true);
    expect(
      isPhoneOrTabletBrowser({ userAgent: "Mozilla/5.0 (Linux; Android 14; Pixel 7a) AppleWebKit/537.36 Mobile Safari/537.36" }),
    ).toBe(true);
    expect(isPhoneOrTabletBrowser({ userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)" })).toBe(true);
    // iPadOS reports a Mac; its touch points tell it apart.
    expect(isPhoneOrTabletBrowser({ userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", maxTouchPoints: 5 })).toBe(true);
    expect(isPhoneOrTabletBrowser({ userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", maxTouchPoints: 0 })).toBe(false);
    expect(
      isPhoneOrTabletBrowser({ userAgent: "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0" }),
    ).toBe(false);
  });
});
