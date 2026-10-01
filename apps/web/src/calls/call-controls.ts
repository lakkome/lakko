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
 * ## The row is the same for both kinds of call, and that is the point
 *
 * §4.1's table gives a voice call and a video call the same four controls, and
 * says why: "People start a call with their voice and decide to show something a
 * minute later; every messenger allows that, and taking it away to keep the row
 * short would be a feature lost to tidiness."
 *
 * So the camera button does not depend on how the call was placed, and it must
 * not depend on `callSurfaceKind` either — that asks what there is to *look at*,
 * and it flips to the portrait the moment both cameras are off. A camera button
 * that vanished exactly when both cameras were off would be missing precisely
 * when it is the only way back. The row is therefore constant for a 1x1 call,
 * and the only thing that varies is the group overflow.
 *
 * ## Invite is there only where it leads somewhere
 *
 * `docs/ux/call-links.md` section 3 gives the call screen one new control. It
 * is not universal: it needs a call a `CallInviteCapability` can name, which
 * is a call founded from `New call` or joined from a `/c/` link. Showing it on
 * a conversation's call would offer a link into a call the arrival cannot be
 * admitted to, which is the same fault as Speaker's dead button below.
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
import { t } from "../l10n";

/** The controls the row can carry, in the order §4.1's table lists them. */
export type CallControlId =
  | "speaker"
  | "mic"
  | "camera"
  | "participants"
  | "chat"
  | "invite"
  | "end";

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
  | "groups"
  | "chat"
  | "link"
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
  /** The layout, which only matters for the group entries (participants,
   *  chat). */
  readonly surface: CallSurfaceKind;
  /** Whether the participant list is open (group only; the button reads
   *  pressed while it is). */
  readonly participantsOpen?: boolean;
  /** Whether the chat panel is open (group only). */
  readonly chatOpen?: boolean;
  /**
   * This call can hand out a link to itself
   * (`docs/ux/call-links.md` section 3).
   *
   * True for a call whose `call_id` a `CallInviteCapability` can name — the
   * calls `New call` starts, and the ones joined from a `/c/` link. A call
   * placed inside a conversation is reached through that conversation, and a
   * link minted over its id would lead a stranger to look for a call group
   * that does not exist, so the control is absent there rather than present
   * and broken.
   */
  readonly canInvite?: boolean;
  /** The link is being prepared: the button shows a spinner and refuses a
   *  second press rather than paying for a second one. */
  readonly inviteBusy?: boolean;
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
      label: t("call.control.speaker"),
      accessibleName: t("call.control.speaker_a11y", { device: input.nextAudioOutputLabel }),
      icon: "speaker",
      tone: "outline",
      pressed: null,
      disabled: inactive,
    });
  }

  out.push({
    id: "mic",
    label: t("call.control.mute"),
    accessibleName: input.micOn ? t("call.control.mute_a11y") : t("call.control.unmute_a11y"),
    icon: input.micOn ? "mic" : "micOff",
    tone: input.micOn ? "outline" : "solid",
    pressed: !input.micOn,
    disabled: inactive,
  });

  out.push({
    id: "camera",
    label: t("call.control.camera"),
    // §4.1: pressing this on a voice call starts the camera and turns the call
    // into a video one. So the name is what the press does, not what the call
    // was placed as.
    accessibleName: input.cameraOn ? t("call.control.camera_stop_a11y") : t("call.control.camera_start_a11y"),
    icon: input.cameraOn ? "video" : "videoOff",
    tone: input.cameraOn ? "outline" : "solid",
    pressed: !input.cameraOn,
    disabled: inactive || input.cameraBusy,
  });

  if (input.surface === "group") {
    // Section 4.1: on the web there is room for the whole set, so the
    // participants and chat entries sit in the row rather than behind a More
    // button (the phones keep the overflow; six circles do not fit their row).
    out.push({
      id: "participants",
      label: t("call.control.participants"),
      accessibleName:
        input.participantsOpen === true
          ? t("call.control.participants_hide_a11y")
          : t("call.control.participants_show_a11y"),
      icon: "groups",
      tone: "outline",
      pressed: input.participantsOpen === true,
      disabled: dead,
    });
    out.push({
      id: "chat",
      label: t("call.control.chat"),
      accessibleName:
        input.chatOpen === true ? t("call.control.chat_hide_a11y") : t("call.control.chat_show_a11y"),
      icon: "chat",
      tone: "outline",
      pressed: input.chatOpen === true,
      disabled: dead,
    });
  }

  if (input.canInvite === true) {
    // Section 3: one control, which offers the link to THIS call. It sits
    // beside the other group entries and before End, so the destructive
    // control keeps the far side of the row to itself.
    out.push({
      id: "invite",
      label: t("call.control.invite"),
      accessibleName: t("call.control.invite_a11y"),
      icon: "link",
      tone: "outline",
      // Not a toggle: the link is offered, never left switched on.
      pressed: null,
      disabled: dead || input.inviteBusy === true,
    });
  }

  out.push({
    id: "end",
    label: t("call.control.end"),
    accessibleName: t("call.control.end_a11y"),
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
  return device.id === "default"
    ? t("call.control.speaker_default_device")
    : t("call.control.speaker_next_device");
}
