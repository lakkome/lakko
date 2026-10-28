// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0082 section 2 — the web client never runs a speech recognizer that
 * may send audio to a server, and the translation code reaches no network.
 *
 * A source scan in the shape of `ui/no-system-share.test.ts`, over code with
 * comments and string literals removed, plus behavioural checks of the one
 * place a recognizer is built:
 *
 * 1. `webkitSpeechRecognition` appears nowhere in the app: the prefixed
 *    constructor has no `processLocally`, so it cannot be told to stay local.
 * 2. `SpeechRecognition` is constructed nowhere: the only construction is
 *    `new api()` inside `createLocalRecognizer`, which sets
 *    `processLocally = true` before any other use and before `start`, and
 *    `SpeechRecognition` is not named outside `browser-engines.ts`.
 * 3. Nothing under `src/translation/` calls `fetch`, opens an
 *    `XMLHttpRequest`, a `WebSocket` or an `EventSource`, sends a beacon, or
 *    imports a module outside the short list below. The audio a transcript
 *    reads is handed in by the bubble from the media hub, decrypted the way
 *    playing decrypts it.
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  browserTranscription,
  createLocalRecognizer,
  localRecognitionSupported,
  type LocalRecognizer,
  type RecognitionStatic,
} from "./browser-engines";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const SRC_ROOT = join(HERE, "..");

function stripCommentsAndStrings(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, " ")
    .replace(/(^|[^:])\/\/[^\n]*/g, "$1 ")
    .replace(/`(?:\\.|[^`\\])*`/g, " ")
    .replace(/"(?:\\.|[^"\\\n])*"/g, " ")
    .replace(/'(?:\\.|[^'\\\n])*'/g, " ");
}

function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) sourceFiles(full, out);
    else if (/\.tsx?$/.test(entry) && !/\.test\.tsx?$/.test(entry)) out.push(full);
  }
  return out;
}

const FILES = sourceFiles(SRC_ROOT);
const ENGINES_FILE = join(HERE, "browser-engines.ts");

/** Whether code constructs a speech recognizer directly. */
export function constructsRecognizer(code: string): boolean {
  return /\bnew\s+(?:window\s*\.\s*|globalThis\s*\.\s*|self\s*\.\s*)?(?:webkit)?SpeechRecognition\b/.test(code);
}

/** Whether code names the prefixed constructor at all. */
export function namesPrefixed(code: string): boolean {
  return /\bwebkitSpeechRecognition\b/.test(code);
}

/** Whether code names the unprefixed constructor as an identifier. */
function namesUnprefixed(code: string): boolean {
  return /\bSpeechRecognition\b/.test(code);
}

/** Network APIs a translation file must not use. */
export function usesNetwork(code: string): boolean {
  return (
    /\bfetch\s*\(/.test(code) ||
    /\bXMLHttpRequest\b/.test(code) ||
    /\bWebSocket\b/.test(code) ||
    /\bEventSource\b/.test(code) ||
    /\bsendBeacon\b/.test(code) ||
    /\bimport\s*\(/.test(code)
  );
}

describe("no remote speech recognizer (ADR-0082 section 2)", () => {
  it("scans the whole app", () => {
    expect(FILES.length).toBeGreaterThan(50);
    expect(FILES).toContain(ENGINES_FILE);
  });

  it("never names the prefixed constructor", () => {
    const offenders = FILES.filter((f) => namesPrefixed(stripCommentsAndStrings(readFileSync(f, "utf8"))));
    expect(offenders.map((f) => f.slice(SRC_ROOT.length + 1))).toEqual([]);
  });

  it("never constructs SpeechRecognition directly", () => {
    const offenders = FILES.filter((f) => constructsRecognizer(stripCommentsAndStrings(readFileSync(f, "utf8"))));
    expect(offenders.map((f) => f.slice(SRC_ROOT.length + 1))).toEqual([]);
  });

  it("names SpeechRecognition only in the engines module", () => {
    const offenders = FILES.filter(
      (f) => f !== ENGINES_FILE && namesUnprefixed(stripCommentsAndStrings(readFileSync(f, "utf8"))),
    );
    expect(offenders.map((f) => f.slice(SRC_ROOT.length + 1))).toEqual([]);
  });

  it("builds a recognizer in one place, local before anything else", () => {
    const code = stripCommentsAndStrings(readFileSync(ENGINES_FILE, "utf8"));
    const constructions = code.match(/\bnew\s+api\s*\(/g) ?? [];
    expect(constructions.length).toBe(1);
    const fn = code.slice(code.indexOf("function createLocalRecognizer"));
    const body = fn.slice(0, fn.indexOf("\n}\n"));
    const created = body.search(/\bnew\s+api\s*\(/);
    const local = body.search(/\.processLocally\s*=\s*true/);
    const check = body.search(/\.processLocally\s*!==\s*true/);
    expect(created).toBeGreaterThan(-1);
    expect(local).toBeGreaterThan(created);
    expect(check).toBeGreaterThan(local);
    // Nothing between construction and the flag touches the recognizer.
    expect(body.slice(created, local)).not.toMatch(/\.(start|lang|continuous)\b/);
    // Every start goes through a recognizer from that function.
    const starts = code.match(/\.start\s*\(\s*track\s*\)/g) ?? [];
    expect(starts.length).toBe(1);
    expect(code).toMatch(/const recognizer = createLocalRecognizer\(/);
  });

  it("scans code and not prose", () => {
    expect(constructsRecognizer("const r = new SpeechRecognition();")).toBe(true);
    expect(constructsRecognizer("const r = new window.webkitSpeechRecognition();")).toBe(true);
    expect(constructsRecognizer(stripCommentsAndStrings("// new SpeechRecognition()\n"))).toBe(false);
    expect(namesPrefixed(stripCommentsAndStrings('const s = "webkitSpeechRecognition";'))).toBe(false);
    expect(usesNetwork("await fetch(url)")).toBe(true);
    expect(usesNetwork(stripCommentsAndStrings("// fetch(url)"))).toBe(false);
  });
});

describe("the translation code reaches no network", () => {
  const translationFiles = FILES.filter((f) => f.startsWith(HERE));
  /** What a translation module may import. */
  const ALLOWED = [
    /^\.\//,
    /^solid-js(\/web)?$/,
    /^\.\.\/l10n$/,
    /^\.\.\/wasm$/,
    /^\.\.\/ui\/(icons|ModalLayer|confirm-dialog|LakkoPicker)$/,
    /^\.\.\/auth\/(account-store-gate|account-memory|key-store|AuthContext)$/,
  ];

  it("scans every translation module", () => {
    expect(translationFiles.length).toBeGreaterThanOrEqual(10);
  });

  it("uses no network API", () => {
    const offenders = translationFiles.filter((f) => usesNetwork(stripCommentsAndStrings(readFileSync(f, "utf8"))));
    expect(offenders.map((f) => f.slice(SRC_ROOT.length + 1))).toEqual([]);
  });

  it("imports only the listed modules", () => {
    const bad: string[] = [];
    for (const f of translationFiles) {
      const src = readFileSync(f, "utf8");
      for (const m of src.matchAll(/\bfrom\s+"([^"]+)"/g)) {
        const spec = m[1] ?? "";
        if (!ALLOWED.some((re) => re.test(spec))) bad.push(`${f.slice(SRC_ROOT.length + 1)}: ${spec}`);
      }
    }
    expect(bad).toEqual([]);
  });
});

describe("createLocalRecognizer", () => {
  function fakeApi(keepsFlag: boolean): RecognitionStatic {
    class Fake {
      private flag = false;
      get processLocally(): boolean {
        return this.flag;
      }
      set processLocally(v: boolean) {
        if (keepsFlag) this.flag = v;
      }
      lang = "";
      continuous = false;
      interimResults = false;
      maxAlternatives = 0;
      onresult = null;
      onerror = null;
      onend = null;
      start(): void {}
      stop(): void {}
      abort(): void {}
      static available(): Promise<unknown> {
        return Promise.resolve("available");
      }
      static install(): Promise<unknown> {
        return Promise.resolve(true);
      }
    }
    return Fake as unknown as RecognitionStatic;
  }

  it("returns a recognizer told to stay on the device", () => {
    const r: LocalRecognizer = createLocalRecognizer(fakeApi(true), "es");
    expect(r.processLocally).toBe(true);
    expect(r.lang).toBe("es");
  });

  it("refuses where the browser did not keep the flag", () => {
    expect(() => createLocalRecognizer(fakeApi(false), "es")).toThrow();
  });

  it("treats a recognizer without processLocally or available() as absent", () => {
    class NoLocal {
      start(): void {}
    }
    expect(localRecognitionSupported(NoLocal as unknown as RecognitionStatic)).toBe(false);
    expect(localRecognitionSupported(null)).toBe(false);
    expect(browserTranscription(NoLocal as unknown as RecognitionStatic, () => {
      throw new Error("no audio graph should be built");
    }).present).toBe(false);
    expect(localRecognitionSupported(fakeApi(true))).toBe(true);
  });
});
