// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `Invite`, the one control the call screen gains
 * (`docs/ux/call-links.md` sections 3 and 5).
 *
 * It offers the link to THIS call — the same `call_id` the people already in
 * the call are using — so whoever follows it arrives here and not in a new
 * call. The first press in a call is what mints the capability
 * ([ADR-0032](../../../../docs/adr/0032-multi-joiner-call-links.md)), which
 * costs a network round trip and a proof-of-work solve; a second press
 * re-offers the link already minted rather than paying that again.
 *
 * Minting from inside the call, rather than at `New call`, is what keeps
 * starting a call instant: the cost is paid by the person who actually wants a
 * link, at the moment they want it, and never by everyone else.
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
 * Pure and node-testable: vitest here runs without jsdom, so `navigator.share`
 * and the clipboard arrive as injected functions rather than as globals.
 */

/** Where the link ended up. */
import { t } from "../l10n";

export type LinkDelivery =
  /** Handed to the platform's own share mechanism. */
  | "shared"
  /** On the clipboard. */
  | "copied"
  /** Neither was available, so the link itself is shown to be copied by
   *  hand. */
  | "shown";

/** Why minting did not produce a link. */
export type InviteFailure =
  /** There is no live session to mint under. */
  | "signed-out"
  /** The round trip failed, or the ledger refused. */
  | "unavailable";

/** What `Invite` is doing, from the moment the call starts. */
export type InCallInvite =
  /** Nothing minted yet in this call. */
  | { readonly kind: "idle" }
  /** Minting: the round trip and the proof-of-work solve. */
  | { readonly kind: "minting" }
  /** A link exists for this call, and where the last press put it. */
  | { readonly kind: "ready"; readonly url: string; readonly delivery: LinkDelivery }
  | { readonly kind: "failed"; readonly failure: InviteFailure };

/** The state a call starts in: no link, nothing paid for. */
export const INVITE_IDLE: InCallInvite = { kind: "idle" };

/** What a press does now. */
export type InvitePress =
  /** Mint, then deliver. */
  | "mint"
  /** Deliver the link already minted — no second round trip. */
  | "reoffer"
  /** A mint is already in flight; a second press must not start another. */
  | "ignore";

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
    case "failed":
      return "mint";
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
      switch (state.delivery) {
        case "shared":
          return t("call.invite.shared");
        case "copied":
          return t("call.invite.copied");
        case "shown":
          return t("call.invite.shown", { url: state.url });
      }
      break;
    case "failed":
      switch (state.failure) {
        case "signed-out":
          return t("call.invite.signed_out");
        case "unavailable":
          return t("call.invite.unavailable");
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
  if (state.kind === "ready" && state.delivery === "shown") return null;
  if (state.kind === "idle") return null;
  return INVITE_STATUS_HOLD_MS;
}

/** The two platform mechanisms, injected so this module stays testable. */
export interface LinkDeliveryDeps {
  /** The platform's own share mechanism (`navigator.share` on web), when it
   *  exists. */
  readonly share?: ((url: string) => Promise<void>) | undefined;
  /** Writing to the clipboard, when it is reachable. */
  readonly copy?: ((text: string) => Promise<void>) | undefined;
}

/**
 * Put the link where the platform puts things: the share mechanism where there
 * is one, else the clipboard, else in front of the person to copy by hand.
 *
 * A rejected share falls through to the clipboard. The web share API rejects
 * both when it fails and when the person dismisses the sheet, and the two are
 * not reliably distinguishable; landing the link on the clipboard in either
 * case is the outcome that leaves a person with what they asked for, and the
 * status line then says "copied", which is true.
 */
export async function deliverCallLink(
  url: string,
  deps: LinkDeliveryDeps,
): Promise<LinkDelivery> {
  if (deps.share !== undefined) {
    try {
      await deps.share(url);
      return "shared";
    } catch {
      // Fall through to the clipboard.
    }
  }
  if (deps.copy !== undefined) {
    try {
      await deps.copy(url);
      return "copied";
    } catch {
      // Fall through to showing it.
    }
  }
  return "shown";
}
