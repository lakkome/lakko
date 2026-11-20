// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The desktop host's system engines as the page sees them (ADR-0082 note of 2026-11-18). */

import { describe, expect, it } from "vitest";

import {
  HOST_ENGINES_PATH,
  hostDetection,
  hostSystemPacks,
  hostTranscription,
  hostTranslation,
  int16le,
  systemEngineCapabilities,
} from "./host-engines";

interface Seen {
  url: string;
  body: unknown;
}

function host(answers: Record<string, unknown>, seen: Seen[] = [], status = 200) {
  return (url: string, init?: RequestInit): Promise<Response> => {
    seen.push({ url, body: init?.body ?? null });
    const op = url.slice(HOST_ENGINES_PATH.length + 1).split("?")[0] ?? "";
    return Promise.resolve(new Response(JSON.stringify(answers[op] ?? {}), { status }));
  };
}

describe("capabilities", () => {
  it("are true only for a true boolean", () => {
    expect(systemEngineCapabilities({ translation: true, transcription: true })).toEqual({
      translation: true,
      transcription: true,
    });
    expect(systemEngineCapabilities({ translation: "yes" })).toEqual({ translation: false, transcription: false });
    expect(systemEngineCapabilities(undefined)).toEqual({ translation: false, transcription: false });
  });
});

describe("translation", () => {
  it("maps the host's states and fails closed on anything else", async () => {
    const t = hostTranslation(
      true,
      host({ "translation-state": { ok: true, state: "needs-pack" } }),
    );
    expect(await t.state("es", "en")).toBe("needs-pack");
    expect(await hostTranslation(true, host({ "translation-state": { ok: true, state: "maybe" } })).state("es", "en")).toBe(
      "unavailable",
    );
    expect(await hostTranslation(true, host({ "translation-state": { ok: false } })).state("es", "en")).toBe("unavailable");
    expect(await hostTranslation(true, host({}, [], 503)).state("es", "en")).toBe("unavailable");
  });

  it("returns the text, refuses an error answer, and leaves packs to System Settings", async () => {
    const seen: Seen[] = [];
    const t = hostTranslation(true, host({ translate: { ok: true, text: "hello" } }, seen));
    expect(await t.translate("es", "en", "hola")).toBe("hello");
    expect(JSON.parse(String(seen[0]?.body))).toEqual({ source: "es", target: "en", text: "hola" });
    await expect(
      hostTranslation(true, host({ translate: { ok: false, error: "not-installed" } })).translate("es", "en", "hola"),
    ).rejects.toThrow("not-installed");
    expect(t.packFetch?.("es", "en")).toBe("system-settings");
  });

  it("is absent without the capability", () => {
    expect(hostTranslation(false).present).toBe(false);
  });
});

describe("detection", () => {
  it("answers the top hypothesis, or the ranked one when a ranker is given", async () => {
    const answers = { detect: { ok: true, hypotheses: [{ tag: "es", permille: 990 }, { tag: "ca", permille: 10 }] } };
    expect(await hostDetection(true, null, host(answers)).detect("hola")).toEqual({ tag: "es", permille: 990 });
    const ranked = await hostDetection(
      true,
      (_text, tags, permilles) => Promise.resolve({ tag: tags[0] ?? "", permille: permilles[0] ?? 0, alternatives: ["ca"] }),
      host(answers),
    ).detect("hola");
    expect(ranked).toEqual({ tag: "es", permille: 990, alternatives: ["ca"] });
    expect(await hostDetection(true, null, host({ detect: { ok: true, hypotheses: [] } })).detect("hola")).toBeNull();
    expect(await hostDetection(true, null, host(answers)).detect("   ")).toBeNull();
  });
});

describe("transcription", () => {
  it("sends the decoded audio and the language, and returns a complete transcript", async () => {
    const seen: Seen[] = [];
    const pcm = int16le(new Float32Array([0, 0.5, -1]));
    const s = hostTranscription(true, host({ transcribe: { ok: true, text: "hello" } }, seen), () => Promise.resolve(pcm));
    expect(await s.transcribe(new Blob(), "en")).toEqual({ text: "hello", complete: true });
    expect(seen[0]?.url).toBe(`${HOST_ENGINES_PATH}/transcribe?language=en`);
    expect(seen[0]?.body).toBe(pcm);
  });

  it("install reports the host's answer", async () => {
    expect(await hostTranscription(true, host({ "speech-install": { ok: true } })).install("es")).toBe(true);
    expect(await hostTranscription(true, host({ "speech-install": { ok: false, error: "failed" } })).install("es")).toBe(
      false,
    );
  });
});

describe("16-bit PCM", () => {
  it("is little-endian and clamped", () => {
    const out = int16le(new Float32Array([0, 1, -1, 2]));
    const view = new DataView(out.buffer);
    expect(view.getInt16(0, true)).toBe(0);
    expect(view.getInt16(2, true)).toBe(32767);
    expect(view.getInt16(4, true)).toBe(-32768);
    expect(view.getInt16(6, true)).toBe(32767);
  });
});

describe("packs and anti-censorship (ADR-0082 amendment of 2026-11-19)", () => {
  it("installs a speech pack without asking the host about anti-censorship", async () => {
    const seen: Seen[] = [];
    // The host as it answered while anti-censorship was on, before the amendment.
    const t = hostTranscription(
      true,
      host({ "packs-blocked": { blocked: true }, "speech-install": { ok: true } }, seen),
    );
    expect(await t.install("es")).toBe(true);
    expect(seen.map((s) => s.url)).toEqual([`${HOST_ENGINES_PATH}/speech-install`]);
  });
});

describe("system packs", () => {
  const both = { translation: true, transcription: true };

  it("lists what macOS holds, for the target asked", async () => {
    const seen: Seen[] = [];
    const answer = { installed: { ok: true, translation: ["es", "de"], transcription: ["en-US", 7, ""] } };
    expect(await hostSystemPacks(both, "en", host(answer, seen))).toEqual({
      translation: ["es", "de"],
      transcription: ["en-US"],
    });
    expect(seen[0]?.url).toBe(`${HOST_ENGINES_PATH}/installed`);
    expect(JSON.parse(String(seen[0]?.body))).toEqual({ target: "en" });
  });

  it("lists nothing for an engine the host lacks, or on any failure", async () => {
    const answer = { installed: { ok: true, translation: ["es"], transcription: ["en-US"] } };
    expect(await hostSystemPacks({ translation: false, transcription: true }, "en", host(answer))).toEqual({
      translation: [],
      transcription: ["en-US"],
    });
    const none = { translation: [], transcription: [] };
    expect(await hostSystemPacks(both, "en", host({ installed: { ok: false } }))).toEqual(none);
    expect(await hostSystemPacks(both, "en", host({}, [], 503))).toEqual(none);
  });

  it("never fetches a translation pack itself", () => {
    expect(hostTranslation(true, host({})).fetchesPacks).toBe(false);
  });
});
