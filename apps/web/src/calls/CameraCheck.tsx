// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The camera check, shown before a video call and only before a video call
 * (`docs/ux/calls.md` §3.2).
 *
 * ## What it replaces
 *
 * A pre-join card headed "Ready to join?" with "Check your camera and
 * microphone first." under it, a bordered box holding a camera **glyph** where
 * a picture was not, two toggles, and a Join button — shown for a voice call
 * too. §3.1 and §4.1 between them retire every part of that: a voice call asks
 * nothing, and "a switch says 'this is a preference you are configuring'. A
 * call needs a button you press."
 *
 * ## What it is instead
 *
 * "It fills the screen with the live self-view, the callee's name across the
 * top, and three round controls above the bottom edge: microphone, camera, and
 * a large Call button between them. Cancel is a plain close control in the top
 * corner."
 *
 * Two things follow from §3.2 that are easy to get wrong:
 *
 * - **Turning the camera off must not leave a hole.** The preview becomes the
 *   callee's avatar on the call background — the same portrait the call itself
 *   draws — so the screen never shows the empty box this change exists to
 *   remove. The button still reads `Call`.
 * - **The camera is released while it is off.** A preview that keeps the
 *   capture running to avoid a re-acquire would leave the recording light on
 *   over a screen that says the camera is off, which is a lie told in hardware.
 *
 * The screen never appears at all when there is no camera: the caller probes
 * first and `planCallStart` (`calls/call-start-flow.ts`) turns that into a
 * voice call with one line of explanation. A check that opened and then
 * discovered it had nothing to show would put the empty box straight back.
 *
 * The decisions — what the buttons say, which state is the loud one, what the
 * Call button places — are in `calls/call-start-flow.ts`, where a test can
 * reach them.
 */

import { createEffect, createSignal, onCleanup, onMount, Show, type Component } from "solid-js";

import {
  cameraCheckCallAccessibleName,
  cameraCheckCallLabel,
  cameraCheckControls,
  cameraCheckResult,
  type CameraCheckControl,
} from "./call-start-flow";
import type { CallMediaChoice } from "./call-media-choice";
import { Icon } from "../ui/icons";
import { initialsOf, toneOf } from "../ui/Avatar";

/** One round toggle, the same two states as the in-call row (§4.1.1). */
const CheckButton: Component<{ control: CameraCheckControl; onPress: () => void }> = (props) => (
  <div class="lk-call-ctl">
    <button
      type="button"
      class="lk-call-ctl-btn"
      data-tone={props.control.tone}
      aria-label={props.control.accessibleName}
      aria-pressed={props.control.pressed}
      title={props.control.accessibleName}
      onClick={(): void => props.onPress()}
    >
      <Icon name={props.control.icon} size={26} />
    </button>
    <span class="lk-call-ctl-label" aria-hidden="true">
      {props.control.label}
    </span>
  </div>
);

export const CameraCheck: Component<{
  /** The person being called, for the line across the top and for the avatar
   *  the preview falls back to. */
  peerName: string;
  /** What the avatar's colour is hashed from, so one person is one colour
   *  everywhere in the app. */
  peerId?: string | null;
  /** The camera the caller already opened to find out there was one. This
   *  screen owns it from here: it stops the tracks when the camera is switched
   *  off, when the call is placed, and when the screen closes. */
  initialStream: MediaStream;
  onCancel: () => void;
  onCall: (media: CallMediaChoice, micOn: boolean) => void;
}> = (props) => {
  const [camOn, setCamOn] = createSignal(true);
  const [micOn, setMicOn] = createSignal(true);
  const [stream, setStream] = createSignal<MediaStream | null>(props.initialStream);
  const [busy, setBusy] = createSignal(false);
  let callButton: HTMLButtonElement | undefined;

  const stop = (): void => {
    const s = stream();
    if (s === null) return;
    for (const track of s.getTracks()) track.stop();
    setStream(null);
  };

  onCleanup(stop);

  // The obvious, safe thing to press is what receives focus, and Escape is the
  // conventional way out of a screen you did not mean to open.
  onMount(() => {
    callButton?.focus();
  });
  createEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      stop();
      props.onCancel();
    };
    document.addEventListener("keydown", onKeyDown);
    onCleanup(() => document.removeEventListener("keydown", onKeyDown));
  });

  const toggleCamera = (): void => {
    if (camOn()) {
      setCamOn(false);
      stop();
      return;
    }
    setCamOn(true);
    if (busy()) return;
    setBusy(true);
    void navigator.mediaDevices
      .getUserMedia({ video: true })
      .then((s) => setStream(s))
      .catch((err: unknown) => {
        // The camera was there a moment ago and is not now — another app took
        // it, or the user revoked the permission. The call is still placeable,
        // so the screen falls back to the portrait rather than failing.
        // eslint-disable-next-line no-console
        console.warn("camera check: could not restart the camera", err);
        setCamOn(false);
      })
      .finally(() => setBusy(false));
  };

  const controls = (): ReturnType<typeof cameraCheckControls> =>
    cameraCheckControls({ micOn: micOn(), camOn: camOn() });

  const place = (): void => {
    const result = cameraCheckResult({ micOn: micOn(), camOn: camOn() });
    // Hand the camera back before the session asks the OS for its own.
    stop();
    props.onCall(result.media, result.micOn);
  };

  return (
    <div
      class="lk-call lk-camera-check"
      data-surface="video"
      role="dialog"
      aria-modal="true"
      aria-label={`Camera check before calling ${props.peerName}`}
    >
      {/* Camera off is the callee's portrait on the call background — never an
          empty box (§3.2). Same colour the call screen and the chat list give
          this person. */}
      <Show
        when={stream()}
        fallback={
          <div
            class="lk-call-portrait"
            data-tone={toneOf(props.peerId ?? props.peerName)}
            aria-hidden="true"
          >
            <div class="lk-call-portrait-wash">
              <span class="lk-call-portrait-blur">{initialsOf(props.peerName)}</span>
            </div>
            <div class="lk-call-portrait-avatar">{initialsOf(props.peerName)}</div>
          </div>
        }
      >
        {(s) => (
          <video
            class="lk-call-fill mirrored"
            autoplay
            playsinline
            muted
            aria-label="Your camera"
            ref={(el) => {
              el.srcObject = s();
            }}
          />
        )}
      </Show>

      <button
        type="button"
        class="lk-camera-check-close"
        aria-label="Cancel"
        title="Cancel"
        onClick={(): void => {
          stop();
          props.onCancel();
        }}
      >
        <Icon name="close" size={22} />
      </button>

      <div class="lk-camera-check-who">
        <h1>{props.peerName}</h1>
      </div>

      <div class="lk-call-bar" data-surface="check" role="toolbar" aria-label="Before you call">
        <CheckButton
          control={controls().mic}
          onPress={(): void => {
            setMicOn(!micOn());
          }}
        />
        <div class="lk-call-ctl">
          <button
            type="button"
            class="lk-call-ctl-btn lk-camera-check-call"
            ref={(el) => {
              callButton = el;
            }}
            aria-label={cameraCheckCallAccessibleName(props.peerName, camOn())}
            onClick={place}
          >
            <Icon name="phone" size={30} />
          </button>
          <span class="lk-call-ctl-label" aria-hidden="true">
            {cameraCheckCallLabel()}
          </span>
        </div>
        <CheckButton control={controls().camera} onPress={toggleCamera} />
      </div>
    </div>
  );
};

export default CameraCheck;
