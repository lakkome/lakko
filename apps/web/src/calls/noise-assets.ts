// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The two files noise suppression loads at run time (ADR-0088): the worklet
 * module and `lakko_noise.wasm` (built by `npm run build:noise`).
 *
 * Kept in a module of its own and imported dynamically by
 * `noise-suppression.ts`, so a test that imports a call session never has
 * to resolve a worker bundle or a `.wasm` that a test checkout may not have
 * built.
 */

import noiseWorkletUrl from "./noise-worklet.ts?worker&url";
import noiseWasmUrl from "../../wasm/lakko_noise.wasm?url";

export { noiseWorkletUrl, noiseWasmUrl };
