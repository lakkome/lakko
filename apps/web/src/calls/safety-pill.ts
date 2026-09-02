// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When the in-call safety-number pill is shown.
 *
 * ## Why this is a module and not a condition in the JSX
 *
 * It used to be a condition in the JSX, and it was wrong there for nine months
 * without anything noticing — `vitest` runs in a plain `node` environment with no
 * jsdom and no Solid JSX transform, so nothing in `routes/Call.tsx` is reachable
 * from a test. A rule nobody can assert is a rule that drifts.
 *
 * ## The bug this replaces, because the reasoning that produced it was sound
 *
 * `Call.tsx` gated the pill on `props.embedded !== true`, with a comment giving a
 * correct reason: the **1×1** web path is P2P DTLS-SRTP with no SFrame, so there is
 * no safety number and a "secured" chip would mislead.
 *
 * That reason is right about 1×1 and wrong about everything else, because the same
 * component also renders the **SFU group call**, which does seal media with SFrame
 * and does have a real safety number. And `Chat.tsx` mounts the call
 * `embedded={true}` in every case — so the gate hid the pill on **every live call**,
 * group calls included. The number was derived, written into the call store, and
 * never reached a screen; it survived only on the standalone design route.
 *
 * ## The rule
 *
 * Show the pill exactly when there **is** a safety number to show. That is not a
 * proxy for the 1×1-vs-group question, it is the question: the 1×1 path publishes
 * `{ active: false, shortFingerprint: null }` (`routes/Chat.tsx`) and the SFU path
 * publishes `{ active: true, shortFingerprint: <derived> }` (`calls/sfu-call-ui.ts`,
 * `calls/multi-joiner-call.ts`). So the misleading-chip hazard the old comment
 * worried about is closed by the data rather than by a layout flag — and a future
 * path that arms SFrame gets the pill automatically instead of needing someone to
 * remember a second gate.
 *
 * **Both halves are load-bearing.** `active` without a number means keying is armed
 * but the number is not derived yet — showing an empty chip claims a verification
 * the user cannot perform. A number without `active` should not occur, and if it
 * does the honest answer is not to claim protection.
 */

/** The call store's E2EE slice, as this rule reads it. */
export interface SafetyPillInput {
  readonly active: boolean;
  readonly shortFingerprint: string | null;
}

/**
 * `true` when the pill should render its safety number.
 *
 * Deliberately takes no notion of which surface is asking. Whether the call is
 * embedded in a chat or standing alone is a layout question, and it was answering
 * this one by accident.
 */
export function showsSafetyNumber(e2ee: SafetyPillInput): boolean {
  return e2ee.active && e2ee.shortFingerprint !== null;
}
