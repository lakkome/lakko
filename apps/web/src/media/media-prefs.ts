// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 — per-device media preferences, as app-wide reactive signals.
 *
 * All of these are conveniences a user can lose without harm (private
 * window, cleared site data), so each read and write tolerates a missing or
 * throwing `localStorage` and falls back to the spec's default.
 *
 *   * the composer's record mode, mic or camera (UX spec §1, "remembered per
 *     device");
 *   * "Autoplay video notes", default OFF (§3 — autoplay downloads media);
 *   * the GIF privacy note's "Got it" (§5, "remembered per device");
 *   * whether this server's GIF proxy answered "not configured" — session
 *     only, so a server that gains a key shows the tab again after a reload.
 */

import { createSignal } from "solid-js";
import type { CaptureKind } from "./recording-machine";
import { GIF_PRIVACY_NOTE_STORAGE_KEY } from "./gif-client";

const MODE_KEY = "lakko.composer.recordMode";
const AUTOPLAY_KEY = "lakko.media.autoplayVideoNotes";

function read(key: string): string | null {
  try {
    return globalThis.localStorage?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

function write(key: string, value: string): void {
  try {
    globalThis.localStorage?.setItem(key, value);
  } catch {
    // A convenience; the in-memory signal still holds the choice.
  }
}

const [recordMode, setRecordModeSignal] = createSignal<CaptureKind>(
  read(MODE_KEY) === "video_note" ? "video_note" : "voice",
);
export { recordMode };
export function setRecordMode(mode: CaptureKind): void {
  setRecordModeSignal(mode);
  write(MODE_KEY, mode);
}

const [autoplayVideoNotes, setAutoplaySignal] = createSignal(read(AUTOPLAY_KEY) === "1");
export { autoplayVideoNotes };
export function setAutoplayVideoNotes(on: boolean): void {
  setAutoplaySignal(on);
  write(AUTOPLAY_KEY, on ? "1" : "0");
}

const [gifNoteDismissed, setGifNoteSignal] = createSignal(
  read(GIF_PRIVACY_NOTE_STORAGE_KEY) === "1",
);
export { gifNoteDismissed };
export function dismissGifNote(): void {
  setGifNoteSignal(true);
  write(GIF_PRIVACY_NOTE_STORAGE_KEY, "1");
}

const [gifProviderMissing, setGifProviderMissing] = createSignal(false);
export { gifProviderMissing, setGifProviderMissing };
