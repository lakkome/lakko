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
 *   day after it was made, so closing the tab leaves nothing behind either;
 * - the device follows the same clock: a guest account older than
 *   {@link GUEST_LIFETIME_MS} is deleted the next time the app opens, and a
 *   guest whose session can no longer be restored is wiped from the device.
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

/**
 * How long a guest account lives on a device before the device deletes it
 * unasked.
 *
 * The servers delete an unkept account a day after it was made. The device
 * stops an hour earlier, so `Create account` is never offered for an account
 * the servers are about to remove under it. Chosen, not measured.
 */
export const GUEST_LIFETIME_MS = 23 * 60 * 60 * 1000;

/** What the device knows about a guest account. */
export interface GuestAccountState {
  /** When this device marked the account as a guest (its creation, to the
   *  second or so), unix milliseconds. */
  readonly markedAtMs: number;
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

// `guest@<ms>` or `named@<ms>:<username>`. A username is `[a-z0-9_]`, so the
// first `:` after the stamp ends it.
const MARK_GUEST_PREFIX = "guest@";
const MARK_NAMED_PREFIX = "named@";

function parseStamp(text: string): number | null {
  if (!/^[0-9]{1,16}$/.test(text)) return null;
  const ms = Number(text);
  return Number.isSafeInteger(ms) ? ms : null;
}

/**
 * The phrase store key a guest's recovery phrase is kept under.
 *
 * Not the account id: see the module comment.
 */
export function guestPhraseKey(accountIdB64: string): string {
  return `guest:${accountIdB64}`;
}

/** Record that `accountIdB64` is a guest account, made at `nowMs`. */
export function markGuestAccount(
  storage: GuestMarkerStorage | undefined,
  accountIdB64: string,
  nowMs: number,
): void {
  try {
    storage?.setItem(markerKey(accountIdB64), `${MARK_GUEST_PREFIX}${Math.trunc(nowMs)}`);
  } catch {
    // Storage blocked: the account then reads as an ordinary one with no
    // username, which is what it is. It is still deleted after a day.
  }
}

/**
 * Record the username taken for a guest account that is being kept. The
 * account keeps the age it had: taking a name does not buy it another day.
 * No-op for an account that is not marked as a guest.
 */
export function noteGuestUsernameClaimed(
  storage: GuestMarkerStorage | undefined,
  accountIdB64: string,
  username: string,
): void {
  const current = guestAccountState(storage, accountIdB64);
  if (current === null) return;
  try {
    storage?.setItem(
      markerKey(accountIdB64),
      `${MARK_NAMED_PREFIX}${current.markedAtMs}:${username}`,
    );
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
  if (raw.startsWith(MARK_GUEST_PREFIX)) {
    const markedAtMs = parseStamp(raw.slice(MARK_GUEST_PREFIX.length));
    return markedAtMs === null ? null : { markedAtMs, claimedUsername: null };
  }
  if (raw.startsWith(MARK_NAMED_PREFIX)) {
    const rest = raw.slice(MARK_NAMED_PREFIX.length);
    const colon = rest.indexOf(":");
    if (colon < 0) return null;
    const markedAtMs = parseStamp(rest.slice(0, colon));
    const name = rest.slice(colon + 1);
    if (markedAtMs === null) return null;
    return { markedAtMs, claimedUsername: name.length > 0 ? name : null };
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

/**
 * Whether a guest account has outlived {@link GUEST_LIFETIME_MS} on this
 * device.
 *
 * A clock that reads earlier than the mark (it was set back) says nothing
 * about the account's age, so it is not expired on that evidence.
 */
export function guestExpired(guest: GuestAccountState, nowMs: number): boolean {
  return nowMs - guest.markedAtMs >= GUEST_LIFETIME_MS;
}

/** What a page load does about a guest account this browser holds. */
export type GuestRestoreAction =
  /** Not a guest, or a guest young enough to go on: the ordinary restore. */
  | "none"
  /** A guest past its lifetime whose session can still be restored: delete
   *  it on the servers (best effort), then wipe the device. */
  | "discard"
  /** A guest whose session cannot be restored (the token expired, a wrapped
   *  store is unreadable): nothing can sign in to it from here and nothing
   *  can show its phrase, so every trace of it leaves the device. The servers
   *  remove their side on their own clock. */
  | "wipe";

/**
 * Decide it. `restorable` is the ordinary restore's own verdict: every piece
 * a silent restore needs is present.
 *
 * A guest account never ends up as a cached, signed-out account: there is no
 * password anyone could type to get back into it.
 */
export function guestRestoreAction(input: {
  readonly guest: GuestAccountState | null;
  readonly restorable: boolean;
  readonly nowMs: number;
}): GuestRestoreAction {
  if (input.guest === null) return "none";
  if (!input.restorable) return "wipe";
  return guestExpired(input.guest, input.nowMs) ? "discard" : "none";
}

/**
 * The order `No, thanks` deletes in, and which steps may fail quietly.
 *
 * An unnamed guest: the auth service decides (once the account is gone there,
 * nothing can sign in to it), and the directory is best effort, since its own
 * sweep removes an entry with no username.
 *
 * A guest for whom a username was already taken: the directory goes FIRST and
 * must succeed. Its sweep never removes an entry that has a username, so a
 * directory delete skipped here would leave that name taken for good. Both
 * deletes are idempotent, so a retry after a half-finished attempt completes.
 */
export function guestDeletePlan(guest: GuestAccountState): readonly {
  readonly service: "auth" | "directory";
  readonly required: boolean;
}[] {
  if (guest.claimedUsername === null) {
    return [
      { service: "auth", required: true },
      { service: "directory", required: false },
    ];
  }
  return [
    { service: "directory", required: true },
    { service: "auth", required: true },
  ];
}
