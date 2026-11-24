// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.2 / R.7 case 1 — what a send into a one-to-one decides before it
 * goes, written once for the two senders: the open chat view
 * (`routes/Chat.tsx`) and the app's outbox runner
 * (`UnsentOutboxContext.tsx`, `docs/ux/connection-state.md` section 7.3).
 *
 * 1. The peer's published identity is checked when the last check of this
 *    conversation is older than the re-admission interval, waited for at
 *    most {@link SEND_IDENTITY_CHECK_WAIT_MS} (an accepted reset re-pins and
 *    starts the conversation's move; a later answer still takes effect).
 * 2. Then the route: a message is never sent into a session only removed
 *    devices can read. It is held until the successor exists, or reads
 *    `Not sent` after the give-up (`held-send.ts` `heldVerdict`).
 *
 * The check's last time is kept per conversation for the whole page, so the
 * two senders share it.
 */

import { base64Decode } from "../auth/account-id";
import { accountIdB64OfKey } from "../contacts/local-store";
import { READMISSION_INTERVAL_MS } from "./device-readmission";
import { heldVerdict, identityCheckDue, sendRoute, type HeldVerdict } from "./held-send";
import { unrepairableBy } from "./successor-claim";

/** How long a send waits for the identity check before it goes ahead.
 *  Chosen: long enough for one directory fetch, short enough to be
 *  unnoticed. */
export const SEND_IDENTITY_CHECK_WAIT_MS = 3000;

/** When this page last ran the identity check of a one-to-one's peer, by
 *  conversation id (hex). */
const lastPeerIdentityCheckAtMs = new Map<string, number>();

/** The directory check of one person (`ContactsClient.checkContactIdentity`). */
export type ContactIdentityCheckFn = (accountId: Uint8Array, peerKeyB64: string) => Promise<unknown>;

/**
 * Run the peer's identity check when it is due (or `force`). Silent whatever
 * it answers; never throws.
 */
export async function checkPeerIdentityIfDue(input: {
  readonly conversationHex: string;
  readonly peerKeyB64: string | null;
  readonly check: ContactIdentityCheckFn;
  readonly force?: boolean;
  readonly nowMs?: number;
}): Promise<void> {
  const now = input.nowMs ?? Date.now();
  if (
    input.force !== true &&
    !identityCheckDue(lastPeerIdentityCheckAtMs.get(input.conversationHex), now, READMISSION_INTERVAL_MS)
  ) {
    return;
  }
  const peer = input.peerKeyB64;
  if (peer === null) return;
  const accountIdB64 = accountIdB64OfKey(peer);
  if (accountIdB64 === null) return;
  lastPeerIdentityCheckAtMs.set(input.conversationHex, now);
  try {
    await input.check(base64Decode(accountIdB64), peer);
  } catch {
    // Refused, inconclusive or offline: nothing changes, nothing is shown.
  }
}

/** The peer of a one-to-one: the one roster person who is not this account. */
export function oneToOnePeerKey(rosterPersonKeysB64: readonly string[], ownPersonKeyB64: string | null): string | null {
  const peers = rosterPersonKeysB64.filter((k) => k !== ownPersonKeyB64);
  return peers.length === 1 ? (peers[0] ?? null) : null;
}

/** The superseded account that makes a one-to-one unrepairable, or `null`. */
export function oneToOneUnrepairableKey(input: {
  readonly rosterPersonKeysB64: readonly string[];
  readonly ownPersonKeyB64: string | null;
  readonly supersededKeys: ReadonlySet<string> | ReadonlyMap<string, unknown>;
}): string | null {
  return unrepairableBy({
    kind: "chat",
    rosterPersonKeysB64: input.rosterPersonKeysB64,
    ownPersonKeyB64: input.ownPersonKeyB64,
    ownerKeyB64: undefined,
    supersededKeys: input.supersededKeys,
  });
}

/**
 * The decision for one send into a one-to-one: the bounded identity check,
 * then the route read AFTER it (an accepted reset may have just re-pinned).
 */
export async function oneToOneSendVerdict(input: {
  /** The due-or-not identity check ({@link checkPeerIdentityIfDue}). */
  readonly check: () => Promise<void>;
  /** Read after the check: the key that makes the session unrepairable. */
  readonly unrepairableKey: () => string | null;
  /** When the message was first held (the give-up counts from then). */
  readonly heldAtMs: number;
  readonly now?: () => number;
  readonly waitMs?: number;
}): Promise<HeldVerdict> {
  const waitMs = input.waitMs ?? SEND_IDENTITY_CHECK_WAIT_MS;
  let timer: ReturnType<typeof setTimeout> | undefined;
  await Promise.race([
    input.check(),
    new Promise<void>((resolve) => {
      timer = setTimeout(resolve, waitMs);
    }),
  ]);
  if (timer !== undefined) clearTimeout(timer);
  const route = sendRoute({ kind: "chat", unrepairableKeyB64: input.unrepairableKey() });
  return heldVerdict({ route, heldAtMs: input.heldAtMs, nowMs: (input.now ?? Date.now)() });
}

/** Tests only. */
export function resetPeerIdentityChecksForTest(): void {
  lastPeerIdentityCheckAtMs.clear();
}
