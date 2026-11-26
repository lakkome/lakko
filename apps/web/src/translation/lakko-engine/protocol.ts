// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0100 section 1 — the messages between the page and the translation
 * worker. The page hands the worker bytes it already holds (the engine
 * binary, a direction's zstd model files); the worker reaches no network.
 */

import type { ModelFileKind } from "./manifest";

/** One zstd model file and the hash its decompressed bytes must have. */
export interface WorkerModelFile {
  readonly kind: ModelFileKind;
  /** The record name; `…intgemm8.bin` selects the int8 shift-all precision. */
  readonly name: string;
  readonly zstd: ArrayBuffer;
  readonly decompressedSha256: string;
  readonly decompressedBytes: number;
}

export type WorkerRequest =
  | { readonly id: number; readonly type: "init"; readonly wasm: ArrayBuffer }
  | {
      readonly id: number;
      readonly type: "load";
      /** The direction key (`es-en`). */
      readonly key: string;
      readonly source: string;
      readonly target: string;
      readonly files: readonly WorkerModelFile[];
    }
  | { readonly id: number; readonly type: "translate"; readonly text: string };

export type WorkerResponse =
  | { readonly id: number; readonly ok: true; readonly value: string | null }
  | { readonly id: number; readonly ok: false; readonly error: string };

/** Byte alignment per file kind, as in Firefox's MODEL_FILE_ALIGNMENTS. */
export const ALIGNMENT: Readonly<Record<ModelFileKind, number>> = {
  model: 256,
  lex: 64,
  vocab: 64,
  srcvocab: 64,
  trgvocab: 64,
};

/**
 * Marian's options, as Firefox's translations engine worker sets them
 * (toolkit/components/translations/content/translations-engine.worker.js),
 * as the YAML text `TranslationModel` takes.
 */
export function modelConfig(modelName: string): string {
  const cfg: [string, string][] = [
    ["beam-size", "1"],
    ["normalize", "1.0"],
    ["word-penalty", "0"],
    ["max-length-break", "128"],
    ["mini-batch-words", "1024"],
    ["workspace", "128"],
    ["max-length-factor", "2.0"],
    ["skip-cost", "true"],
    ["cpu-threads", "0"],
    ["quiet", "true"],
    ["quiet-translation", "true"],
    ["gemm-precision", modelName.endsWith("intgemm8.bin") ? "int8shiftAll" : "int8shiftAlphaAll"],
    ["alignment", "soft"],
  ];
  const indent = "            ";
  return "\n" + cfg.map(([k, v]) => `${indent}${k}: ${v}\n`).join("") + indent;
}

/** Leading and trailing white space, kept around a translation. */
export function splitWhitespace(text: string): { lead: string; core: string; trail: string } {
  const m = /^(\s*)([\s\S]*?)(\s*)$/.exec(text);
  return { lead: m?.[1] ?? "", core: m?.[2] ?? "", trail: m?.[3] ?? "" };
}
