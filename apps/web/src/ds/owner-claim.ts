// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 obligation 2 - the owner claims its journal when it OPENS the
 * group, not only after a signed request was refused.
 *
 * ## Why the lazy claim alone was not enough
 *
 * The DS client claims lazily: a signed request that meets `401` makes the
 * owner claim and retry (`client.ts`). But an UNCLAIMED journal never answers
 * `401` - it is open, and serves every request - so a journal whose claim was
 * dropped (the live walk of 2026-09-29: DS migration `0006` dropped every
 * earlier claim) stayed unclaimed forever. Nobody could then register an
 * epoch in it: every ADR-0045 sweep met `401` on `PUT .../access`, every ~5 s,
 * in every pre-existing group, and a device added through the ADR-0049 link
 * was never admitted to any chat.
 *
 * So the owner sends the claim for the epoch its group is at the first time
 * it opens the group in a session: a conversation group (the ownership
 * provider's session-start reconcile, and before any registration), the
 * DeviceGroup (its sync provider), a channel's PublisherGroup (the owner's
 * Manage screen). The claim is idempotent on the server:
 *
 *   - `204` - claimed now, or already claimed under this root with this key
 *     at this epoch. This account holds the root: the caller marks it held
 *     and runs the registrar reconcile, exactly as after a lazy claim;
 *   - `409` - claimed under this root with another chain, or the chain moved
 *     on. Left alone: never a reset from here (a conversation group's reset
 *     would drop every registration its members made);
 *   - `401` - claimed under ANOTHER root (an ownership transfer whose
 *     takeover has not landed). Left alone too; the takeover owns that case.
 *
 * Each of those settles the group for the session. Anything else (a network
 * failure, a 5xx) is retried on a later open, no sooner than
 * {@link OWNER_CLAIM_RETRY_MS}.
 */

/** Lowercase hex of a journal id. Kept local: `./access` imports this module
 *  and instantiates the memo at load time, so importing back would cycle. */
function journalHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) out += (bytes[i] ?? 0).toString(16).padStart(2, "0");
  return out;
}

/** A failed claim (network, 5xx) is retried on a later open no sooner than
 *  this. Chosen, not measured. */
export const OWNER_CLAIM_RETRY_MS = 60_000;

/** What {@link OwnerClaimMemo.run} did. */
export type OwnerClaimOutcome =
  /** This account does not own the journal (or no source knows the group). */
  | "not-owner"
  /** The group's current epoch key could not be derived; nothing sent. */
  | "no-epoch-key"
  /** `204`: the root is held. */
  | "claimed"
  /** `409`: claimed under this root with another chain; left alone. */
  | "already-claimed"
  /** `401`: claimed under another root; left alone. */
  | "claimed-by-another-root"
  /** Settled earlier this session; nothing sent. */
  | "settled"
  /** An earlier attempt failed less than {@link OWNER_CLAIM_RETRY_MS} ago. */
  | "deferred"
  /** The claim could not be sent or the server failed; retried later. */
  | "failed";

/** What the claim needs from the outside world. */
export interface OwnerClaimPorts {
  /** Does this account own the journal (`DsAccessRegistry.ownsJournal`)? */
  ownsJournal(groupId: Uint8Array): boolean;
  /** The epoch the group is at and its access key, or `null`. */
  currentEpochKey(groupId: Uint8Array): { epoch: bigint; key: Uint8Array } | null;
  /** Send the claim signed with this account's ROOT. Resolves on `204`,
   *  throws the DS refusal (carrying `status`) or a network error. */
  claim(groupId: Uint8Array, epoch: bigint, key: Uint8Array): Promise<void>;
  /** The claim landed (`204`): mark the root held, run the reconcile. */
  onClaimed(groupId: Uint8Array): void;
  now(): number;
}

function statusOf(err: unknown): number | null {
  if (err !== null && typeof err === "object") {
    const status = (err as { status?: unknown }).status;
    if (typeof status === "number") return status;
  }
  return null;
}

interface Entry {
  settled: boolean;
  inFlight: Promise<OwnerClaimOutcome> | null;
  notBefore: number;
}

/** The per-session record of which journals the owner already claimed. One
 *  instance lives in the DS access registry, so every `DsClient` shares it. */
export class OwnerClaimMemo {
  private readonly entries = new Map<string, Entry>();

  /** Claim `groupId` once per session when this account owns it. Never
   *  throws. Concurrent calls for one group share one request. */
  /** Forget what this session settled for `groupId` (its chain was
   *  replaced): the next open claims it again. */
  forget(groupId: Uint8Array): void {
    this.entries.delete(journalHex(groupId));
  }

  run(groupId: Uint8Array, ports: OwnerClaimPorts): Promise<OwnerClaimOutcome> {
    const hex = journalHex(groupId);
    let entry = this.entries.get(hex);
    if (entry?.settled === true) return Promise.resolve("settled");
    if (entry?.inFlight !== null && entry?.inFlight !== undefined) return entry.inFlight;
    if (entry !== undefined && ports.now() < entry.notBefore) return Promise.resolve("deferred");
    let owns = false;
    try {
      owns = ports.ownsJournal(groupId);
    } catch {
      owns = false;
    }
    // Not remembered: ownership can arrive later in the session (a transfer).
    if (!owns) return Promise.resolve("not-owner");
    let current: { epoch: bigint; key: Uint8Array } | null = null;
    try {
      current = ports.currentEpochKey(groupId);
    } catch {
      current = null;
    }
    if (current === null) return Promise.resolve("no-epoch-key");
    if (entry === undefined) {
      entry = { settled: false, inFlight: null, notBefore: 0 };
      this.entries.set(hex, entry);
    }
    const held = entry;
    const { epoch, key } = current;
    held.inFlight = (async (): Promise<OwnerClaimOutcome> => {
      try {
        await ports.claim(groupId, epoch, key);
        held.settled = true;
        try {
          ports.onClaimed(groupId);
        } catch {
          /* the listeners are best effort */
        }
        return "claimed";
      } catch (err) {
        const status = statusOf(err);
        if (status === 409) {
          held.settled = true;
          return "already-claimed";
        }
        if (status === 401) {
          held.settled = true;
          return "claimed-by-another-root";
        }
        held.notBefore = ports.now() + OWNER_CLAIM_RETRY_MS;
        return "failed";
      } finally {
        held.inFlight = null;
      }
    })();
    return held.inFlight;
  }

  /** Record a claim that landed elsewhere (the lazy claim after a `401`). */
  noteClaimed(groupId: Uint8Array): void {
    const hex = journalHex(groupId);
    const entry = this.entries.get(hex);
    if (entry === undefined) {
      this.entries.set(hex, { settled: true, inFlight: null, notBefore: 0 });
    } else {
      entry.settled = true;
    }
  }

  /** Forget everything: sign-out. */
  clear(): void {
    this.entries.clear();
  }
}
