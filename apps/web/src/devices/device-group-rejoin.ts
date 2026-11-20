// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 decision 4 - a device that lost or fell behind its DeviceGroup
 * rejoins through a sibling instead of re-founding.
 *
 * ## When a device rejoins
 *
 * - **Lost:** it has no usable DeviceGroup state - no snapshot, or one that
 *   does not decrypt under its device key or does not restore - while the
 *   account's directory lists another own device (`lifecycle.ts` throws
 *   `DeviceGroupNotJoinedError("rejoin")`). With no other device listed it
 *   founds instead: nobody can be cut off.
 * - **Behind:** a signed read of its DeviceGroup journal after its cursor
 *   returned no rows and nothing more, the DS names a newer epoch than its
 *   handle's, and it has no commit of its own in flight
 *   ({@link isBehind}). Since ADR-0081 decision 1 an epoch is registered
 *   only together with the commit row that reaches it, so a drained journal
 *   that is still ahead means the device cannot apply a row it has already
 *   passed (its own commit read back, a row it could not decrypt, a journal
 *   pruned past it): nothing it will ever read moves it. No grace period is
 *   needed for a commit "still on its way" any more; the one exception is
 *   this device's own outbox entry, which is finished first.
 *
 * ## The device's side (ADR-0081 third amendment, 2026-11-11)
 *
 * 1. Delete the local DeviceGroup state (snapshot, pending commit) and make
 *    sure its own directory KeyPackage is published (ADR-0050; it carries the
 *    signed device binding, published under the device-bound session).
 * 2. Build a DeviceGroup KeyPackage from the same device identity (same leaf
 *    key) and the ADR-0056 pairing payload: login handle, KeyPackage and a
 *    fresh rendezvous id. The DeviceGroup id is derived from the account seed
 *    (ADR-0078 phase 6). The joiner's provider and the rendezvous are kept
 *    (`pending`, `rejoin:`) so a reload keeps waiting for the same Welcome.
 * 3. Open the long poll on the rendezvous, THEN post the knock (ADR-0053, one
 *    `ds-knock` token), measuring the knock request's own round trip.
 * 4. Wait {@link answerWindowMs} for a presence note (`isRejoinPresent`) or a
 *    Welcome. A presence note means a device of the account is running: wait
 *    for its Welcome with no deadline, knocking again with a fresh KeyPackage
 *    after {@link REKNOCK_AFTER_MS}. Silence means every listed device is
 *    gone or not running: this device founds the DeviceGroup (a root reset of
 *    the journal it is claimed under) and starts at once.
 *
 * An account with no username this device knows has no login handle a
 * sibling could match, so it does not knock. It does not found either: a
 * rejoin runs only while another device of the account is listed, and a
 * founding there splits the account into two DeviceGroups that cannot admit
 * each other (each founding resets the journal under the other; seen
 * 2026-11-15 on an iPhone after a passkey sign-in with no name typed whose
 * backup did not open). The pass ends `"no-handle"` and the caller looks for
 * the handle again after {@link NO_HANDLE_RETRY_MS}.
 *
 * ## A sibling's side
 *
 * A device holding the DeviceGroup keeps a long poll on its knocks while it
 * is in the foreground. On a knock whose payload names this account's login
 * handle and that {@link knockScreen} does not set aside, it first posts a
 * presence note to the knock's rendezvous (before any directory lookup, so
 * the answer costs one round trip), then admits the knock when
 * {@link admissionVerdict} says so: the KeyPackage's leaf key is listed in the
 * account's directory with a device binding that verifies under the account
 * key (a deleted device is tombstoned there, ADR-0074, and not listed); the
 * knock is newer than every addition of that key this device has seen and
 * newer than when it started watching. Every running device that passes the
 * checks admits at once: the DS commit route lets exactly one commit take the
 * epoch, the others get `409` and drop theirs. To admit it replaces the key's
 * previous leaf, if still in the roster, by the KeyPackage in ONE commit
 * (`replaceDeviceAt`), or adds the KeyPackage when no previous leaf is there,
 * through the outbox (`device-group-commit.ts`), and the Welcome goes to the
 * knock's rendezvous.
 *
 * Under the ADR-0015 policy a removal needs a passkey attestation OR (ADR-0081
 * amendment) the device's own self-removal proof: the rejoining device signs,
 * with its old leaf key, that its leaf may be replaced by the KeyPackage it
 * knocks with, and the proof rides in the commit's removal-authorisation slot.
 * A knock without a proof that the sibling verified, whose previous leaf is
 * present, is left waiting under the policy (verdict `needs-attestation`): a
 * device that no longer holds its old key is a new device, its KeyPackage key
 * matches no leaf, and it is simply added.
 */

/** How long a rejoining device waits for a Welcome before knocking again
 *  with a fresh KeyPackage. Chosen, not measured. */
export const REKNOCK_AFTER_MS = 30 * 60_000;

/** How long a rejoining device that knows no login handle waits before
 *  looking for one again (the session's name, the own-username register a
 *  restore stored). Chosen, not measured; the same on every client. */
export const NO_HANDLE_RETRY_MS = 30_000;

/**
 * The wait between two `"no-handle"` passes, cut short when the device
 * learns its username (`docs/ux/identity-recovery.md` section 8b: the person
 * typed it, or the landing adopted the backup's): the next pass knocks at
 * once instead of up to {@link NO_HANDLE_RETRY_MS} later.
 */
export interface NoHandleWaker {
  /** Resolves after `ms`, on {@link wake}, or when `signal` aborts. */
  sleep(ms: number, signal: AbortSignal): Promise<void>;
  /** End every wait in progress. A wake with nobody waiting is not kept. */
  wake(): void;
}

export function createNoHandleWaker(): NoHandleWaker {
  const waiting = new Set<() => void>();
  return {
    sleep(ms, signal) {
      return new Promise((resolve) => {
        if (signal.aborted) {
          resolve();
          return;
        }
        let done = false;
        const finish = (): void => {
          if (done) return;
          done = true;
          clearTimeout(timer);
          waiting.delete(finish);
          signal.removeEventListener("abort", finish);
          resolve();
        };
        const timer = setTimeout(finish, ms);
        waiting.add(finish);
        signal.addEventListener("abort", finish, { once: true });
      });
    },
    wake() {
      for (const finish of [...waiting]) finish();
    },
  };
}

/** The shortest answer window ({@link answerWindowMs}). Chosen, not
 *  measured (ADR-0081 third amendment). */
export const ANSWER_WINDOW_MIN_MS = 1_000;

/** How many times the knock's own round trip the new device waits for an
 *  answer. Chosen, not measured: the answer is a DS wake, one read and one
 *  write on the sibling, and this device's read. */
export const ANSWER_WINDOW_RTT_FACTOR = 4;

/** How long the new device waits for a presence note or a Welcome after its
 *  knock came back in `rttMs`: `max(4 x rtt, 1 s)`. A round trip that cannot
 *  be read (negative, not finite) counts as zero. */
export function answerWindowMs(rttMs: number): number {
  const rtt = Number.isFinite(rttMs) && rttMs > 0 ? rttMs : 0;
  return Math.max(ANSWER_WINDOW_RTT_FACTOR * rtt, ANSWER_WINDOW_MIN_MS);
}

/** A knock already waiting when this device starts listening is answered
 *  only when it is at most this old: coming back to the app does not post
 *  presence notes to a week of old knocks (their devices founded or knocked
 *  again long ago). Chosen, not measured; the same on every client. */
export const STALE_KNOCK_AT_LISTEN_START_MS = 30_000;

/** How many refused signed reads of its DeviceGroup journal in a row make a
 *  device read its journal as reset under it (ADR-0081 third amendment,
 *  point 5). Refusals for a wrong device clock never count. Chosen, not
 *  measured. */
export const RESET_REFUSALS_BEFORE_REJOIN = 3;

/** Does this run of refused reads mean the journal was reset under this
 *  device? `consecutiveRefusals` counts signed reads refused with `401`
 *  while the device clock was right. */
export function journalResetUnder(consecutiveRefusals: number): boolean {
  return consecutiveRefusals >= RESET_REFUSALS_BEFORE_REJOIN;
}

/**
 * Is this device behind its DeviceGroup for good? `drained`: this poll's
 * fetch after the cursor returned zero rows and `has_more` was false.
 * `latestEpoch`: the DS's `latest_epoch` (absent when it named none).
 * `commitPending`: this device has a DeviceGroup commit in flight (its entry
 * is finished first; until then the DS may hold this device's own next
 * epoch).
 */
export function isBehind(
  localEpoch: bigint,
  drained: boolean,
  latestEpoch: bigint | undefined,
  commitPending: boolean,
): boolean {
  if (commitPending || !drained || latestEpoch === undefined) return false;
  return latestEpoch > localEpoch;
}

/** What a sibling decides about one knock. */
export type AdmissionVerdict =
  | "admit"
  /** Already handled by this device. */
  | "handled"
  /** From before this device started watching the journal: it cannot know
   *  whether another device admitted it; the knocker knocks again. */
  | "before-watching"
  /** Already waiting, and older than {@link STALE_KNOCK_AT_LISTEN_START_MS},
   *  when this device started listening: neither answered nor admitted. */
  | "stale"
  /** The key was added after the knock: someone already admitted it. */
  | "already-admitted"
  /** The knocking key is this device's own. */
  | "own-key"
  /** The key is not listed in the account's directory with a verified
   *  binding (unknown, deleted, tombstoned). */
  | "not-listed"
  /** The previous leaf is present and the policy wants a passkey
   *  attestation for its removal, which this device cannot obtain. */
  | "needs-attestation";

/** Everything {@link admissionVerdict} reads. Keys are lowercase hex. */
export interface AdmissionInput {
  readonly knockId: number;
  readonly knockAtMs: number;
  readonly keyHex: string;
  readonly ownKeyHex: string;
  /** The roster: leaf index and key of every device in the DeviceGroup. */
  readonly roster: readonly { readonly leaf: number; readonly keyHex: string }[];
  readonly handled: ReadonlySet<number>;
  /** When this device started watching the journal (unix ms). */
  readonly watchingSinceMs: number;
  /** When this device last saw `keyHex` added to the roster, if ever. */
  readonly lastAddedAtMs: number | undefined;
  /** The directory lists the key with a binding under the account key. */
  readonly listed: boolean;
  readonly policyRequiresAttestation: boolean;
  /** The knock carries a self-removal proof that covers replacing the
   *  previous leaf by its KeyPackage (checked with
   *  `deviceGroupSelfRemovalCovers`). */
  readonly selfRemovalProofCovers: boolean;
  /** A leaf of the DeviceGroup already carries the knock's KeyPackage leaf
   *  (`containsLeafOfKeyPackage`): another sibling answered it. A
   *  replacement keeps the key (often the leaf index too), so the roster of
   *  keys cannot tell. */
  readonly keyPackageAlreadyIn: boolean;
  /** When this device's current knock long poll started (unix ms); absent
   *  for a caller that does not listen. */
  readonly listeningSinceMs?: number;
  readonly nowMs: number;
}

/** The part of {@link AdmissionInput} that this device decides alone, with
 *  no directory lookup. */
export type KnockScreenInput = Pick<
  AdmissionInput,
  | "knockId"
  | "knockAtMs"
  | "keyHex"
  | "ownKeyHex"
  | "handled"
  | "watchingSinceMs"
  | "lastAddedAtMs"
  | "keyPackageAlreadyIn"
  | "listeningSinceMs"
>;

/**
 * The checks this device makes on a knock that names its account before it
 * answers it: the verdict that sets the knock aside, or `null` when the knock
 * is open and this device posts a presence note and goes on to
 * {@link admissionVerdict}.
 */
export function knockScreen(input: KnockScreenInput): AdmissionVerdict | null {
  if (input.handled.has(input.knockId)) return "handled";
  if (input.keyHex === input.ownKeyHex) return "own-key";
  if (input.knockAtMs < input.watchingSinceMs) return "before-watching";
  if (
    input.listeningSinceMs !== undefined &&
    input.knockAtMs < input.listeningSinceMs - STALE_KNOCK_AT_LISTEN_START_MS
  ) {
    return "stale";
  }
  if (input.keyPackageAlreadyIn) return "already-admitted";
  if (input.lastAddedAtMs !== undefined && input.lastAddedAtMs >= input.knockAtMs) {
    return "already-admitted";
  }
  return null;
}

/** The pure admission rule (see the module doc). Every running device that
 *  passes it admits at once; the DS commit route picks one. */
export function admissionVerdict(input: AdmissionInput): AdmissionVerdict {
  const screened = knockScreen(input);
  if (screened !== null) return screened;
  if (!input.listed) return "not-listed";
  const present = input.roster.some((m) => m.keyHex === input.keyHex);
  if (present && input.policyRequiresAttestation && !input.selfRemovalProofCovers) return "needs-attestation";
  return "admit";
}

// ----------------------------------------------------------------------------
// The sibling's durable record
// ----------------------------------------------------------------------------

/** What a sibling keeps across sessions (`pending`, `admissions:`): when it
 *  started watching, when it last saw each key added, and the knocks it
 *  handled. Cleared with the DeviceGroup state. */
export interface AdmissionRecord {
  readonly v: 1;
  readonly watchingSinceMs: number;
  readonly lastAddedAtMs: Readonly<Record<string, number>>;
  readonly handled: readonly number[];
}

/** At most this many handled knock ids are kept (knocks live seven days). */
export const MAX_HANDLED_KNOCKS = 256;

export function newAdmissionRecord(nowMs: number): AdmissionRecord {
  return { v: 1, watchingSinceMs: nowMs, lastAddedAtMs: {}, handled: [] };
}

/** The record with `keyHex` seen added at `atMs` (the later time wins). */
export function noteAdded(record: AdmissionRecord, keyHex: string, atMs: number): AdmissionRecord {
  const prior = record.lastAddedAtMs[keyHex];
  if (prior !== undefined && prior >= atMs) return record;
  return { ...record, lastAddedAtMs: { ...record.lastAddedAtMs, [keyHex]: atMs } };
}

/** The record with `knockId` handled (bounded, newest kept). */
export function noteHandled(record: AdmissionRecord, knockId: number): AdmissionRecord {
  if (record.handled.includes(knockId)) return record;
  const handled = [...record.handled, knockId].sort((a, b) => a - b).slice(-MAX_HANDLED_KNOCKS);
  return { ...record, handled };
}

export function encodeAdmissionRecord(record: AdmissionRecord): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(record));
}

/** `null` for bytes this build did not write. */
export function decodeAdmissionRecord(bytes: Uint8Array): AdmissionRecord | null {
  try {
    const v = JSON.parse(new TextDecoder().decode(bytes)) as Partial<AdmissionRecord>;
    if (v.v !== 1 || typeof v.watchingSinceMs !== "number") return null;
    if (!Array.isArray(v.handled) || !v.handled.every((n) => Number.isInteger(n))) return null;
    if (v.lastAddedAtMs === null || typeof v.lastAddedAtMs !== "object") return null;
    for (const at of Object.values(v.lastAddedAtMs)) if (typeof at !== "number") return null;
    return v as AdmissionRecord;
  } catch {
    return null;
  }
}

// ----------------------------------------------------------------------------
// The rejoining device's durable state
// ----------------------------------------------------------------------------

/** A rejoin in progress (`pending`, `rejoin:`). */
export interface RejoinState {
  readonly v: 1;
  /** The rendezvous the Welcome will arrive at, standard base64. */
  readonly rendezvousB64: string;
  /** The joiner's DeviceIdentity provider snapshot (it holds the
   *  KeyPackage's private keys), standard base64. */
  readonly joinerSnapshotB64: string;
  readonly knockedAtMs: number;
}

export function encodeRejoinState(state: RejoinState): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(state));
}

/** `null` for bytes this build did not write. */
export function decodeRejoinState(bytes: Uint8Array): RejoinState | null {
  try {
    const v = JSON.parse(new TextDecoder().decode(bytes)) as Partial<RejoinState>;
    if (v.v !== 1 || typeof v.rendezvousB64 !== "string" || typeof v.joinerSnapshotB64 !== "string") {
      return null;
    }
    if (typeof v.knockedAtMs !== "number") return null;
    return v as RejoinState;
  } catch {
    return null;
  }
}

/** Whether a rejoin knocked long enough ago to knock again. */
export function shouldKnockAgain(state: RejoinState | null, nowMs: number): boolean {
  return state === null || nowMs - state.knockedAtMs >= REKNOCK_AFTER_MS || nowMs < state.knockedAtMs;
}

// ----------------------------------------------------------------------------
// The rejoining device's run, over ports
// ----------------------------------------------------------------------------

/** One answer on a rejoin rendezvous. */
export type RejoinAnswer =
  /** A device of the account is running and saw the knock. */
  | { readonly kind: "present" }
  | { readonly kind: "welcome"; readonly bytes: Uint8Array };

/** The long poll on a rejoin rendezvous, open from creation. */
export interface AnswerWatch {
  /** The next answer not yet returned, or `null` when `deadlineMs` passes
   *  with none while the rendezvous was read. Rejects when the wait ends
   *  with no answer and a read failed during it: silence is only silence
   *  when this device was listening. */
  next(deadlineMs: number): Promise<RejoinAnswer | null>;
  close(): void;
}

/** A knock built and ready to post. */
export interface PreparedKnock {
  readonly state: RejoinState;
  readonly payload: Uint8Array;
}

/** What one rejoin attempt needs. */
export interface RejoinPorts {
  /** Delete the local DeviceGroup state (snapshot and pending commit). */
  dropLocalState(): Promise<void>;
  /** Make sure this device's directory KeyPackage is published. */
  publishDirectoryKeyPackage(): Promise<void>;
  loadState(): Promise<RejoinState | null>;
  saveState(state: RejoinState): Promise<void>;
  /** Build a fresh joiner (DeviceGroup KeyPackage from the device identity)
   *  and the knock payload. `null` when this device knows no username of
   *  the account, so no sibling could match a knock (the pass then ends
   *  `"no-handle"`). */
  prepareKnock(nowMs: number): Promise<PreparedKnock | null>;
  /** Open the long poll on `state`'s rendezvous. */
  watch(state: RejoinState): AnswerWatch;
  /** Post the knock. */
  postKnock(knock: PreparedKnock): Promise<void>;
  /** Join from the Welcome with the joiner kept in `state` and persist the
   *  joined DeviceGroup (fail-closed). */
  join(state: RejoinState, welcome: Uint8Array): Promise<void>;
  /** Found the DeviceGroup with this device as its only leaf (a root reset
   *  of its journal) and persist it (fail-closed). */
  found(): Promise<void>;
  clearState(): Promise<void>;
  /** The knock's answer came `ms` after the knock was sent (`null`: none). */
  noteAnswer(ms: number | null, kind: RejoinAnswer["kind"] | null): void;
  now(): number;
}

/** What one pass ended in. */
export type RejoinOutcome =
  /** A member again (its DeviceGroup is on disk). */
  | "joined"
  /** Nobody answered: this device founded the DeviceGroup (on disk). */
  | "founded"
  /** Answered, no Welcome before the re-knock time: call again. */
  | "waiting"
  /** No login handle is known, so no knock was made and nothing was
   *  founded: call again after {@link NO_HANDLE_RETRY_MS}. */
  | "no-handle";

/**
 * One pass of the device side (see the module doc): drop the state (first
 * pass only), resume a knock already out or knock, and then join, found or
 * wait.
 */
export async function rejoinOnce(ports: RejoinPorts, firstPass: boolean): Promise<RejoinOutcome> {
  if (firstPass) {
    await ports.dropLocalState();
    await ports.publishDirectoryKeyPackage();
  }
  const kept = await ports.loadState();
  if (kept !== null && !shouldKnockAgain(kept, ports.now())) {
    // A reload while a knock was out: an answer already on the rendezvous is
    // read at once; none means the knock was never answered, so knock again.
    const watch = ports.watch(kept);
    try {
      const answer = await watch.next(ports.now() + ANSWER_WINDOW_MIN_MS);
      if (answer?.kind === "welcome") return await joinWith(ports, kept, answer.bytes);
      if (answer?.kind === "present") return await awaitWelcome(ports, kept, watch);
    } finally {
      watch.close();
    }
  }
  const knock = await ports.prepareKnock(ports.now());
  // Never found here: another device of the account is listed.
  if (knock === null) return "no-handle";
  await ports.saveState(knock.state);
  const watch = ports.watch(knock.state);
  try {
    const sentAt = ports.now();
    await ports.postKnock(knock);
    const rtt = ports.now() - sentAt;
    const answer = await watch.next(ports.now() + answerWindowMs(rtt));
    ports.noteAnswer(answer === null ? null : ports.now() - sentAt, answer?.kind ?? null);
    if (answer === null) return await foundAlone(ports);
    if (answer.kind === "welcome") return await joinWith(ports, knock.state, answer.bytes);
    return await awaitWelcome(ports, knock.state, watch);
  } finally {
    watch.close();
  }
}

async function foundAlone(ports: RejoinPorts): Promise<RejoinOutcome> {
  await ports.found();
  await ports.clearState();
  return "founded";
}

/** A device answered: wait for its Welcome until the re-knock time. */
async function awaitWelcome(ports: RejoinPorts, state: RejoinState, watch: AnswerWatch): Promise<RejoinOutcome> {
  const deadline = state.knockedAtMs + REKNOCK_AFTER_MS;
  for (;;) {
    const answer = await watch.next(deadline);
    if (answer === null) return "waiting";
    if (answer.kind === "welcome") return joinWith(ports, state, answer.bytes);
    // Another device saw the knock too; keep waiting.
  }
}

async function joinWith(ports: RejoinPorts, state: RejoinState, welcome: Uint8Array): Promise<RejoinOutcome> {
  try {
    await ports.join(state, welcome);
  } catch (err) {
    // A Welcome this device cannot join from (or a write that failed): the
    // next pass knocks afresh instead of reading the same Welcome again.
    await ports.clearState().catch(() => undefined);
    throw err;
  }
  await ports.clearState();
  return "joined";
}

// ----------------------------------------------------------------------------
// What a failed attempt logs
// ----------------------------------------------------------------------------

/** A short code for a rejoin failure, safe to log: no identifiers (long hex
 *  or base64 runs are cut out, and the message is bounded). */
export function rejoinFailureCode(err: unknown): string {
  if (err === null || typeof err !== "object") return typeof err;
  const e = err as { name?: unknown; message?: unknown; status?: unknown; clockSkewed?: unknown };
  const name = typeof e.name === "string" ? e.name : "unknown";
  if (typeof e.status === "number") {
    return `${name} ${e.status}${e.clockSkewed === true ? " clock-skewed" : ""}`;
  }
  if (name === "DsNetworkError") return name;
  const message = typeof e.message === "string" ? redactIdentifiers(e.message).slice(0, 120) : "";
  return message.length === 0 ? name : `${name}: ${message}`;
}

/** `text` with every run that could be an identifier (16 or more hex digits,
 *  or 20 or more base64 characters) replaced by `<id>`. */
export function redactIdentifiers(text: string): string {
  return text.replace(/[0-9a-fA-F]{16,}/g, "<id>").replace(/[A-Za-z0-9+/_=-]{20,}/g, "<id>");
}
