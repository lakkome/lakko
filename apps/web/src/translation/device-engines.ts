// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The engines this client runs with, in the order of ADR-0082 (note of
 * 2026-11-18): the operating system's own on-device engine where the desktop
 * host offers one, then the browser's. Lakko's own on-device engine, when it
 * exists, goes last in the list; nothing else changes.
 *
 * Detection follows the same order and ends with the core detector, which
 * always answers (`browser-engines.ts`, `coreDetection`).
 */

import {
  browserTranscription,
  browserTranslation,
  coreDetection,
  layeredDetection,
} from "./browser-engines";
import { enginesInOrder } from "./engine-order";
import type { Engines } from "./engines";
import { lakkoDeviceTranslation } from "./lakko-engine/device";
import { hostEngines } from "../platform/host-engines";

export function deviceEngines(): Engines {
  const core = coreDetection();
  const browser: Engines = {
    translation: browserTranslation(),
    detection: layeredDetection(undefined, core),
    transcription: browserTranscription(),
  };
  const lakko: Engines = {
    translation: lakkoDeviceTranslation(),
    detection: { present: false, detect: () => Promise.resolve(null) },
    transcription: {
      present: false,
      state: () => Promise.resolve("unavailable"),
      install: () => Promise.resolve(false),
      transcribe: () => Promise.reject(new Error("no on-device recognizer")),
    },
  };
  const host = hostEngines(core.rank);
  return enginesInOrder(host === null ? [browser, lakko] : [host, browser, lakko]);
}
