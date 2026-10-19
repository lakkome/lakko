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
 * ## The device's side
 *
 * 1. Delete the local DeviceGroup state (snapshot, pending commit) and make
 *    sure its own directory KeyPackage is published (ADR-0050; it carries the
 *    signed device binding, published under the device-bound session).
 * 2. Build a DeviceGroup KeyPackage from the same device identity (same leaf
 *    key) and post it to the DeviceGroup's knock (ADR-0053, one `ds-knock`
 *    token) as the ADR-0056 pairing payload: login handle, KeyPackage and a
 *    fresh rendezvous id. The DeviceGroup id is derived from the account seed
 *    (ADR-0078 phase 6). The joiner's provider and the rendezvous are kept
 *    (`pending`, `rejoin:`) so a reload keeps waiting for the same Welcome.
 * 3. Poll the rendezvous for the Welcome; join from it. Knock again with a
 *    fresh KeyPackage after {@link REKNOCK_AFTER_MS} without one.
 *
 * ## A sibling's side
 *
 * A device holding the DeviceGroup reads the knocks once its journal is read
 * to the head and admits a knock only when {@link admissionVerdict} says so:
 * the payload names this account's login handle; the KeyPackage's leaf key is
 * listed in the account's directory with a device binding that verifies under
 * the account key (a deleted device is tombstoned there, ADR-0074, and not
 * listed); the knock is newer than every addition of that key this device has
 * seen and newer than when it started watching; this device is the
 * designated admitter (the lowest leaf other than the knocking key) or the
 * knock has waited {@link FALLBACK_ADMIT_AFTER_MS}. To admit it replaces the
 * key's previous leaf, if still in the roster, by the KeyPackage in ONE
 * commit (`replaceDeviceAt`), or adds the KeyPackage when no previous leaf is
 * there - one ordinary DeviceGroup commit through the outbox
 * (`device-group-commit.ts`), so the DS arbitrates two siblings acting at
 * once - and the Welcome goes to the knock's rendezvous.
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

/** How long a knock waits for the designated admitter before any sibling
 *  admits it. Chosen, not measured. */
export const FALLBACK_ADMIT_AFTER_MS = 10 * 60_000;

/** How often a sibling reads the knocks while its journal is at the head.
 *  Chosen, not measured. */
export const KNOCK_CHECK_INTERVAL_MS = 30_000;

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
  /** The key was added after the knock: someone already admitted it. */
  | "already-admitted"
  /** The knocking key is this device's own. */
  | "own-key"
  /** The key is not listed in the account's directory with a verified
   *  binding (unknown, deleted, tombstoned). */
  | "not-listed"
  /** Waiting for the designated admitter. */
  | "not-mine-yet"
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
  readonly nowMs: number;
}

/** The pure admission rule (see the module doc). */
export function admissionVerdict(input: AdmissionInput): AdmissionVerdict {
  if (input.handled.has(input.knockId)) return "handled";
  if (input.keyHex === input.ownKeyHex) return "own-key";
  if (input.knockAtMs < input.watchingSinceMs) return "before-watching";
  if (input.keyPackageAlreadyIn) return "already-admitted";
  if (input.lastAddedAtMs !== undefined && input.lastAddedAtMs >= input.knockAtMs) {
    return "already-admitted";
  }
  if (!input.listed) return "not-listed";
  const others = input.roster.filter((m) => m.keyHex !== input.keyHex);
  const lowest = others.reduce<{ leaf: number; keyHex: string } | null>(
    (best, m) => (best === null || m.leaf < best.leaf ? m : best),
    null,
  );
  const designated = lowest !== null && lowest.keyHex === input.ownKeyHex;
  if (!designated && input.nowMs - input.knockAtMs < FALLBACK_ADMIT_AFTER_MS) return "not-mine-yet";
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

/** What one rejoin attempt needs. */
export interface RejoinPorts {
  /** Delete the local DeviceGroup state (snapshot and pending commit). */
  dropLocalState(): Promise<void>;
  /** Make sure this device's directory KeyPackage is published. */
  publishDirectoryKeyPackage(): Promise<void>;
  loadState(): Promise<RejoinState | null>;
  saveState(state: RejoinState): Promise<void>;
  /** Build a fresh joiner (DeviceGroup KeyPackage from the device identity)
   *  and knock with it; resolves to the state to keep. */
  knock(nowMs: number): Promise<RejoinState>;
  /** Wait for the Welcome at `state`'s rendezvous until `deadlineMs`;
   *  `null` on a timeout. */
  awaitWelcome(state: RejoinState, deadlineMs: number): Promise<Uint8Array | null>;
  /** Join from the Welcome with the joiner kept in `state` and persist the
   *  joined DeviceGroup (fail-closed). */
  join(state: RejoinState, welcome: Uint8Array): Promise<void>;
  clearState(): Promise<void>;
  now(): number;
}

/**
 * One pass of the device side: drop the state (first pass only), make sure a
 * knock is out, wait for its Welcome until the re-knock time. `"joined"` when
 * the device is a member again (its DeviceGroup is on disk), `"waiting"`
 * when the caller should call again.
 */
export async function rejoinOnce(ports: RejoinPorts, firstPass: boolean): Promise<"joined" | "waiting"> {
  if (firstPass) {
    await ports.dropLocalState();
    await ports.publishDirectoryKeyPackage();
  }
  let state = await ports.loadState();
  if (shouldKnockAgain(state, ports.now())) {
    state = await ports.knock(ports.now());
    await ports.saveState(state);
  }
  if (state === null) return "waiting";
  const welcome = await ports.awaitWelcome(state, state.knockedAtMs + REKNOCK_AFTER_MS);
  if (welcome === null) return "waiting";
  await ports.join(state, welcome);
  await ports.clearState();
  return "joined";
}
