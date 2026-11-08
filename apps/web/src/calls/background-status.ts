// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the call screen shows of the camera background (UX
 * `docs/ux/call-backgrounds.md` section 3): the live call's background state
 * and the camera track it sends (the Settings sheet's preview plays it), plus
 * the one event the call host acts on, a failure.
 *
 * Written by the live call session's background controller only (one call at
 * a time); the Settings sheet's own camera-off preview keeps its state to
 * itself.
 */

import { createSignal } from "solid-js";

/** `off`: no background is applied (none selected, or no camera). */
export type CameraBackgroundState = "off" | "starting" | "working" | "failed";

const [state, setState] = createSignal<CameraBackgroundState>("off");
const [sentCamera, setSentCamera] = createSignal<MediaStreamTrack | null>(null);
const failureListeners = new Set<() => void>();

/** The live call's background state. */
export const cameraBackgroundState = state;

/** The camera track the live call sends now (processed or raw), or `null`. */
export const sentCameraTrack = sentCamera;

/**
 * Whether a view of this person's own camera shows the avatar with a small
 * spinner instead of the picture (UX section 3, Starting row): the background
 * is starting, so nothing is sent and there is no picture to show yet. Every
 * own-camera view asks this one rule: the own tile, the one-to-one self view
 * and the full-screen own camera. While the camera itself is still starting,
 * that state's own overlay is shown instead.
 */
export function ownCameraPreparing(background: CameraBackgroundState, cameraStarting: boolean): boolean {
  return background === "starting" && !cameraStarting;
}

/** Called by the live call's controller. */
export function publishCameraBackground(next: CameraBackgroundState, track: MediaStreamTrack | null): void {
  setState(next);
  setSentCamera(track);
}

/** Tells the call host the background failed: it turns the camera off, as
 *  if the person had pressed the button, and shows the line. */
export function reportCameraBackgroundFailure(): void {
  for (const listener of [...failureListeners]) {
    try {
      listener();
    } catch {
      // One subscriber's failure must not stop the others.
    }
  }
}

/** Subscribes to failures; returns the unsubscribe function. */
export function onCameraBackgroundFailure(listener: () => void): () => void {
  failureListeners.add(listener);
  return () => {
    failureListeners.delete(listener);
  };
}
