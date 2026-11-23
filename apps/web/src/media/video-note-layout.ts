// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` section 3, "Timeline bubble" (amended
 * 2026-11-22) — what sits on and beside a video-note circle.
 *
 * Seen on a real iPhone and a real Pixel: the duration, the time with its
 * ticks and the `Aa` control stood under the circle on two or three loose
 * lines and read as text floating under a video. The note is now drawn like a
 * photo: the duration on a pill inside the circle at the bottom leading
 * corner, the time and ticks on a pill at the bottom trailing corner, and
 * `Aa` beside the circle on the side away from the screen edge.
 *
 * The same numbers are written in the iOS and Android bubbles; they are kept
 * here so a test can hold them to the one property that matters: both pills
 * lie wholly inside the circle at any diameter.
 */

/** Height of the pills' bottom edge above the circle's bottom, as a fraction
 *  of the diameter. */
export const VIDEO_NOTE_PILL_BOTTOM = 0.14;
/** Distance of the pills' outer edge from the circle's side, as a fraction of
 *  the diameter. */
export const VIDEO_NOTE_PILL_INSET = 0.16;
/** Gap between the circle and the `Aa` control beside it, in px. */
export const VIDEO_NOTE_ASIDE_GAP = 6;

/**
 * Whether the outer bottom corner of a pill placed by the two fractions above
 * lies inside a circle of `diameter`. The placement scales with the diameter,
 * so a true answer at one diameter is a true answer at every diameter; the
 * function takes it anyway so a test can say so.
 */
export function videoNotePillCornerInside(diameter: number): boolean {
  const r = diameter / 2;
  const dx = r - VIDEO_NOTE_PILL_INSET * diameter;
  const dy = r - VIDEO_NOTE_PILL_BOTTOM * diameter;
  return dx * dx + dy * dy < r * r;
}

/** `Aa` sits on the side away from the screen edge: after the circle on an
 *  incoming note (drawn at the start edge), before it on an own note. */
export function videoNoteAsideFirst(incoming: boolean): boolean {
  return !incoming;
}

/**
 * What the duration pill says: the elapsed time while the note is the one
 * playing with sound (paused part-way included), the total otherwise, the
 * sound hint when a muted loop was refused sound, or nothing when there is no
 * duration to show.
 */
export type VideoNoteDurationLabel =
  | { readonly kind: "elapsed"; readonly ms: number }
  | { readonly kind: "total"; readonly ms: number }
  | { readonly kind: "sound-hint" }
  | null;

export function videoNoteDurationLabel(args: {
  readonly mode: "poster" | "muted" | "sound";
  readonly soundHint: boolean;
  readonly positionMs: number;
  readonly durationMs: number | undefined;
}): VideoNoteDurationLabel {
  if (args.soundHint && args.mode === "muted") return { kind: "sound-hint" };
  if (args.mode === "sound") return { kind: "elapsed", ms: Math.max(0, args.positionMs) };
  if (args.durationMs === undefined) return null;
  return { kind: "total", ms: args.durationMs };
}
