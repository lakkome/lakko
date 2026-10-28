// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Seeing a microphone or camera capture start while a transcript is made
 * (ADR-0082 section 2, the guard Android carries).
 *
 * The risk this exists for: the transcript feeds the recognizer a track
 * built from the recording (`start(track)`). A browser that has
 * `processLocally` but not that overload would drop the argument (WebIDL
 * ignores an argument no overload takes) and listen to the microphone. No
 * probe tells the two apart beforehand without risking exactly that, and the
 * browser's own capture for recognition is not visible to the page.
 *
 * What IS observable, and what the transcription engine acts on:
 * 1. every capture this app starts goes through
 *    `navigator.mediaDevices.getUserMedia` (the voice recorder, video notes,
 *    calls). {@link watchCaptures} wraps that one function, so a capture that
 *    starts during a transcription is seen;
 * 2. the recognizer's own `not-allowed` / `audio-capture` error, which a
 *    session reading a supplied track has no reason to raise (the draft says
 *    `start(track)` runs "without requesting microphone permission").
 *
 * Either one stops the transcription and turns transcription off for good on
 * this device. Neither one catches the case it cannot see: a browser that
 * already holds microphone permission for this site, lacks the overload, and
 * silently listens to the room for the length of the message.
 */

type Listener = () => void;

const listeners = new Set<Listener>();
let installed = false;

/** Wrap `getUserMedia` once so a capture start is reported. Idempotent. */
export function watchCaptures(): void {
  if (installed) return;
  const devices = (globalThis.navigator as { mediaDevices?: MediaDevices } | undefined)?.mediaDevices;
  if (devices === undefined || typeof devices.getUserMedia !== "function") return;
  installed = true;
  const original = devices.getUserMedia.bind(devices);
  devices.getUserMedia = (constraints?: MediaStreamConstraints): Promise<MediaStream> => {
    for (const l of [...listeners]) {
      try {
        l();
      } catch {
        // A listener never stops the capture the person asked for.
      }
    }
    return original(constraints);
  };
}

/** Call `listener` on every capture start until the returned function runs. */
export function onCaptureStart(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}
