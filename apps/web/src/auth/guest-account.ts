// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A guest account: the account made for someone who joined a call from a
 * link without having one (`docs/ux/call-links.md` section 9, ADR-0032).
 *
 * ## What changed (2026-10-02)
 *
 * The first version showed the 24-word recovery phrase before the call. A
 * person who was sent a link wants the call; whether they want an account is
 * a question for afterwards. So now:
 *
 * - the account is made silently and the person goes straight into the call;
 * - while it is a guest account nothing about it is kept on a server beyond
 *   what the call itself needs: no backup, no three-word recovery, no push
 *   registration;
 * - after the call the person is asked once: `Create account` or
 *   `No, thanks`;
 * - `No, thanks` deletes the account at once, here and on the servers;
 * - an account that was never given a username is deleted by the servers a
 *   day after it was made, so closing the tab leaves nothing behind either.
 *
 * ## What this module is
 *
 * The two facts a device keeps about a guest account, and the one decision
 * the screens read. Pure, so vitest (plain node, no Solid transform) can run
 * all of it.
 *
 * 1. **The marker**: this account is a guest account, and whether a username
 *    was already taken for it. Plain `localStorage`; it holds no secret.
 * 2. **The phrase row**: the recovery phrase, wrapped under the device key in
 *    the phrase store, under a key of its own ({@link guestPhraseKey}) and
 *    not the account's. That choice is what keeps the backup and the
 *    three-word enrolment from running for a guest without either of them
 *    knowing what a guest is: both read the phrase by the account's key and
 *    find nothing.
 */

/** The subset of `Storage` the marker needs. */
export interface GuestMarkerStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** What the device knows about a guest account. */
export interface GuestAccountState {
  /**
   * The username already taken for this account by an earlier
   * `Create account` that did not finish, or `null`.
   *
   * A username cannot be changed and an account holds one, so a second
   * attempt must go on with this one instead of asking again.
   */
  readonly claimedUsername: string | null;
}

function markerKey(accountIdB64: string): string {
  return `lakko:guest-account:${accountIdB64}`;
}

const MARK_GUEST = "guest";
const MARK_NAMED_PREFIX = "named:";

/**
 * The phrase store key a guest's recovery phrase is kept under.
 *
 * Not the account id: see the module comment.
 */
export function guestPhraseKey(accountIdB64: string): string {
  return `guest:${accountIdB64}`;
}

/** Record that `accountIdB64` is a guest account. */
export function markGuestAccount(
  storage: GuestMarkerStorage | undefined,
  accountIdB64: string,
): void {
  try {
    storage?.setItem(markerKey(accountIdB64), MARK_GUEST);
  } catch {
    // Storage blocked: the account then reads as an ordinary one with no
    // username, which is what it is. It is still deleted after a day.
  }
}

/** Record the username taken for a guest account that is being kept. */
export function noteGuestUsernameClaimed(
  storage: GuestMarkerStorage | undefined,
  accountIdB64: string,
  username: string,
): void {
  try {
    storage?.setItem(markerKey(accountIdB64), `${MARK_NAMED_PREFIX}${username}`);
  } catch {
    // Storage blocked: a retry asks for the name again and the server
    // answers that the account already has one.
  }
}

/** What the device knows about `accountIdB64` as a guest, or `null` when it
 *  is not a guest account. */
export function guestAccountState(
  storage: GuestMarkerStorage | undefined,
  accountIdB64: string,
): GuestAccountState | null {
  let raw: string | null;
  try {
    raw = storage?.getItem(markerKey(accountIdB64)) ?? null;
  } catch {
    return null;
  }
  if (raw === null) return null;
  if (raw === MARK_GUEST) return { claimedUsername: null };
  if (raw.startsWith(MARK_NAMED_PREFIX)) {
    const name = raw.slice(MARK_NAMED_PREFIX.length);
    return { claimedUsername: name.length > 0 ? name : null };
  }
  // An unknown value is not a guest: never offer to delete an account on the
  // strength of bytes this build did not write.
  return null;
}

/** Forget that `accountIdB64` is a guest: it became an account, or it is
 *  being removed from this device. */
export function clearGuestAccount(
  storage: GuestMarkerStorage | undefined,
  accountIdB64: string,
): void {
  try {
    storage?.removeItem(markerKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}

/** What a screen shows a guest. */
export type GuestScreen =
  /** Not a guest, or there is nothing to say: the screen's own content. */
  | "none"
  /** A call is running. The guest is in it; the only thing to offer is the
   *  way back to it. Deleting the account under a live call is never one
   *  press away. */
  | "in-call"
  /** No call is running: `Create account` or `No, thanks`. */
  | "offer";

/**
 * What a signed-in screen shows in place of the app.
 *
 * A guest account has no conversations and no contacts, so the app behind it
 * is an empty shell; a guest never lands in it. While a call runs the person
 * is sent back to the call, and once there is none they are asked the one
 * question.
 */
export function guestScreen(input: {
  readonly guest: boolean;
  readonly callInProgress: boolean;
}): GuestScreen {
  if (!input.guest) return "none";
  return input.callInProgress ? "in-call" : "offer";
}
