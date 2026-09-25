// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The harness surface, declared for the specs.
 *
 * `harness/cases.tsx` declares the same thing for the page's own graph; a spec
 * body is transpiled separately by Playwright and never sees that file, so the
 * declaration is repeated rather than imported. Kept to the two members a spec
 * actually reads, so a widening of the harness has to be declared deliberately.
 */

interface LakkoCanvasFacts {
  readonly width: number;
  readonly height: number;
  readonly mean: number;
  readonly max: number;
}

interface LakkoHarness {
  /** Every `Audio` the page constructed, newest last. */
  readonly audios: HTMLAudioElement[];
  /** Takes the composer sent, in order. */
  readonly sent: Array<{ durationMs: number; blob: Blob; mime: string }>;
  /** How many times the page has asked for a microphone or camera. */
  gumCalls(): number;
  /** How many of those asks have been answered with a stream. */
  gumAnswers(): number;
  /** How many times a `MediaRecorder` has been started. */
  recorderStarts(): number;
  /** The canvas the last started recorder was handed, read at that instant;
   *  `null` when it was not recording a canvas. */
  canvasAtRecorderStart(): LakkoCanvasFacts | null;
  /** Run the retry a person starts from `Not sent · Retry`, then land it. */
  retryUpload?: () => void;
  completeUpload?: () => void;
  /** The live recorder the composer case mounts. */
  recorder?: {
    state: () => { phase: string };
    elapsedMs: () => number;
    mode: () => string;
    preview: () => { result: { durationMs: number; blob: Blob } } | null;
  };
  decode(blob: Blob): Promise<{ durationMs: number; peakInFirstMs: number }>;
  decodeVideo(blob: Blob): Promise<{
    durationMs: number;
    firstFrameMs: number;
    lastFrameMs: number;
    firstFrame: LakkoCanvasFacts;
  }>;
}

interface Window {
  __lakkoHarness: LakkoHarness;
}
