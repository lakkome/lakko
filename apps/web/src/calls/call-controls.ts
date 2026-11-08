// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which call controls exist, in what order, and in which of their two states
 * (`docs/ux/calls.md` §4.1, §4.1.1).
 *
 * ## The row
 *
 * | Call | The row, left to right |
 * |---|---|
 * | One-to-one, voice or video | Microphone · Camera · Settings · End |
 * | Group | Microphone · Camera · Participants · Chat · Settings · End |
 *
 * A browser that can send a screen share gets `Share screen` after Camera in
 * a connected call (`docs/ux/screen-share.md` section 1); everywhere else the
 * circle is absent, not disabled.
 *
 * Nothing else. There is no Speaker control and no More menu. The group
 * row's `Chat` (section 4.1.4, 2026-11-07) opens the call's own chat; it
 * replaced nothing, the old Chat circle that sent nothing having gone in
 * 2026-10. Which
 * microphone, camera and speaker the call uses is chosen in Settings, a sheet
 * of the app's own (`call-settings.ts`); the row itself stays short enough to
 * fit a phone in one line.
 *
 * ## What the old rows got wrong
 *
 * 1. **Wrong kind of control.** The pre-call screen used switches in a settings
 *    list. "Never a switch, a checkbox or a list row inside a call. Every call
 *    control is a round icon button that toggles on press."
 * 2. **Backwards states.** The accent highlight landed on the button that was
 *    **off**, so a muted microphone read as a live one. §4.1.1: off is the loud
 *    state — solid white with a dark slashed icon — and on is the quiet outline.
 * 3. **Controls that did nothing, and too many of them.** A group call carried
 *    Speaker, Mute, Camera, Participants, Chat, Invite and End: seven circles
 *    that wrapped into two rows on a phone, one of which (Chat) sent nothing
 *    anywhere (found live 2026-10-02).
 *
 * ## The row is the same for both kinds of one-to-one call
 *
 * "People start a call with their voice and decide to show something a minute
 * later; every messenger allows that." So the camera button does not depend on
 * how the call was placed, and it must not depend on `callSurfaceKind` either —
 * that asks what there is to *look at*, and it flips to the portrait the moment
 * both cameras are off. A camera button that vanished exactly when both cameras
 * were off would be missing precisely when it is the only way back.
 *
 * ## Participants: who is here, and how to bring someone
 *
 * A group call's one entry beyond the two devices. On a call that can hand out
 * a link to itself (`docs/ux/call-links.md` section 3: a call founded from
 * `New call` or joined from a `/c/` link) it opens a menu of the list of
 * participants and the two ways to invite; on a call placed inside a
 * conversation, which is reached through that conversation and has no link, it
 * opens the list directly, and is absent while the person is alone, when there
 * would be nothing to list.
 */

import type { CallPhaseKind, CallSurfaceKind } from "./call-screen-model";
import { t } from "../l10n";
import { chatControlName, unreadBadge } from "./call-chat-panel-state";

/** The controls the row can carry, in the order §4.1's table lists them. */
export type CallControlId =
  | "mic"
  | "camera"
  | "screen"
  | "participants"
  | "chat"
  | "settings"
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
  | "mic"
  | "micOff"
  | "video"
  | "videoOff"
  | "monitor"
  | "groups"
  | "chat"
  | "settings"
  | "phoneEnd";

/** One circle in the row. */
export interface CallControlSpec {
  readonly id: CallControlId;
  /** The short word under the circle. Names the thing, never the state, and
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
  /** A count drawn on the circle (`Chat`'s unread messages, `1`..`9`, `9+`);
   *  absent for none. */
  readonly badge?: string;
}

/** What {@link callControls} needs to know. */
export interface CallControlsInput {
  /** The layout, which only matters for the group entry (Participants). */
  readonly surface: CallSurfaceKind;
  /** Someone besides this device is in the call. */
  readonly othersPresent?: boolean;
  /** Whether the participant list is open (group only; the button reads
   *  pressed while it is, where it opens the list directly). */
  readonly participantsOpen?: boolean;
  /**
   * This call can hand out a link to itself
   * (`docs/ux/call-links.md` section 3).
   *
   * True for a call whose `call_id` a `CallInviteCapability` can name — the
   * calls `New call` starts, and the ones joined from a `/c/` link. A call
   * placed inside a conversation is reached through that conversation, and a
   * link minted over its id would lead a stranger to look for a call group
   * that does not exist.
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
  /**
   * The screen share (`docs/ux/screen-share.md`): absent when this device
   * cannot send one (iOS, Android, a phone or tablet browser, a browser with
   * no screen capture). `sharing` is this device's own share; `busy` the
   * picker is open or the share is being set up.
   */
  readonly screenShare?: { readonly sharing: boolean; readonly busy: boolean } | undefined;
  /**
   * The call's chat (section 4.1.4): absent on a call that has none (a
   * one-to-one call). `open` is the panel; `unread` the messages from others
   * that arrived while it was closed.
   */
  readonly chat?: { readonly open: boolean; readonly unread: number } | undefined;
}

/**
 * The row, left to right.
 *
 * §4's state table: while `Calling` "only End is active" — there is nothing to
 * mute yet — so the devices are disabled rather than removed, which is §4.1.1's
 * rule ("never a hidden button"). Once the call is over every control is inert,
 * including End: pressing it a second time cannot end the call more.
 */
export function callControls(input: CallControlsInput): CallControlSpec[] {
  const dead = input.phase === "ended" || input.phase === "idle";
  const inactive = dead || input.phase === "calling";
  const out: CallControlSpec[] = [];

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

  // Share screen: only inside a connected call, only where this device can
  // send one (section 1). Sharing is the loud, filled state (section 3).
  const share = input.screenShare;
  if (share !== undefined && input.phase === "connected") {
    out.push({
      id: "screen",
      label: t("call.screen_share.button"),
      accessibleName: share.sharing ? t("call.screen_share.button_stop") : t("call.screen_share.button"),
      icon: "monitor",
      tone: share.sharing ? "solid" : "outline",
      pressed: share.sharing,
      disabled: share.busy,
    });
  }

  // Present when it has something to open: a call that can invite always
  // does; a call placed inside a conversation only once someone else is in
  // it.
  const canInvite = input.canInvite === true;
  const others = input.othersPresent === true;
  if (input.surface === "group" && (canInvite || others)) {
    out.push({
      id: "participants",
      label: t("call.control.participants"),
      accessibleName: canInvite
        ? t("call.control.participants_menu_a11y")
        : input.participantsOpen === true
          ? t("call.control.participants_hide_a11y")
          : t("call.control.participants_show_a11y"),
      icon: "groups",
      tone: "outline",
      // A toggle only where it opens the list directly; where it opens a menu
      // a pressed state would be a false claim.
      pressed: canInvite ? null : input.participantsOpen === true,
      disabled: dead || input.inviteBusy === true,
    });
  }

  // The call's chat (section 4.1.4): after Participants, from the first
  // second, alone in the call too.
  const chat = input.chat;
  if (input.surface === "group" && chat !== undefined) {
    const spoken = chatControlName(chat.open, chat.unread);
    const badge = chat.open ? null : unreadBadge(chat.unread);
    out.push({
      id: "chat",
      label: t("call.control.chat"),
      accessibleName:
        spoken.count === undefined ? t(spoken.key) : t(spoken.key, { count: spoken.count }),
      icon: "chat",
      tone: "outline",
      pressed: chat.open,
      disabled: dead,
      ...(badge !== null ? { badge } : {}),
    });
  }

  // Which microphone, camera and speaker the call uses. Live from the first
  // second: a person who cannot be heard wants it before anyone answers.
  out.push({
    id: "settings",
    label: t("call.control.settings"),
    accessibleName: t("call.settings.title"),
    icon: "settings",
    tone: "outline",
    pressed: null,
    disabled: dead,
  });

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
