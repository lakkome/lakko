// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0100 section 1 — the worker that runs Lakko's own translation engine
 * (`lakko_translate.wasm`, Mozilla's Bergamot inference built by
 * `tools/translate-wasm`), one per page, single-threaded.
 *
 * It holds the engine and one direction (the page starts one worker per
 * direction: `engine.ts`, `newPort`). Everything it reads
 * arrives in a message: the engine binary and each direction's zstd model
 * files. A file is loaded only when its decompressed bytes hash to what the
 * manifest says; otherwise the load fails and nothing is translated. Output
 * is plain text (the engine's HTML mode is off).
 */

import loadLakkoTranslate, {
  type AlignedMemory,
  type BlockingService,
  type LakkoTranslateModule,
  type TranslationModel,
  type VectorResponse,
} from "../../../wasm-translate/lakko_translate.mjs";

import { ALIGNMENT, modelConfig, splitWhitespace, type WorkerRequest, type WorkerResponse } from "./protocol";

let engine: LakkoTranslateModule | null = null;
let service: BlockingService | null = null;
let loaded: TranslationModel | null = null;

function hex(buf: ArrayBuffer): string {
  return [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

async function init(wasm: ArrayBuffer): Promise<void> {
  if (engine !== null) return;
  engine = await loadLakkoTranslate({
    wasmBinary: wasm,
    INITIAL_MEMORY: 41_943_040,
    print: () => undefined,
    printErr: () => undefined,
  });
  service = new engine.BlockingService({ cacheSize: 0 });
}

async function load(request: Extract<WorkerRequest, { type: "load" }>): Promise<void> {
  const e = engine;
  if (e === null) throw new Error("engine not started");
  if (loaded !== null) throw new Error("a direction is already loaded");
  const memories: AlignedMemory[] = [];
  const byKind = new Map<string, AlignedMemory>();
  try {
    for (const f of request.files) {
      // Mozilla's frames state no size: the manifest's is required, and the
      // output must be exactly that long.
      const mem = e.zstdDecompressToAligned(new Uint8Array(f.zstd), ALIGNMENT[f.kind], f.decompressedBytes);
      memories.push(mem);
      const view = mem.getByteArrayView();
      if (view.byteLength !== f.decompressedBytes) throw new Error("model file size mismatch");
      // `digest` copies the bytes when it is called, before any await.
      // The engine has no threads, so its memory is never shared.
      const digest = hex(await crypto.subtle.digest("SHA-256", view as Uint8Array<ArrayBuffer>));
      if (digest !== f.decompressedSha256) throw new Error("model file hash mismatch");
      byKind.set(f.kind, mem);
    }
    const model = byKind.get("model");
    const lex = byKind.get("lex");
    if (model === undefined || lex === undefined) throw new Error("model files missing");
    const vocabs = new e.AlignedMemoryList();
    const shared = byKind.get("vocab");
    if (shared !== undefined) vocabs.push_back(shared);
    else {
      const src = byKind.get("srcvocab");
      const trg = byKind.get("trgvocab");
      if (src === undefined || trg === undefined) throw new Error("vocabulary missing");
      vocabs.push_back(src);
      vocabs.push_back(trg);
    }
    const modelName = request.files.find((f) => f.kind === "model")?.name ?? "";
    const built = new e.TranslationModel(request.source, request.target, modelConfig(modelName), model, lex, vocabs, null);
    vocabs.delete();
    loaded = built;
  } finally {
    // The model moved their contents out; the handles are empty either way.
    for (const m of memories) m.delete();
  }
}

function translate(text: string): string {
  const e = engine;
  const s = service;
  if (e === null || s === null) throw new Error("engine not started");
  const { lead, core, trail } = splitWhitespace(text);
  // A soft hyphen breaks Marian's tokens; Firefox removes it too.
  const clean = core.replaceAll("\u00ad", "");
  if (clean.length === 0) return text;
  const model = loaded;
  if (model === null) throw new Error("model not loaded");
  const messages = new e.VectorString();
  const options = new e.VectorResponseOptions();
  let responses: VectorResponse | null = null;
  try {
    messages.push_back(clean);
    options.push_back({ qualityScores: false, alignment: true, html: false });
    responses = s.translate(model, messages, options);
    const first0 = responses.get(0);
    if (first0 === undefined) throw new Error("no translation");
    try {
      return lead + first0.getTranslatedText() + trail;
    } finally {
      first0.delete();
    }
  } finally {
    messages.delete();
    options.delete();
    responses?.delete();
  }
}

function reply(message: WorkerResponse): void {
  (self as unknown as { postMessage(m: WorkerResponse): void }).postMessage(message);
}

self.onmessage = async (ev: MessageEvent<WorkerRequest>): Promise<void> => {
  const request = ev.data;
  try {
    switch (request.type) {
      case "init":
        await init(request.wasm);
        reply({ id: request.id, ok: true, value: null });
        return;
      case "load":
        await load(request);
        reply({ id: request.id, ok: true, value: null });
        return;
      case "translate":
        reply({ id: request.id, ok: true, value: translate(request.text) });
        return;
    }
  } catch (err) {
    // Only the error's name: a message could carry the text being translated.
    reply({ id: request.id, ok: false, error: err instanceof Error ? err.message.slice(0, 80) : "failed" });
  }
};
