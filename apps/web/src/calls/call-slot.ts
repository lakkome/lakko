// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One call at a time, across the three kinds of call the web client can be in.
 *
 * The app-level call host (`CallHostContext.tsx`) holds a 1x1 call, a group
 * (SFU) call in a chat, and an ADR-0032 call-link call. They share one in-call
 * UI store (`state/call-state.ts`), one overlay and one set of media devices,
 * so two of them live at once would not be two calls — it would be one screen
 * driven by two sessions. Before the link call moved into the host, opening a
 * `/c/` link during a group call did exactly that.
 *
 * Every entry point into a call asks {@link callSlotOccupant} first, and a
 * refusal is explained with {@link callBusyMessage} rather than silently
 * ignored. Kept pure so it can be tested (vitest runs in plain node, without
 * the Solid transform).
 */

/** The kind of call occupying the slot. */
export type CallKind = "one-to-one" | "group" | "link";

export interface CallSlotInput {
  /** A 1x1 call is ringing, dialing, connecting, live or ending. */
  readonly oneToOneInCall: boolean;
  /** A group call is starting or live. */
  readonly groupCallActive: boolean;
  /** A call-link call is starting, waiting for admission or live. */
  readonly linkCallActive: boolean;
}

/** The call occupying the slot, or `null` when it is free. At most one of the
 *  inputs is true in a correct host; the order only makes a bug deterministic. */
export function callSlotOccupant(input: CallSlotInput): CallKind | null {
  if (input.oneToOneInCall) return "one-to-one";
  if (input.groupCallActive) return "group";
  if (input.linkCallActive) return "link";
  return null;
}

function occupantPhrase(kind: CallKind): string {
  switch (kind) {
    case "one-to-one":
      return "a call";
    case "group":
      return "a group call";
    case "link":
      return "a call from a link";
  }
}

/**
 * Why a new call was refused: `occupant` is the call already running,
 * `verb` what the user tried to do. Says what to do about it, because the
 * refusal is the whole outcome — nothing else happens.
 */
export function callBusyMessage(occupant: CallKind, verb: "start" | "join"): string {
  return `You are already in ${occupantPhrase(occupant)}. End it before you ${verb} another one.`;
}
