// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 6 — one audio or voice message plays at a
 * time: starting one pauses the one playing before.
 */

/** The smallest surface this needs from a media element. */
export interface Pausable {
  pause(): void;
  readonly paused: boolean;
}

let current: Pausable | null = null;

/** Call when `el` starts playing: the previously claimed element is paused. */
export function claimPlayback(el: Pausable): void {
  if (current !== null && current !== el && !current.paused) current.pause();
  current = el;
}

/** Call when `el` goes away, so it is not held. */
export function releasePlayback(el: Pausable): void {
  if (current === el) current = null;
}
