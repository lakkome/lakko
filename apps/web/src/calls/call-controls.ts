// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which call controls exist, in what order, and in which of their two states
 * (`docs/ux/calls.md` §4.1, §4.1.1).
 *
 * ## The three things §4.1 says the old row got wrong
 *
 * 1. **Wrong kind of control.** The pre-call screen used switches in a settings
 *    list and the call screen used loose circles on the background. "Never a
 *    switch, a checkbox or a list row inside a call. Every call control is a
 *    round icon button that toggles on press."
 * 2. **Wrong contents.** Real actions sat behind an ellipsis menu while
 *    Speaker — "the control people reach for most on a voice call" — was not on
 *    the screen at all. There is no More menu in a 1x1 call: the whole set is
 *    the row.
 * 3. **Backwards states.** The accent highlight landed on the button that was
 *    **off**, so a muted microphone read as a live one. §4.1.1: off is the loud
 *    state — solid white with a dark slashed icon — and on is the quiet outline.
 *
 * ## Why the row follows the call's KIND and not what is on screen
 *
 * §4.1's table is indexed by call kind — a voice call carries `Speaker · Mute ·
 * End`, a video call adds `Camera` — and that is deliberately not the same
 * question as `callSurfaceKind` in `calls/call-screen-model.ts`, which asks what
 * there is to *look at*. If the row followed the surface, stopping your own
 * camera on a video call whose callee also has theirs off would delete the
 * camera button from under your thumb and leave no way to start it again. So
 * the row reads {@link CallControlsInput.mediaKind}, which a call is placed
 * with and which does not move.
 *
 * ## Speaker is omitted, never dead
 *
 * §4.1 asks for Speaker "where the browser supports output selection; omit it
 * entirely where it does not, rather than showing a dead button". Two things
 * have to be true for it to do anything: the browser must expose
 * `HTMLMediaElement.setSinkId`, and there must be a second output to move the
 * sound to. One output and a button that cycles to itself is the dead button
 * that clause names, so both are required.
 */

import type { CallPhaseKind, CallSurfaceKind } from "./call-screen-model";

/** The controls the row can carry, in the order §4.1's table lists them. */
export type CallControlId = "speaker" | "mic" | "camera" | "overflow" | "end";

/**
 * How a control is painted (§4.1.1).
 *
 * `outline` — the thing is on (a live microphone, a sending camera): a
 * translucent fill and a plain icon. `solid` — the thing is off: solid white,
 * dark slashed icon, the loud state. `end` — the one red circle on the screen.
 */
export type CallControlTone = "outline" | "solid" | "end";

/** Icon names, matching `ui/icons.tsx`. */
export type CallControlIcon =
  | "speaker"
  | "mic"
  | "micOff"
  | "video"
  | "videoOff"
  | "more"
  | "phoneEnd";

/** One circle in the row. */
export interface CallControlSpec {
  readonly id: CallControlId;
  /** The short word under the circle. Names the action, never the state, and
   *  never changes with it (§4.1.1). */
  readonly label: string;
  /** What a screen reader says: the action and the resulting state (§4.1.1,
   *  §8). */
  readonly accessibleName: string;
  readonly icon: CallControlIcon;
  readonly tone: CallControlTone;
  /** `aria-pressed` for the toggles; `null` for the buttons that are not
   *  toggles, where a pressed state would be a false claim. */
  readonly pressed: boolean | null;
  /** 40 percent opacity and inert — never hidden (§4.1.1). */
  readonly disabled: boolean;
}

/** What {@link callControls} needs to know. */
export interface CallControlsInput {
  /** What the call was placed or answered as. Decides whether the row carries
   *  a camera button; see the module note above. */
  readonly mediaKind: "voice" | "video";
  /** The layout, which only matters for the group overflow entry. */
  readonly surface: CallSurfaceKind;
  readonly phase: CallPhaseKind;
  readonly micOn: boolean;
  readonly cameraOn: boolean;
  /** The camera is being acquired; the button shows a spinner and refuses a
   *  second press rather than queueing one. */
  readonly cameraBusy: boolean;
  /** `HTMLMediaElement.setSinkId` exists in this browser. */
  readonly canSelectAudioOutput: boolean;
  /** Name of the output a press would move the sound to, when known. Device
   *  labels are blank until the browser has granted a capture permission, so
   *  `null` is the ordinary early state and not an error. */
  readonly nextAudioOutputLabel: string | null;
}

/**
 * The row, left to right.
 *
 * §4's state table: while `Calling` "only End is active" — there is nothing to
 * mute yet and no output to move — so everything else is disabled rather than
 * removed, which is §4.1.1's rule ("never a hidden button"). Once the call is
 * over every control is inert, including End: pressing it a second time cannot
 * end the call more.
 */
export function callControls(input: CallControlsInput): CallControlSpec[] {
  const dead = input.phase === "ended" || input.phase === "idle";
  const inactive = dead || input.phase === "calling";
  const out: CallControlSpec[] = [];

  if (input.canSelectAudioOutput && input.nextAudioOutputLabel !== null) {
    out.push({
      id: "speaker",
      label: "Speaker",
      accessibleName: `Play sound through ${input.nextAudioOutputLabel}`,
      icon: "speaker",
      tone: "outline",
      pressed: null,
      disabled: inactive,
    });
  }

  out.push({
    id: "mic",
    label: "Mute",
    accessibleName: input.micOn ? "Mute microphone" : "Unmute microphone",
    icon: input.micOn ? "mic" : "micOff",
    tone: input.micOn ? "outline" : "solid",
    pressed: !input.micOn,
    disabled: inactive,
  });

  if (input.mediaKind === "video" || input.surface === "group") {
    out.push({
      id: "camera",
      label: "Camera",
      accessibleName: input.cameraOn ? "Stop camera" : "Start camera",
      icon: input.cameraOn ? "video" : "videoOff",
      tone: input.cameraOn ? "outline" : "solid",
      pressed: !input.cameraOn,
      disabled: inactive || input.cameraBusy,
    });
  }

  if (input.surface === "group") {
    out.push({
      id: "overflow",
      label: "More",
      accessibleName: "Show participants and chat",
      icon: "more",
      tone: "outline",
      pressed: null,
      disabled: dead,
    });
  }

  out.push({
    id: "end",
    label: "End",
    accessibleName: "End call",
    icon: "phoneEnd",
    tone: "end",
    pressed: null,
    disabled: dead,
  });

  return out;
}

/** An audio output, as {@link nextAudioOutput} reads one. Kept local so this
 *  module never imports the Solid store. */
export interface AudioOutputDevice {
  readonly id: string;
  readonly label: string;
}

/**
 * The output a press of Speaker moves the sound to: the next one after the
 * current pick, wrapping.
 *
 * Cycling rather than opening a list is what keeps §4.1's "never a list row
 * inside a call" and "something a person does mid-sentence with one thumb"
 * both true. `null` when there is nothing to move to, which is what makes the
 * button disappear instead of doing nothing.
 *
 * A selection this browser no longer offers (a headset was unplugged) is
 * treated as no selection, so the next press lands on the first real device
 * instead of on nothing.
 */
export function nextAudioOutput(
  outputs: readonly AudioOutputDevice[],
  selectedId: string | null,
): AudioOutputDevice | null {
  if (outputs.length < 2) return null;
  const index = selectedId === null ? -1 : outputs.findIndex((d) => d.id === selectedId);
  const next = outputs[(index + 1) % outputs.length];
  return next ?? null;
}

/**
 * What to call an output in a spoken label.
 *
 * `enumerateDevices` returns blank labels until a capture permission has been
 * granted, and the default device is conventionally reported with the reserved
 * id `default`. Neither is worth reading aloud as-is.
 */
export function audioOutputName(device: AudioOutputDevice | null): string | null {
  if (device === null) return null;
  const label = device.label.trim();
  if (label.length > 0) return label;
  return device.id === "default" ? "the default speaker" : "the next speaker";
}
