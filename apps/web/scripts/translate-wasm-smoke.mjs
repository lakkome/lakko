// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Manual smoke test of apps/web/wasm-translate/lakko_translate.{mjs,wasm} in Node
 * (ADR-0100). Not part of vitest: it needs model files that are not in the
 * repository.
 *
 *   node scripts/translate-wasm-smoke.mjs --model M --lex L --vocab V [--src en --trg ru]
 *
 * M, L and V are Mozilla's zstd-compressed model files (`.zst`). A file that
 * is not zstd is compressed first with `zstd -19` (or Python's
 * compression.zstd) into a temporary directory, so decompressed files work
 * too. The script:
 *   1. checks the glue contains no `new Function` / `eval(` (the web
 *      client's CSP has no 'unsafe-eval');
 *   2. loads the module from bytes, as the worker will;
 *   3. checks the zstd entry points refuse bad input (garbage, a truncated
 *      frame, a frame without a content size, two frames, a bad alignment);
 *   4. decompresses the three files with zstdDecompressToAligned, builds the
 *      model and translates three sentences, printing them.
 *
 * Mozilla's files do not state their decompressed size in the frame; in the
 * web client it comes from the model manifest. Here it is measured with the
 * `zstd` CLI (or Python's compression.zstd) and passed as `expectedSize`.
 */

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const wasmDir = resolve(here, "..", "wasm-translate");
const MJS = join(wasmDir, "lakko_translate.mjs");
const WASM = join(wasmDir, "lakko_translate.wasm");

// Byte alignment per file type, as in Firefox's MODEL_FILE_ALIGNMENTS.
const ALIGN = { model: 256, lex: 64, vocab: 64 };
const ZSTD_MAGIC = [0x28, 0xb5, 0x2f, 0xfd];

const SENTENCES = [
  "Hello, how are you today?",
  "I will send you the photos from the trip tomorrow morning.",
  "The meeting has been moved to Thursday because the room is busy.",
];

function parseArgs(argv) {
  const out = { src: "en", trg: "ru" };
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i];
    const value = argv[i + 1];
    if (!key?.startsWith("--") || value === undefined) usage(`bad argument ${key ?? ""}`);
    out[key.slice(2)] = value;
  }
  for (const k of ["model", "lex", "vocab"]) if (!out[k]) usage(`--${k} is required`);
  return out;
}

function usage(reason) {
  console.error(`translate-wasm-smoke: ${reason}`);
  console.error("usage: node scripts/translate-wasm-smoke.mjs --model M --lex L --vocab V [--src en --trg ru]");
  process.exit(2);
}

function check(cond, what) {
  if (!cond) throw new Error(`FAIL: ${what}`);
  console.log(`OK   ${what}`);
}

function isZstd(bytes) {
  return bytes.length >= 4 && ZSTD_MAGIC.every((b, i) => bytes[i] === b);
}

/** Decompressed size and SHA-256 of a zstd file, measured outside the module. */
function decompressedInfo(zstdPath) {
  let plain;
  try {
    plain = execFileSync("zstd", ["-dc", zstdPath], { maxBuffer: 1024 * 1024 * 1024 });
  } catch {
    plain = execFileSync(
      "python3",
      ["-I", "-c", "import sys, compression.zstd as z; sys.stdout.buffer.write(z.decompress(open(sys.argv[1],'rb').read()))", zstdPath],
      { maxBuffer: 1024 * 1024 * 1024 },
    );
  }
  return { size: plain.length, sha256: createHash("sha256").update(plain).digest("hex") };
}

function sha256Of(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

/** Returns zstd bytes for `path`, compressing a raw file at level 19. */
function zstdBytes(path, scratch) {
  const raw = readFileSync(path);
  if (isZstd(raw)) return { bytes: new Uint8Array(raw.buffer, raw.byteOffset, raw.byteLength), path };
  const out = join(scratch, `${basename(path)}.zst`);
  try {
    execFileSync("zstd", ["-19", "-q", "-f", "-o", out, path], { stdio: "inherit" });
  } catch {
    execFileSync(
      "python3",
      [
        "-I",
        "-c",
        "import sys, compression.zstd as z; open(sys.argv[2],'wb').write(z.compress(open(sys.argv[1],'rb').read(), level=19))",
        path,
        out,
      ],
      { stdio: "inherit" },
    );
  }
  console.log(`note: ${basename(path)} was not zstd; compressed to ${out}`);
  const z = readFileSync(out);
  return { bytes: new Uint8Array(z.buffer, z.byteOffset, z.byteLength), path: out };
}

/** A valid zstd frame that does not state its content size (one raw block). */
function frameWithoutContentSize(payload) {
  const n = payload.length;
  const header = [
    ...ZSTD_MAGIC,
    0x00, // descriptor: no content size, not single-segment, no checksum, no dictionary
    0x00, // window descriptor: 1 KiB window
    // block header: last block, raw, size n
    ((n << 3) | 1) & 0xff,
    ((n << 3) >> 8) & 0xff,
    ((n << 3) >> 16) & 0xff,
  ];
  return Uint8Array.from([...header, ...payload]);
}

function expectThrow(fn, what) {
  let threw = false;
  try {
    const result = fn();
    result?.delete?.();
  } catch (e) {
    threw = true;
    console.log(`     (${String(e?.message ?? e)})`);
  }
  check(threw, what);
}

function configYaml(gemmPrecision) {
  const cfg = {
    "beam-size": "1",
    normalize: "1.0",
    "word-penalty": "0",
    "max-length-break": "128",
    "mini-batch-words": "1024",
    workspace: "128",
    "max-length-factor": "2.0",
    "skip-cost": "true",
    "cpu-threads": "0",
    quiet: "true",
    "quiet-translation": "true",
    "gemm-precision": gemmPrecision,
    alignment: "soft",
  };
  let s = "\n";
  for (const [k, v] of Object.entries(cfg)) s += `            ${k}: ${v}\n`;
  return s + "            ";
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const scratch = mkdtempSync(join(tmpdir(), "lakko-translate-smoke-"));
  try {
    // 1. CSP: no dynamic code in the glue.
    const glue = readFileSync(MJS, "utf8");
    check(!/new Function/.test(glue), "glue contains no `new Function`");
    check(!/(^|[^A-Za-z0-9_$.])eval\(/m.test(glue), "glue contains no `eval(`");

    // 2. Load from bytes. The module is built for a worker only
    // (-sENVIRONMENT=worker) and reads `self.location` at start; Node has no
    // `self`, so a stand-in with the module's own URL is provided.
    if (typeof globalThis.self === "undefined") {
      globalThis.self = { location: { href: pathToFileURL(MJS).href } };
    }
    const { default: loadLakkoTranslate } = await import(pathToFileURL(MJS).href);
    const wasmBinary = readFileSync(WASM);
    const t0 = performance.now();
    const m = await loadLakkoTranslate({
      wasmBinary,
      print: () => {},
      printErr: (s) => console.error(`[engine] ${s}`),
      onAbort: (w) => console.error(`[engine abort] ${w}`),
    });
    console.log(`module loaded in ${(performance.now() - t0).toFixed(0)} ms, memory ${m.HEAPU8.byteLength} bytes`);
    check(typeof m.zstdDecompressToAligned === "function", "zstdDecompressToAligned is exported");
    check(typeof m.zstdFrameContentSize === "function", "zstdFrameContentSize is exported");

    // 3. Refusals.
    const garbage = Uint8Array.from({ length: 64 }, (_, i) => (i * 37 + 11) & 0xff);
    check(m.zstdFrameContentSize(garbage) === -1, "content size of garbage is -1");
    check(m.zstdFrameContentSize(new Uint8Array(0)) === -1, "content size of empty input is -1");
    const noSize = frameWithoutContentSize(new TextEncoder().encode("no size here"));
    check(m.zstdFrameContentSize(noSize) === -1, "content size of a frame without one is -1");
    expectThrow(() => m.zstdDecompressToAligned(garbage, 64), "garbage is refused");
    expectThrow(() => m.zstdDecompressToAligned(noSize, 64), "a frame without a content size is refused");

    const files = {};
    for (const kind of ["model", "lex", "vocab"]) {
      const z = zstdBytes(args[kind], scratch);
      files[kind] = { bytes: z.bytes, ...decompressedInfo(z.path) };
      const stated = m.zstdFrameContentSize(z.bytes);
      console.log(`${kind}: ${z.bytes.length} bytes zstd, ${files[kind].size} decompressed, frame states ${stated === -1 ? "no size" : stated}`);
    }
    const vocabZ = files.vocab.bytes;
    const vocabSize = files.vocab.size;
    if (m.zstdFrameContentSize(vocabZ) === -1) {
      expectThrow(() => m.zstdDecompressToAligned(vocabZ, 64), "a frame without a size and no expectedSize is refused");
    }
    expectThrow(() => m.zstdDecompressToAligned(vocabZ, 64, vocabSize - 1), "an expectedSize one byte short is refused");
    expectThrow(() => m.zstdDecompressToAligned(vocabZ, 64, vocabSize + 1), "an expectedSize one byte long is refused");
    expectThrow(() => m.zstdDecompressToAligned(vocabZ.subarray(0, vocabZ.length - 7), 64, vocabSize), "a truncated frame is refused");
    const twice = new Uint8Array(vocabZ.length * 2);
    twice.set(vocabZ, 0);
    twice.set(vocabZ, vocabZ.length);
    expectThrow(() => m.zstdDecompressToAligned(twice, 64, vocabSize), "two frames are refused");
    expectThrow(() => m.zstdDecompressToAligned(vocabZ, 48, vocabSize), "an alignment that is not a power of two is refused");
    expectThrow(() => m.zstdDecompressToAligned(Array.from(vocabZ), 64, vocabSize), "a non-Uint8Array source is refused");
    expectThrow(() => m.zstdDecompressToAligned(vocabZ, 64, 600 * 1024 * 1024), "an expectedSize over 512 MiB is refused");

    // 4. Decompress, build, translate.
    const t1 = performance.now();
    const model = m.zstdDecompressToAligned(files.model.bytes, ALIGN.model, files.model.size);
    const lex = m.zstdDecompressToAligned(files.lex.bytes, ALIGN.lex, files.lex.size);
    const vocab = m.zstdDecompressToAligned(vocabZ, ALIGN.vocab, vocabSize);
    console.log(`decompressed in ${(performance.now() - t1).toFixed(0)} ms: model ${model.size()}, lex ${lex.size()}, vocab ${vocab.size()} bytes`);
    check(vocab.size() === vocabSize, "vocab size equals the expected size");
    check(model.size() === files.model.size && lex.size() === files.lex.size, "model and lex sizes equal the expected sizes");
    for (const [kind, mem] of [["model", model], ["lex", lex], ["vocab", vocab]]) {
      check(sha256Of(mem.getByteArrayView()) === files[kind].sha256, `${kind} bytes equal the zstd CLI's output (SHA-256)`);
    }
    check(m.HEAPU8 instanceof Uint8Array, "HEAPU8 is exported");

    const vocabs = new m.AlignedMemoryList();
    vocabs.push_back(vocab);
    const precision = args.model.includes("intgemm8") ? "int8shiftAll" : "int8shiftAlphaAll";
    const t2 = performance.now();
    const translationModel = new m.TranslationModel(args.src, args.trg, configYaml(precision), model, lex, vocabs, null);
    const service = new m.BlockingService({ cacheSize: 0 });
    console.log(`model built in ${(performance.now() - t2).toFixed(0)} ms (gemm-precision ${precision})`);

    for (const sentence of SENTENCES) {
      const messages = new m.VectorString();
      const options = new m.VectorResponseOptions();
      messages.push_back(sentence);
      options.push_back({ qualityScores: false, alignment: true, html: false });
      const t = performance.now();
      const responses = service.translate(translationModel, messages, options);
      const response = responses.get(0);
      const text = response.getTranslatedText();
      console.log(`${args.src}: ${sentence}`);
      console.log(`${args.trg}: ${text}   (${(performance.now() - t).toFixed(0)} ms)`);
      check(text.length > 0, "translation is not empty");
      response.delete();
      responses.delete();
      messages.delete();
      options.delete();
    }

    service.delete();
    translationModel.delete();
    vocabs.delete();
    for (const mem of [model, lex, vocab]) mem.delete();
    console.log(`memory after the run: ${m.HEAPU8.byteLength} bytes`);
    console.log("SMOKE OK");
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error(e?.stack ?? String(e));
  process.exit(1);
});

