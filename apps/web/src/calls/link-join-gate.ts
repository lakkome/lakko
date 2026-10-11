// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Two decisions of the call-link page (`routes/JoinCall.tsx`), apart from the
 * page so they can be tested without a DOM.
 *
 * 1. What stops a tap on `Join` BEFORE anything is created. A visitor with
 *    no account becomes a guest on that tap (`docs/ux/call-links.md` section
 *    9.1 step 2: "A guest account is made silently ... and the call starts"),
 *    so a call that is known not to be able to start must be refused before
 *    the account is made. Seen on a browser that could not encrypt call
 *    media: the account was created, the call was refused, and the page then
 *    asked a person who had never been in a call whether to keep an account
 *    they had not asked for. The spec's question is for a guest who exists
 *    (section 9.1 step 5: "shown when the guest is not in a call - on the
 *    link page under `The call has ended.`, after a join that failed, after
 *    the page or the app is reopened"); the fix is that no guest is made for
 *    a call that cannot happen, not that an existing guest is left without
 *    the question.
 *
 * 2. Whether the page shows that question under its idle and failed states.
 *    Unchanged from the spec sentence quoted above; written down here so that
 *    it has a test.
 */

/** Why a join is refused before anything is created; `null` when it may go on. */
export type LinkJoinRefusal = "no-media-encryption" | "busy" | null;

/**
 * What stops a join before a guest account is created, a call-admission token
 * is asked for, or the microphone is opened.
 *
 *   * `canEncryptMedia` - the browser can hand encoded frames to the SFrame
 *     worker (`isSFrameTransformSupported`). Without it no call is opened
 *     (security invariant #4), whoever asks.
 *   * `busy` - another call holds this device.
 *
 * The encryption refusal comes first: ending the other call would not help.
 */
export function linkJoinRefusal(input: {
  readonly canEncryptMedia: boolean;
  readonly busy: boolean;
}): LinkJoinRefusal {
  if (!input.canEncryptMedia) return "no-media-encryption";
  if (input.busy) return "busy";
  return null;
}

/**
 * Whether the link page asks a guest `Keep using Lakko?` under its idle or
 * failed state (the ended and dead states ask it themselves). Section 9.1
 * step 5: the question is shown whenever a guest is not in a call, a failed
 * join included, because this page is the only screen a guest has and the
 * question is the only way to delete or keep the account. A visitor who is
 * not a guest is never asked.
 */
export function showsGuestQuestion(input: {
  readonly isGuest: boolean;
  readonly phase: string;
}): boolean {
  return input.isGuest && (input.phase === "idle" || input.phase === "error");
}
