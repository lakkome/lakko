// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057, the second security pass (finding 1 + LOW 7): repair a journal a
 * lower rank pushed ahead.
 *
 * Every commit registers its next epoch in APPEND mode. A member-rank
 * registrar may append at most one epoch past the newest admin or root
 * registration, but that is still enough to put a key at `N+1` that no commit
 * on the journal ever reaches (a member that registered and then never
 * published, or published garbage). Every device that stays at `N` then finds
 * its own Append refused (409), and once the fake epoch pushes `N` out of the
 * write tier its signed writes are refused (401).
 *
 * The owner or an admin repairs it from above:
 *
 *   - **Triggers.** (a) this device's Append got 409; (b) a signed request of
 *     this device at its current epoch got 401 after everything else was
 *     tried (the DS client's last word). Either only NOTES the journal; a
 *     plain member never repairs (its note is dropped).
 *   - **The check, 30 s later** ({@link JOURNAL_REPAIR_DELAY_MS}, chosen, not
 *     measured: long enough for a real committer's publish to follow its
 *     registration). Poll the group to the journal head; if the group's epoch
 *     is still the `N` noted (no commit reached `N+1` on the journal), send
 *     `replaceEpoch(journal, N, keyAtN)`. 204: the fake epoch is gone. 403:
 *     an equal or higher rank registered above, which is a real epoch this
 *     device will reach by polling; stop.
 *
 * Never part of a commit path: the commit that met 409 was abandoned before
 * anything was noted, and the Replace re-registers only the epoch this
 * device is already at.
 *
 * A group with no head poller (the DeviceGroup, a PublisherGroup) is never
 * repaired: every registrar there is the root (every device of the account;
 * the channel owner), so there is no lower rank above to replace.
 *
 * Every step is logged at info level; no key material is logged.
 */
import { journalHex } from "./access";
import { base64Encode } from "../auth/account-id";
import { conversationBehind } from "../chat/conversation-behind";
import { registeredCommitHolds } from "../chat/registered-commit-hold";
import {
  RANK_ADMIN,
  isStatus,
  releaseAuthority,
  replaceEpochWith,
  type JournalAuthority,
  type StatementSender,
} from "./ownership";

/** How long after a trigger the check runs. Chosen, not measured. */
export const JOURNAL_REPAIR_DELAY_MS = 30_000;

/** How many immediate re-polls the head poll may take before giving up on
 *  reaching the head this round (each poll applies one page). */
export const JOURNAL_REPAIR_MAX_POLLS = 20;

/** Why a journal was noted. */
export type RepairReason = "append-conflict" | "refused-at-head";

/** One group's head poll, registered by whoever holds the live group. */
export interface HeadPoller {
  /** `true` when this poller holds the group. */
  owns(groupId: Uint8Array): boolean;
  /** Poll the group's journal to its head and apply everything; resolves when
   *  no more rows remain. Throws the DS refusal or the network failure. */
  pollToHead(groupId: Uint8Array): Promise<void>;
}

/** What the check needs from the session. */
export interface JournalRepairPorts {
  /** This device's authority now; the check releases it. */
  authority(groupId: Uint8Array): JournalAuthority;
  /** The group's current epoch and the access key registered for it, or
   *  `null` when the group is not held (or cannot derive: evicted). */
  currentEpochKey(groupId: Uint8Array): { readonly epoch: bigint; readonly key: Uint8Array } | null;
  readonly sender: StatementSender;
  /** ADR-0091 phase 1: `true` when this device holds a registered commit of
   *  the group, or is behind at or above `epoch`. The epochs above are then
   *  real and a Replace would cut them. Defaults to the chat poll's holds. */
  registeredCommitKnown?(groupId: Uint8Array, epoch: bigint): boolean;
  /** `setTimeout`, injectable for tests; returns the canceller. */
  schedule?(run: () => void, ms: number): () => void;
}

/** What one check came to. */
export type RepairOutcome =
  /** 204: the epochs above `N` were dropped. */
  | "replaced"
  /** 403: an equal or higher rank registered above; stop. */
  | "outranked"
  /** The group's epoch moved past `N` (or away from it): nothing to repair. */
  | "moved"
  /** This device holds no admin or root authority (any more). */
  | "not-permitted"
  /** No poller holds the group, so its head could not be checked. */
  | "no-poller"
  /** The head poll failed for a reason other than the journal refusing this
   *  device (offline, 5xx): no Replace on a head we could not read. */
  | "poll-failed"
  /** The group is no longer held here, or its key could not be derived. */
  | "no-key"
  /** ADR-0091 phase 1: this device holds a registered commit of the group
   *  above this epoch (or is behind there): the epochs above are real. */
  | "registered-commit-known"
  /** The Replace failed for any other reason. */
  | "failed";

/** The root or an admin registrar repairs; a member never does. */
export function mayRepair(authority: JournalAuthority): boolean {
  return authority.kind === "root" || (authority.kind === "registrar" && authority.rank === RANK_ADMIN);
}

/** The HTTP status anywhere in a wrapped error chain, or `null`. */
function statusDeep(err: unknown): number | null {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== undefined && current !== null; depth += 1) {
    if (typeof current !== "object") return null;
    const r = current as { status?: unknown; cause?: unknown; variant?: { cause?: unknown } };
    if (typeof r.status === "number") return r.status;
    current = r.cause ?? r.variant?.cause;
  }
  return null;
}

function log(message: string): void {
  // eslint-disable-next-line no-console
  console.info(`ds repair: ${message}`);
}

/** The per-process repair scheduler. One instance is the app's
 *  ({@link journalRepair}); tests build their own. */
export class JournalRepair {
  private readonly pollers = new Set<HeadPoller>();
  private readonly pending = new Map<string, () => void>();
  private ports: JournalRepairPorts | null = null;

  /** Install the session's ports (or clear with `null`, which cancels every
   *  scheduled check). */
  install(ports: JournalRepairPorts | null): void {
    for (const cancel of this.pending.values()) cancel();
    this.pending.clear();
    this.ports = ports;
  }

  /** Add a head poller; returns the function that removes it. */
  registerHeadPoller(poller: HeadPoller): () => void {
    this.pollers.add(poller);
    return () => {
      this.pollers.delete(poller);
    };
  }

  private pollerFor(groupId: Uint8Array): HeadPoller | null {
    for (const p of this.pollers) {
      try {
        if (p.owns(groupId)) return p;
      } catch {
        /* a failing source holds nothing */
      }
    }
    return null;
  }

  /**
   * Note a trigger at epoch `epoch` (the epoch this device is at). Schedules
   * one check {@link JOURNAL_REPAIR_DELAY_MS} later when this device is the
   * root or an admin of a group a poller holds and no check is pending for
   * the group. Returns whether a check was scheduled. Never throws.
   */
  note(groupId: Uint8Array, epoch: bigint, reason: RepairReason): boolean {
    const ports = this.ports;
    if (ports === null) return false;
    const hex = journalHex(groupId);
    if (this.pending.has(hex)) return false;
    if (this.pollerFor(groupId) === null) return false;
    let authority: JournalAuthority = { kind: "none" };
    try {
      authority = ports.authority(groupId);
      if (!mayRepair(authority)) return false;
    } catch {
      return false;
    } finally {
      releaseAuthority(authority);
    }
    const id = groupId.slice();
    log(`${reason} at epoch ${epoch}; checking in ${JOURNAL_REPAIR_DELAY_MS / 1000} s`);
    const schedule =
      ports.schedule ??
      ((run: () => void, ms: number): (() => void) => {
        const t = setTimeout(run, ms);
        return () => clearTimeout(t);
      });
    const cancel = schedule(() => {
      void this.check(id, epoch).finally(() => {
        this.pending.delete(hex);
      });
    }, JOURNAL_REPAIR_DELAY_MS);
    this.pending.set(hex, cancel);
    return true;
  }

  /** `true` while a check is scheduled or running for the group. */
  isPending(groupId: Uint8Array): boolean {
    return this.pending.has(journalHex(groupId));
  }

  /**
   * The check itself: poll to the head, and Replace at `epoch` only when the
   * group is still there. Exposed for tests; the scheduler calls it. Never
   * throws.
   */
  async check(groupId: Uint8Array, epoch: bigint): Promise<RepairOutcome> {
    const ports = this.ports;
    if (ports === null) return "not-permitted";
    const poller = this.pollerFor(groupId);
    if (poller === null) {
      log("no head poller for the group; not repaired");
      return "no-poller";
    }
    try {
      await poller.pollToHead(groupId);
      log("polled to the journal head");
    } catch (err) {
      // A journal that refuses this device at its epoch is the very state
      // being repaired; anything else means the head is unknown.
      if (statusDeep(err) !== 401) {
        log(`head poll failed (${statusDeep(err) ?? "network"}); not repaired`);
        return "poll-failed";
      }
      log("head poll refused (401)");
    }
    let current: { readonly epoch: bigint; readonly key: Uint8Array } | null;
    try {
      current = ports.currentEpochKey(groupId);
    } catch {
      current = null;
    }
    if (current === null) return "no-key";
    if (current.epoch !== epoch) {
      log(`the group moved from epoch ${epoch} to ${current.epoch}; nothing to repair`);
      return "moved";
    }
    let known: boolean;
    try {
      known =
        ports.registeredCommitKnown?.(groupId, epoch) ??
        (registeredCommitHolds.blocksRepair(base64Encode(groupId), epoch) ||
          // ADR-0091 section 2 — confirmed behind is evidence that the epochs
          // above are real: a Replace would cut them.
          conversationBehind.isConfirmed(base64Encode(groupId)));
    } catch {
      // Unknown is not "nothing above": no Replace.
      known = true;
    }
    if (known) {
      log("a registered commit is known above this epoch; not repaired");
      return "registered-commit-known";
    }
    const authority = ports.authority(groupId);
    try {
      if (!mayRepair(authority)) return "not-permitted";
      log(`replacing at epoch ${epoch}`);
      await replaceEpochWith(ports.sender, groupId, authority, epoch, current.key);
      log(`replaced at epoch ${epoch}; the epochs above are gone`);
      return "replaced";
    } catch (err) {
      if (isStatus(err, 403)) {
        log("replace refused (403): an equal or higher rank registered above; stopping");
        return "outranked";
      }
      log(`replace failed (${statusDeep(err) ?? "network"})`);
      return "failed";
    } finally {
      releaseAuthority(authority);
    }
  }
}

/** The app's scheduler; the session installs its ports
 *  (`JournalOwnershipContext`). */
export const journalRepair = new JournalRepair();
