// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `Invite`, the one control the call screen gains
 * (`docs/ux/call-links.md` sections 3 and 5).
 *
 * It opens a menu of the app's own with two items: **Copy link**, and
 * **Invite a contact**, which sends the invitation into a conversation the
 * person picks. Never the system's share sheet: a system sheet is a foreign
 * surface inside the call, different on every platform, and it offered a
 * dozen destinations where two are wanted.
 *
 * Both items hand out the link to THIS call — the same `call_id` the people
 * already in the call are using — so whoever follows it arrives here and not
 * in a new call. The first use in a call is what mints the capability
 * ([ADR-0032](../../../../docs/adr/0032-multi-joiner-call-links.md)), which
 * costs a network round trip; later uses reuse the link already minted.
 *
 * Minting from inside the call, rather than at `New call`, is what keeps
 * starting a call instant: the cost is paid by the person who actually wants
 * a link, at the moment they want it, and never by everyone else.
 *
 * ## Why the wait needs words
 *
 * Section 5 asks the busy state to say what it is doing "rather than only
 * spinning". A spinner on the button says *something* is happening; it does
 * not say that the wait is expected and bounded, which is the difference
 * between a person waiting and a person pressing again. So the state carries a
 * sentence, and the caller renders it.
 *
 * ## Why a failure must not touch the call
 *
 * Minting talks to the discovery service. The call does not: it is founded and
 * hosted over the SFU, and it keeps running whatever the ledger says. So every
 * failure here ends in a sentence and an unchanged call — never a hang-up.
 *
 * Pure and node-testable: vitest here runs without jsdom, so the clipboard
 * arrives as an injected function rather than as a global.
 */

/** Where the link ended up. */
import { t } from "../l10n";

export type LinkDelivery =
  /** On the clipboard. */
  | { readonly kind: "copied" }
  /** The clipboard was not reachable, so the link itself is shown to be
   *  copied by hand. */
  | { readonly kind: "shown" }
  /** Sent as an invitation into a conversation; `name` is its title. */
  | { readonly kind: "sent"; readonly name: string };

/** Why minting did not produce a link. */
export type InviteFailure =
  /** There is no live session to mint under. */
  | "signed-out"
  /** The round trip failed, or the ledger refused. */
  | "unavailable"
  /** The link exists, but the invitation could not be sent to the picked
   *  conversation. */
  | "not-sent";

/** What `Invite` is doing, from the moment the call starts. */
export type InCallInvite =
  /** Nothing minted yet in this call. */
  | { readonly kind: "idle" }
  /** Minting: the round trip and the proof-of-work solve. */
  | { readonly kind: "minting" }
  /** A link exists for this call, and where the last press put it. */
  | { readonly kind: "ready"; readonly url: string; readonly delivery: LinkDelivery }
  /** `url` is kept when the link was minted and only the delivery failed, so
   *  the next use does not mint again. */
  | { readonly kind: "failed"; readonly failure: InviteFailure; readonly url?: string };

/** The state a call starts in: no link, nothing paid for. */
export const INVITE_IDLE: InCallInvite = { kind: "idle" };

/** What choosing a menu item does now. */
export type InvitePress =
  /** Mint, then deliver. */
  | "mint"
  /** Deliver the link already minted — no second round trip. */
  | "reoffer"
  /** A mint is already in flight; a second one must not start. */
  | "ignore";

/** The link this call already has, if any. */
export function mintedInviteUrl(state: InCallInvite): string | null {
  if (state.kind === "ready") return state.url;
  if (state.kind === "failed" && state.url !== undefined) return state.url;
  return null;
}

/**
 * What the next press of `Invite` should do.
 *
 * A failed mint returns to `mint`: section 5 says the failure "can be tried
 * again", and nothing about a failure makes the next attempt less likely to
 * work.
 */
export function nextInvitePress(state: InCallInvite): InvitePress {
  switch (state.kind) {
    case "minting":
      return "ignore";
    case "ready":
      return "reoffer";
    case "idle":
      return "mint";
    case "failed":
      return state.url === undefined ? "mint" : "reoffer";
  }
}

/** Whether the control shows its working state. */
export function inviteBusy(state: InCallInvite): boolean {
  return state.kind === "minting";
}

/**
 * The line the call screen shows under the status row, or `null` when there is
 * nothing to say.
 *
 * Sentence case, no protocol names, no exclamation marks — and the failure
 * gets one sentence on what happened and one on what to do, per section 5.
 */
export function inviteStatusLine(state: InCallInvite): string | null {
  switch (state.kind) {
    case "idle":
      return null;
    case "minting":
      return t("call.invite.minting");
    case "ready":
      switch (state.delivery.kind) {
        case "copied":
          return t("call.invite.copied");
        case "shown":
          return t("call.invite.shown", { url: state.url });
        case "sent":
          return t("call.invite.sent", { name: state.delivery.name });
      }
      break;
    case "failed":
      switch (state.failure) {
        case "signed-out":
          return t("call.invite.signed_out");
        case "unavailable":
          return t("call.invite.unavailable");
        case "not-sent":
          return t("call.invite.send_failed");
      }
  }
  return null;
}

/**
 * How long a finished line stays up before the screen goes quiet again.
 *
 * Long enough to read a sentence; the `shown` fallback carries a URL a person
 * has to select by hand, so it is not cleared on a timer at all — see
 * {@link inviteStatusHoldMs}.
 */
export const INVITE_STATUS_HOLD_MS = 6_000;

/**
 * How long this state's line should stay, or `null` for "until something else
 * replaces it".
 *
 * `minting` has no timer: it ends when the mint does. `shown` has none either
 * — taking a link away while someone is selecting it is the one case where a
 * timer does harm.
 */
export function inviteStatusHoldMs(state: InCallInvite): number | null {
  if (state.kind === "minting") return null;
  if (state.kind === "ready" && state.delivery.kind === "shown") return null;
  if (state.kind === "idle") return null;
  return INVITE_STATUS_HOLD_MS;
}

/**
 * Put the link on the clipboard, or in front of the person to copy by hand
 * when the clipboard is not reachable (a page without clipboard permission).
 */
export async function copyCallLink(
  url: string,
  copy: ((text: string) => Promise<void>) | undefined,
): Promise<LinkDelivery> {
  if (copy !== undefined) {
    try {
      await copy(url);
      return { kind: "copied" };
    } catch {
      // Fall through to showing it.
    }
  }
  return { kind: "shown" };
}

/** What becomes of a link once its mint has finished. */
export type MintedInviteFate =
  /** The call it was minted for still holds the slot. */
  | "show"
  /** No call holds the slot: the line goes back to idle. */
  | "call-over"
  /** Another call holds the slot: nothing of this link is shown on it. */
  | "another-call";

/**
 * A mint takes a round trip, and the call it was started for can end during
 * it and be followed by another. Asking only "does some link call hold the
 * slot" then shows the first call's link on the second, where anyone it is
 * sent to joins a call that is over. So the call id read before the mint is
 * compared with the one in the slot after it.
 */
export function mintedInviteFate(input: {
  /** The call the mint was started for. */
  readonly mintedForCallIdHex: string;
  /** The link call holding the slot now, or `null` for none. */
  readonly slotCallIdHex: string | null;
}): MintedInviteFate {
  if (input.slotCallIdHex === null) return "call-over";
  return input.slotCallIdHex === input.mintedForCallIdHex ? "show" : "another-call";
}
