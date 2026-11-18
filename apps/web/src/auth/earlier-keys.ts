// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 "Joining after a rotation: the device waits for the earlier
 * keys" (contract G-56 sections 2 and 4; UX `identity-recovery.md` 8a).
 *
 * After a proven key rotation every conversation of the account stays bound
 * to the key of before it (the conversation key `C`, the oldest own
 * predecessor). A device that joins the account and does not hold `C` must
 * not bind its device identity, publish a KeyPackage, join a conversation or
 * open the DeviceGroup: it would be another account to its own siblings, for
 * good. It waits instead, signed in, in the parked state, until the earlier
 * keys reach it (a pairing recovery row, the account backup) or the person
 * resets the account keys. No time limit, never a fallback to the current
 * key.
 *
 * This module holds the two orderings, with every effect injected so they
 * can be tested without the auth provider:
 *
 *   - {@link runEarlierKeysGate}: the landing's gate, run BEFORE the
 *     session's identities are installed. The verdict is the core's
 *     (`devices/own-conversation-key.ts`); the bundle is read only when the
 *     verdict with the recorded link (or none) is not "kept", so a warm
 *     landing costs no request.
 *   - {@link tryEarlierKeysOnce}: one look of the parked device for the
 *     earlier keys (the backup with the phrase it holds, then a re-read of
 *     the own bundle), landing when the gate lets it.
 */
import {
  OWN_LINK_ABSENT,
  type BindingDecision,
  type OwnLink,
} from "../devices/own-conversation-key";

/** Thrown by the session install when its verdict is `wait`: nothing was
 *  minted, stored or published. The landing parks the device. */
export class EarlierKeysMissingError extends Error {
  override name = "EarlierKeysMissingError";
  constructor() {
    super("the account's earlier keys are not on this device yet");
  }
}

/** How often the parked screen looks for the earlier keys while it is in
 *  the foreground (chosen, not measured; contract section 4.2). */
export const EARLIER_KEYS_RETRY_MS = 5 * 60 * 1000;

/** The facts of the stored binding the verdict reads. */
export interface BindingFacts {
  /** The account key the stored binding names; `null` when none. */
  readonly boundKey: Uint8Array | null;
  readonly currentKey: Uint8Array;
  /** The own predecessor keys this device holds, oldest first. */
  readonly predecessorKeys: readonly Uint8Array[];
  readonly holdsConversations: boolean;
}

export interface EarlierKeysGateDeps {
  /** The binding facts; `null` when the device has no device key (nothing
   *  can be bound, so nothing is gated). */
  readFacts(): Promise<BindingFacts | null>;
  /** The recorded own link, `null` when never read. May throw. */
  readRecord(): Promise<OwnLink | null>;
  /** Read the own bundle's link: `unknown` when it could not be read. */
  fetchLink(): Promise<OwnLink>;
  /** Record an answer (`unknown` records nothing). Never throws. */
  writeRecord(link: OwnLink): Promise<void>;
  verdict(facts: BindingFacts, link: OwnLink): BindingDecision;
}

export type EarlierKeysGateAnswer =
  | { readonly kind: "land"; readonly link: OwnLink }
  | { readonly kind: "wait"; readonly link: OwnLink };

/**
 * The gate. `input.link` is the answer a route already read with its seed
 * check (no second fetch); `input.exempt` is the signup, the guest signup
 * and the landing after a key reset or recall on this device (no link can
 * name a key they lack).
 */
export async function runEarlierKeysGate(
  deps: EarlierKeysGateDeps,
  input: { readonly link?: OwnLink; readonly exempt?: boolean } = {},
): Promise<EarlierKeysGateAnswer> {
  if (input.exempt === true) return { kind: "land", link: OWN_LINK_ABSENT };
  const facts = await deps.readFacts();
  if (facts === null) return { kind: "land", link: input.link ?? OWN_LINK_ABSENT };
  let link = input.link;
  if (link === undefined) {
    let recorded: OwnLink | null = null;
    try {
      recorded = await deps.readRecord();
    } catch {
      recorded = null;
    }
    const first = deps.verdict(facts, recorded ?? OWN_LINK_ABSENT);
    if (first.isKept) return { kind: "land", link: recorded ?? OWN_LINK_ABSENT };
    link = await deps.fetchLink();
  }
  await deps.writeRecord(link);
  const decision = deps.verdict(facts, link);
  return decision.kind === "wait" ? { kind: "wait", link } : { kind: "land", link };
}

export interface EarlierKeysRetryDeps {
  /** The device is still parked for this account. Asked between steps. */
  stillParked(): boolean;
  /** Source 1: fetch the account backup with the phrase this device holds
   *  and store its own predecessors. Never throws. */
  fetchBackup(): Promise<void>;
  /** Source 2: re-read the own bundle's link (`unknown` on failure). */
  fetchLink(): Promise<OwnLink>;
  /** The landing's gate with that link. */
  gate(link: OwnLink): Promise<EarlierKeysGateAnswer>;
  /** Land the session the device parked (it binds under `C` now). */
  land(link: OwnLink): Promise<void>;
}

/** One look for the earlier keys. A failed look is not shown. */
export async function tryEarlierKeysOnce(
  deps: EarlierKeysRetryDeps,
): Promise<"landed" | "waiting" | "stopped"> {
  if (!deps.stillParked()) return "stopped";
  await deps.fetchBackup();
  if (!deps.stillParked()) return "stopped";
  const link = await deps.fetchLink();
  if (!deps.stillParked()) return "stopped";
  const answer = await deps.gate(link);
  if (answer.kind === "wait") return "waiting";
  if (!deps.stillParked()) return "stopped";
  await deps.land(answer.link);
  return "landed";
}

/**
 * The joining device's persist order on every recovery route (contract
 * section 3.3 step 4): the earlier keys the recovery row carried FIRST,
 * then the account seed, the phrase, and the rest of the join state; the
 * landing (and its gate) runs after all of them. Each step does its own
 * binding checks; a step that throws stops the order.
 */
export async function persistRecoveredKeysInOrder(steps: {
  readonly predecessors: () => Promise<void>;
  readonly seed: () => Promise<void>;
  readonly phrase: () => Promise<void>;
  readonly rest: () => Promise<void>;
}): Promise<void> {
  await steps.predecessors();
  await steps.seed();
  await steps.phrase();
  await steps.rest();
}

/**
 * At most one landing of a parked account at a time (the parked device's own
 * look for the earlier keys, an "Another device" completion, a typed phrase,
 * a device restore). A completion the person started waits for a look in
 * progress and then runs; a look skips while anything is in flight. Each
 * landing re-checks that its account is still parked once it holds the
 * guard (a look that landed first leaves nothing parked).
 */
export class ParkedLandingGuard {
  private tail: Promise<void> = Promise.resolve();
  private inFlight = 0;

  /** Whether a landing holds or waits for the guard. */
  get busy(): boolean {
    return this.inFlight > 0;
  }

  /** Resolves once every landing that holds or waits for the guard now has
   *  finished. Claims nothing. */
  idle(): Promise<void> {
    return this.tail;
  }

  /** Run `fn` alone, after whatever holds the guard now. */
  async runExclusive<T>(fn: () => Promise<T>): Promise<T> {
    const previous = this.tail;
    let release: () => void = () => undefined;
    this.tail = new Promise<void>((resolve) => {
      release = resolve;
    });
    this.inFlight += 1;
    try {
      await previous;
      return await fn();
    } finally {
      this.inFlight -= 1;
      release();
    }
  }

  /** Run `fn` only when nothing holds or waits for the guard; `null` when
   *  skipped. The check and the claim are one synchronous step. */
  tryRun<T>(fn: () => Promise<T>): Promise<T> | null {
    if (this.inFlight > 0) return null;
    return this.runExclusive(fn);
  }
}
