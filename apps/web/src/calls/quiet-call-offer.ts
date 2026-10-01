// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The muted conversation's quiet call offer — which surfaces show it, and
 * until when (`docs/ux/calls.md` section 2.1.1).
 *
 * ## What this is for
 *
 * Muting takes away the **interruption**, not the call. The shared rule
 * (`crates/lakko-ui-rules/src/ring.rs`) answers a muted offer `Silent`, which
 * the call host takes exactly as it takes a ring — same session, same slot,
 * same call-log row — while withholding the ringtone, the vibration and the
 * screen that takes the tab over.
 *
 * What was missing is what the person is offered INSTEAD. This client's only
 * Accept was on the screen being withheld, so a muted call could not be
 * answered at all: muting a conversation quietly became half-blocking the
 * person. Section 2.1.1 rejects that reading and names two quiet surfaces —
 * a panel inside that conversation, and Accept / Decline on that chat's row
 * in the conversation list — and this module decides what each of them shows.
 *
 * ## Why it is a module and not a condition in the JSX
 *
 * vitest here runs bare node: no jsdom, no Solid transform, so nothing inside
 * a `.tsx` body is reachable from a test. A wrong condition has already
 * survived unnoticed in `routes/Call.tsx` for exactly that reason (the safety
 * number was rendered behind a layout flag, so it showed on no live call).
 * Two surfaces asking the same question in two components is the same hazard
 * twice, so the question is asked once, here.
 *
 * ## The deadline, and why it is not the session's own timer
 *
 * `CallSession` arms its ring timeout with the whole shared window at the
 * moment the offer ARRIVES. For a ring that is fine to within the poll
 * interval. For these surfaces it is not: a `Silent` offer can be almost the
 * whole window old already (anything short of the window is still live), and
 * a surface armed with the full window from arrival would keep offering an
 * answer well after the caller had given up. So the offer carries a deadline
 * computed from what is LEFT of the window (`ring-rule.ts`
 * `offerExpiresAtMs`), and every read here is bounded by it.
 */

/** A muted 1x1 call being offered quietly. */
import { t } from "../l10n";

export interface QuietCallOffer {
  /** The conversation it arrived in — which row, and which chat, offer it. */
  readonly groupIdB64: string;
  /** Lowercase hex of the 16-byte call id, so a surface can name the call it
   *  is acting on rather than "whatever is ringing". */
  readonly callIdHex: string;
  /** The kind of call the CALLER placed, for the one line the surfaces show. */
  readonly video: boolean;
  /** Epoch ms at which the ring window runs out and both surfaces stop
   *  offering it. */
  readonly expiresAtMs: number;
}

/** What the call host holds about the quiet offer. */
export interface QuietCallOfferState {
  /**
   * The 1x1 slot is ringing.
   *
   * Read as well as {@link taken} because it is what makes the surfaces
   * vanish the moment the call is answered, declined or gives up: at that
   * point the slot leaves `ringing` and there is nothing left to offer.
   */
  readonly ringing: boolean;
  /** The offer the host recorded when it took the call on the `silent` arm,
   *  or `null` when the ringing call is not a muted one. */
  readonly taken: QuietCallOffer | null;
  readonly nowMs: number;
}

/**
 * The quiet offer to show anywhere, or `null`.
 *
 * All three conditions have to hold, and none of them is the others: a
 * recorded offer with the slot no longer ringing is a call that has been
 * answered or has given up; a ringing slot with nothing recorded is an
 * ordinary unmuted ring, which takes the screen instead; and a recorded,
 * still-ringing offer past its deadline is one the caller has stopped waiting
 * on.
 */
export function quietCallOffer(state: QuietCallOfferState): QuietCallOffer | null {
  if (!state.ringing) return null;
  const offer = state.taken;
  if (offer === null) return null;
  return quietCallOfferIsLive(offer, state.nowMs) ? offer : null;
}

/**
 * The offer is still within the ring window.
 *
 * Exclusive at the deadline, matching the shared rule's own boundary: at the
 * window the caller has given up, so this side must not still be offering an
 * answer that would connect to nobody.
 */
export function quietCallOfferIsLive(offer: QuietCallOffer, nowMs: number): boolean {
  return nowMs < offer.expiresAtMs;
}

/**
 * The quiet offer for one conversation, or `null`.
 *
 * What both surfaces ask: the chat asks it for the conversation it is showing,
 * and each conversation row asks it for its own. A call that arrived in
 * another conversation is that conversation's to offer — section 2.1.1 is
 * explicit that the quiet surfaces are the muted conversation's own, so a
 * second chat must not grow an Accept button for a call that is nothing to do
 * with it.
 */
export function quietCallOfferFor(
  offer: QuietCallOffer | null,
  groupIdB64: string,
  nowMs: number,
): QuietCallOffer | null {
  if (offer === null || offer.groupIdB64 !== groupIdB64) return null;
  return quietCallOfferIsLive(offer, nowMs) ? offer : null;
}

/**
 * The one line saying what kind of call it is (`docs/ux/calls.md` section
 * 2.2, and 2.1.1 repeating it for the panel).
 *
 * The same two words the full-screen call uses, from the same source — the
 * offer's own media flags — because section 1 requires the word on the
 * callee's screen to match what the caller chose on every surface, not only
 * on the loud one.
 */
export function quietCallKindLabel(video: boolean): string {
  return video ? t("call.kind.video") : t("call.kind.voice");
}
