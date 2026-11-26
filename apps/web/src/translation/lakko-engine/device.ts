// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0100 — Lakko's own engine as this page runs it: the manifest shipped in
 * the build, the worker, and the downloader of `../../translation-models/`.
 *
 * Offered only where this browser can run it and where it is wanted: never
 * in a phone or tablet browser, whose person has the Lakko app and its
 * system engines (ADR-0100 open question 4), and never without wasm SIMD.
 */

import {
  downloadGroup,
  fetchEngineBinary,
  readSlice,
  removeGroup,
  storedGroups,
} from "../../translation-models/models-store";
import type { TranslationEngine } from "../engines";
import { type EnginePort, lakkoTranslation } from "./engine";
import { readManifest } from "./manifest";
import rawManifest from "./models.json";
import type { WorkerRequest, WorkerResponse } from "./protocol";

/** wasm-feature-detect's SIMD probe: one function returning a v128
 *  (`i32.const 0`, `i8x16.splat`, `i8x16.popcnt`); `validate` answers
 *  without running it. */
const SIMD_PROBE = new Uint8Array([
  0, 97, 115, 109, 1, 0, 0, 0, 1, 5, 1, 96, 0, 1, 123, 3, 2, 1, 0, 10, 10, 1, 8, 0, 65, 0, 253, 15, 253, 98, 11,
]);

/** Whether this browser runs wasm with SIMD. */
export function wasmSimdSupported(): boolean {
  try {
    return typeof WebAssembly === "object" && WebAssembly.validate(SIMD_PROBE);
  } catch {
    return false;
  }
}

/**
 * Whether this page runs in a phone or tablet browser: the browser's own
 * answer where it gives one (`navigator.userAgentData.mobile`), else the user
 * agent string; an iPad that reports a Mac is told by its touch points.
 */
export function isPhoneOrTabletBrowser(nav: {
  readonly userAgent?: string;
  readonly maxTouchPoints?: number;
  readonly userAgentData?: { readonly mobile?: boolean };
} = globalThis.navigator ?? {}): boolean {
  if (typeof nav.userAgentData?.mobile === "boolean" && nav.userAgentData.mobile) return true;
  const ua = nav.userAgent ?? "";
  if (/Android|iPhone|iPad|iPod|Mobile|Silk|Kindle/i.test(ua)) return true;
  return /Macintosh/.test(ua) && (nav.maxTouchPoints ?? 0) > 1;
}

/** A new translation worker (one per loaded direction, `engine.ts`). */
function workerPort(): EnginePort {
  const worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
  let next = 1;
  let failed: Error | null = null;
  const pending = new Map<number, { resolve: (v: string | null) => void; reject: (e: Error) => void }>();
  const fail = (err: Error): void => {
    failed = err;
    for (const h of pending.values()) h.reject(err);
    pending.clear();
  };
  worker.onmessage = (ev: MessageEvent<WorkerResponse>) => {
    const r = ev.data;
    const h = pending.get(r.id);
    if (h === undefined) return;
    pending.delete(r.id);
    if (r.ok) h.resolve(r.value);
    else h.reject(new Error(r.error));
  };
  worker.onerror = () => {
    fail(new Error("translation worker failed"));
    worker.terminate();
  };
  return {
    call(request, transfer = []) {
      if (failed !== null) return Promise.reject(failed);
      const id = next++;
      return new Promise<string | null>((resolve, reject) => {
        pending.set(id, { resolve, reject });
        worker.postMessage({ id, ...request } as WorkerRequest, transfer);
      });
    },
    terminate() {
      worker.terminate();
      fail(new Error("translation worker stopped"));
    },
  };
}

/** Lakko's own translation engine for this page. */
export function lakkoDeviceTranslation(): TranslationEngine {
  const supported =
    typeof Worker === "function" &&
    typeof indexedDB !== "undefined" &&
    typeof crypto?.subtle?.digest === "function" &&
    wasmSimdSupported() &&
    !isPhoneOrTabletBrowser();
  return lakkoTranslation({
    manifest: readManifest(rawManifest),
    supported,
    storedGroups,
    readSlice,
    downloadGroup: (group, bytes, slices, options) => downloadGroup(group, bytes, slices, options),
    removeGroup,
    engineBinary: () => fetchEngineBinary(),
    newPort: workerPort,
  });
}
