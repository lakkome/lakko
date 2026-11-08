// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The two files a camera background loads at run time (ADR-0089): the worker
 * module and `lakko_segment.wasm` (built by `npm run build:segment`).
 *
 * In a module of its own, imported dynamically by `background-stage.ts`, so a
 * test that imports a call session never has to resolve a worker bundle or a
 * `.wasm` a test checkout may not have built; and so the 19 MB module is
 * fetched only when a background is first selected (the UX's `Preparing
 * background...`).
 */

import segmentWasmUrl from "../../wasm/lakko_segment.wasm?url";

/** A new background worker. */
export function createBackgroundWorker(): Worker {
  return new Worker(new URL("./background-worker.ts", import.meta.url), {
    type: "module",
    name: "lakko-background",
  });
}

export { segmentWasmUrl };
