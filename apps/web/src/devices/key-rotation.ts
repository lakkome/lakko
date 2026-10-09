// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "It was lost or stolen": remove a device AND change the account keys,
 * keeping the other devices (ADR-0066 part 3, Stage A; "T5 as built";
 * `docs/ux/identity-recovery.md` sections 18.1 and 18.3).
 *
 * ONE owed operation, resumable from its record, of these steps in order:
 *
 *   1. `remove`            the removal operation of `removal.ts` (revoke at
 *                          auth, the directory delete, the DeviceGroup Remove
 *                          commit). Done only once the commit is in the
 *                          journal: the device is out and the epoch advanced.
 *   2. `prepare`           a new phrase, the supersession proof signed by the
 *                          old key, and the `key-rotation` envelope posted
 *                          into the DeviceGroup, sealed at the POST-removal
 *                          epoch ({@link sealKeyRotationAfterRemoval}).
 *   3. `commit`            `POST /account/keys/rotate/{start,finish}`; the
 *                          account is at the new generation. The new seed,
 *                          phrase and pair are stored at once; ALL local data
 *                          is kept; the old key stays as the predecessor.
 *   4. `directory`         the identity bundle with BOTH the proof and the
 *                          attestation.
 *   5. `key-package`       this device's KeyPackage again (every row of the
 *                          account was deleted; the device id stays).
 *   6. `device-group-root` the attested root transfer of the DeviceGroup
 *                          journal, old root -> new root.
 *   7. `journals`          the plain root transfer of every other journal the
 *                          account owns, and the registrars re-appointed.
 *   8. `channels`          the owner key of every channel the account owns.
 *   9. `reach`             the new mailbox and the reach keys.
 *  10. `backup`            the backup re-sealed under the new phrase.
 *
 * A failure before `commit` changes nothing on the server: the device WAS
 * removed and the keys are unchanged. A failure after it resumes from the
 * record with the same attestation; the new words are withheld meanwhile.
 *
 * Everything that decides is a pure function over the record; the network,
 * the wasm core and the stores are ports. The record holds public material
 * only: the new entropy and the proof are sealed under the device key
 * (`key-rotation-store.ts`).
 */
import type { MarkerStorage } from "../auth/phrase-gate";
import type { RevokeFailure } from "./removal";

/** The steps, in the order they run. */
export const ROTATION_STEPS = [
  "remove",
  "prepare",
  "commit",
  "directory",
  "key-package",
  "device-group-root",
  "journals",
  "channels",
  "reach",
  "backup",
] as const;
export type RotationStep = (typeof ROTATION_STEPS)[number];

/** The supersession proof's validity (ADR-0066 section 3.2 step 3). */
export const SUPERSESSION_WINDOW_SECS = 14 * 24 * 60 * 60;

/** At most this many ids ride `revoke_device_ids_b64` (T5 item 11). */
export const MAX_REVOKE_DEVICE_IDS = 8;

/** The owed record. Public material only. */
export interface RotationOwed {
  readonly v: 1;
  /** The removed device's id (URL-safe base64, 43 chars). */
  readonly deviceIdB64: string;
  readonly startedAtMs: number;
  /** Steps that have their answer. */
  readonly done: readonly RotationStep[];
  /** The DeviceGroup's epoch read BEFORE the removal ran (decimal), when it
   *  could be read. The envelope must be sealed at a later one. */
  readonly preRemovalEpoch?: string;
  /** Set by `prepare`: the new account key (standard base64), when it was
   *  prepared, and the epoch the envelope was sealed at. */
  readonly newSignaturePubkeyB64?: string;
  readonly preparedAtMs?: number;
  readonly envelopeEpoch?: string;
  /** `rotate/finish` was sent and not answered: whether it landed is asked
   *  before anything is sent again ({@link CommitProbe}). */
  readonly commitUnconfirmed?: boolean;
  /** Set by `commit`. The attestation is absent when the commit's answer
   *  was lost and the commit was found to have landed: the later steps then
   *  run without it (proof-only directory upload, plain root transfer). */
  readonly keyGeneration?: string;
  readonly attestationB64Url?: string;
  readonly signedAtMs?: number;
  /** The directory refused the proof because the row's key had already
   *  been changed (T5 item 20): the upload was repeated WITHOUT the proof,
   *  as an attested reset. Peers then follow it as a reset. */
  readonly directoryWithoutProof?: boolean;
}

export function newRotation(deviceIdB64: string, nowMs: number): RotationOwed {
  return { v: 1, deviceIdB64, startedAtMs: nowMs, done: [] };
}

export function rotationStepDone(record: RotationOwed, step: RotationStep): boolean {
  return record.done.includes(step);
}

export function withRotationStepDone(record: RotationOwed, step: RotationStep): RotationOwed {
  if (record.done.includes(step)) return record;
  return { ...record, done: ROTATION_STEPS.filter((s) => s === step || record.done.includes(s)) };
}

export function remainingRotationSteps(record: RotationOwed): RotationStep[] {
  return ROTATION_STEPS.filter((s) => !record.done.includes(s));
}

/** Strictly in order: a later step never runs before an earlier one. */
export function nextRotationStep(record: RotationOwed): RotationStep | null {
  return remainingRotationSteps(record)[0] ?? null;
}

/** Whether the account's keys were changed (the point of no return). */
export function rotationCommitted(record: RotationOwed): boolean {
  return rotationStepDone(record, "commit");
}

/**
 * Whether the record continues WITHOUT the person: only once the commit
 * landed. Before it, every step needs the fresh proof (the revoke, the
 * commit) or the person's choice to try again.
 */
export function rotationResumesByItself(record: RotationOwed): boolean {
  return rotationCommitted(record) && nextRotationStep(record) !== null;
}

/** Whether the new words are withheld: the keys were changed and a
 *  publishing step is still owed (UX 15.4's "publish owed"). */
export function rotationWithholdsPhrase(record: RotationOwed | null): boolean {
  return record !== null && rotationResumesByItself(record);
}

/** The three lines of the sheet's step list (UX 18.1). */
export type RotationStepLine = "remove" | "keys" | "devices";

export function rotationStepLine(step: RotationStep): RotationStepLine {
  if (step === "remove") return "remove";
  return step === "prepare" || step === "commit" ? "keys" : "devices";
}

// ---------------------------------------------------------------------------
// What each answer means
// ---------------------------------------------------------------------------

/** Why the keys were not changed although the device was removed. */
export type NotStartedReason =
  /** The removal is signed out at auth but its DeviceGroup commit has not
   *  landed yet: the envelope must not be sealed. */
  | "removal-finishing"
  /** The envelope could not be prepared or posted. */
  | "prepare"
  /** `403 fresh-auth-required`: the proof block. */
  | "proof-required"
  /** `403 account-key-required`. */
  | "account-key-required"
  /** `429 too-many-attempts`. */
  | "rate-limited"
  /** No answer to the commit, and whether it landed could not be asked. */
  | "unconfirmed"
  /** `400`, anything unexpected. */
  | "other";

/** `rotate/finish`'s (and `start`'s) refusals, in the server's order
 *  (T5 item 7). `status` is `null` when no answer arrived. */
export function rotateCommitVerdict(
  status: number | null,
  errorCode: string | null,
): { readonly verdict: "done" | "later" | "refused"; readonly reason: NotStartedReason | null } {
  if (status !== null && status >= 200 && status < 300) return { verdict: "done", reason: null };
  if (status === null || status >= 500) return { verdict: "later", reason: "unconfirmed" };
  if (status === 403 && errorCode === "fresh-auth-required") {
    return { verdict: "refused", reason: "proof-required" };
  }
  if (status === 403 && errorCode === "account-key-required") {
    return { verdict: "refused", reason: "account-key-required" };
  }
  if (status === 429) return { verdict: "refused", reason: "rate-limited" };
  return { verdict: "refused", reason: "other" };
}

/** What a later step's answer means. */
export type PublishVerdict = "done" | "later";

/** How the directory upload is sent. */
export type DirectoryUploadMode =
  /** The proof AND the attestation: the proven-rotation arm. */
  | "proof-and-attestation"
  /** The attestation alone: the fallback of T5 item 20. */
  | "attestation-only"
  /** The proof alone: the commit's answer was lost, no attestation held. */
  | "proof-only";

export function directoryUploadMode(record: RotationOwed): DirectoryUploadMode {
  if (record.attestationB64Url === undefined) return "proof-only";
  return record.directoryWithoutProof === true ? "attestation-only" : "proof-and-attestation";
}

/** The directory's answer to the rotation upload (T5 items 18-20). */
export type DirectoryRotationOutcome =
  | "done"
  | "later"
  /** `400 supersession-proof-invalid` on the proven upload: the row's key
   *  was changed by someone else first (the removed device, before its
   *  session ended). Send again WITHOUT the proof. */
  | "retry-without-proof"
  /** The session is of a superseded generation: another change overtook
   *  this one. */
  | "superseded";

export function directoryRotationOutcome(input: {
  readonly mode: DirectoryUploadMode;
  readonly status: number | null;
  readonly errorCode: string | null;
}): DirectoryRotationOutcome {
  const { status, errorCode } = input;
  if (status === null) return "later";
  if (status >= 200 && status < 300) return "done";
  // A retry after a lost answer: the generation already landed.
  if (status === 409 && errorCode === "key-generation-not-newer") return "done";
  if (status === 401 && errorCode === "key-generation-superseded") return "superseded";
  if (
    status === 400 &&
    errorCode === "supersession-proof-invalid" &&
    input.mode === "proof-and-attestation"
  ) {
    return "retry-without-proof";
  }
  return "later";
}

/** `PUT /v1/channels/{id}/owner` (T5 item 26). `204` also answers a handover
 *  naming the key that already owns the channel, so a retry reads as done;
 *  `409 owner-changed` and `401 owner-proof-required` mean the OLD key is
 *  no longer the owner: confirmed done only when the caller's probe by the
 *  NEW key says so. */
export function channelOwnerOutcome(input: {
  readonly status: number | null;
  /** Whether a handover signed by the NEW key naming itself answered 204;
   *  `null` when it was not asked. */
  readonly newKeyOwns: boolean | null;
}): PublishVerdict {
  if (input.status === 204 || input.status === 200) return "done";
  if (input.status === 404) return "done";
  if (input.status === 409 || input.status === 401) return input.newKeyOwns === true ? "done" : "later";
  return "later";
}

// ---------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------

/** What the removal came to, for the rotation. */
export type RemoveStepResult =
  /** The device is out of the DeviceGroup and the commit is in the journal
   *  (or no leaf holds its key any more). */
  | { readonly kind: "removed"; readonly preRemovalEpoch: bigint | null }
  /** The revoke was refused or unconfirmed: nothing was removed. */
  | { readonly kind: "not-removed"; readonly failure: RevokeFailure }
  /** Signed out at auth; the DeviceGroup step has no answer yet. */
  | { readonly kind: "finishing" };

/** Whether an unanswered commit landed. */
export type CommitProbe =
  /** The account is at a new generation under the prepared key. */
  | { readonly kind: "landed"; readonly keyGeneration: bigint }
  | { readonly kind: "not-landed" }
  | { readonly kind: "unknown" };

export interface RotationPorts {
  /** Step 1. `grant` is needed until the revoke has its answer. */
  readonly remove: (deviceIdB64: string, grant: string | null) => Promise<RemoveStepResult>;
  /** Steps 3-4 of ADR 3.2: generate, sign the proof, post the envelope
   *  through {@link sealKeyRotationAfterRemoval}. Throws when it could not. */
  readonly prepare: (record: RotationOwed) => Promise<{
    readonly newSignaturePubkeyB64: string;
    readonly preparedAtMs: number;
    readonly envelopeEpoch: bigint;
  }>;
  /** Step 5-7: rotate start/finish, then adopt locally. */
  readonly commit: (
    record: RotationOwed,
    grant: string,
  ) => Promise<
    | {
        readonly status: 200;
        readonly keyGeneration: bigint;
        readonly attestationB64Url: string;
        readonly signedAtMs: number;
      }
    | { readonly status: number | null; readonly errorCode: string | null }
  >;
  /** Ask whether an unanswered commit landed, and adopt locally if so. */
  readonly probeCommit: (record: RotationOwed) => Promise<CommitProbe>;
  /** Step 8. */
  readonly uploadIdentity: (
    record: RotationOwed,
    mode: DirectoryUploadMode,
  ) => Promise<{ readonly status: number | null; readonly errorCode: string | null }>;
  /** The remaining steps: each answers done or later and never throws for a
   *  refusal (a throw reads as later). */
  readonly publish: (
    step: "key-package" | "device-group-root" | "journals" | "channels" | "reach" | "backup",
    record: RotationOwed,
  ) => Promise<PublishVerdict>;
  /** The session is of a superseded generation. */
  readonly superseded: () => void;
  /** Persist the record after every step (or drop it: `null`). */
  readonly save: (record: RotationOwed | null) => void;
}

/** How one run ended. */
export type RotationOutcome =
  /** Every step has its answer; the record is gone. The words are shown. */
  | { readonly kind: "complete" }
  /** Nothing was removed (UX 18.1: the removal's own states). */
  | { readonly kind: "not-removed"; readonly failure: RevokeFailure }
  /** The device was removed; the keys were NOT changed. */
  | { readonly kind: "not-started"; readonly reason: NotStartedReason }
  /** The keys were changed; `owed` is still being published. */
  | { readonly kind: "publishing"; readonly owed: readonly RotationStep[] };

/**
 * Run the operation from `record` as far as it goes.
 *
 * `onStep` is told each step as it becomes active (the sheet's step list).
 */
export async function runRotation(
  record: RotationOwed,
  ports: RotationPorts,
  options: { readonly grant: string | null; readonly onStep?: (step: RotationStep) => void },
): Promise<RotationOutcome> {
  let current = record;
  const save = (next: RotationOwed | null): void => {
    ports.save(next);
  };
  // Written before anything is sent.
  save(current);
  for (;;) {
    const step = nextRotationStep(current);
    if (step === null) {
      save(null);
      return { kind: "complete" };
    }
    options.onStep?.(step);
    if (step === "remove") {
      let result: RemoveStepResult;
      try {
        result = await ports.remove(current.deviceIdB64, options.grant);
      } catch {
        result = { kind: "finishing" };
      }
      if (result.kind === "not-removed") {
        // Nothing was removed and nothing of the rotation started: no record
        // (the removal keeps its own when its revoke had no answer).
        save(null);
        return { kind: "not-removed", failure: result.failure };
      }
      if (result.kind === "finishing") return { kind: "not-started", reason: "removal-finishing" };
      const epoch = result.preRemovalEpoch;
      current =
        epoch === null || current.preRemovalEpoch !== undefined
          ? current
          : { ...current, preRemovalEpoch: epoch.toString() };
    } else if (step === "prepare") {
      try {
        const prepared = await ports.prepare(current);
        current = {
          ...current,
          newSignaturePubkeyB64: prepared.newSignaturePubkeyB64,
          preparedAtMs: prepared.preparedAtMs,
          envelopeEpoch: prepared.envelopeEpoch.toString(),
        };
      } catch {
        return { kind: "not-started", reason: "prepare" };
      }
    } else if (step === "commit") {
      if (current.commitUnconfirmed === true) {
        let probe: CommitProbe;
        try {
          probe = await ports.probeCommit(current);
        } catch {
          probe = { kind: "unknown" };
        }
        if (probe.kind === "unknown") return { kind: "not-started", reason: "unconfirmed" };
        if (probe.kind === "landed") {
          const { commitUnconfirmed: _c, ...rest } = current;
          current = withRotationStepDone(
            { ...rest, keyGeneration: probe.keyGeneration.toString() },
            "commit",
          );
          save(current);
          continue;
        }
        const { commitUnconfirmed: _c, ...rest } = current;
        current = rest;
        save(current);
      }
      const grant = options.grant;
      if (grant === null) return { kind: "not-started", reason: "proof-required" };
      // Marked BEFORE it is sent: a crash mid-request must ask, not resend.
      current = { ...current, commitUnconfirmed: true };
      save(current);
      let answer: Awaited<ReturnType<RotationPorts["commit"]>>;
      try {
        answer = await ports.commit(current, grant);
      } catch {
        answer = { status: null, errorCode: null };
      }
      if (answer.status === 200 && "attestationB64Url" in answer) {
        const { commitUnconfirmed: _c, ...rest } = current;
        current = {
          ...rest,
          keyGeneration: answer.keyGeneration.toString(),
          attestationB64Url: answer.attestationB64Url,
          signedAtMs: answer.signedAtMs,
        };
      } else {
        const refusal = answer as { readonly status: number | null; readonly errorCode: string | null };
        const read = rotateCommitVerdict(refusal.status, refusal.errorCode);
        if (read.verdict === "refused") {
          // The server chose an answer: nothing was changed.
          const { commitUnconfirmed: _c, ...rest } = current;
          current = rest;
          save(current);
        }
        return { kind: "not-started", reason: read.reason ?? "other" };
      }
    } else if (step === "directory") {
      let mode = directoryUploadMode(current);
      let outcome: DirectoryRotationOutcome;
      for (;;) {
        let answer: { readonly status: number | null; readonly errorCode: string | null };
        try {
          answer = await ports.uploadIdentity(current, mode);
        } catch {
          answer = { status: null, errorCode: null };
        }
        outcome = directoryRotationOutcome({ mode, ...answer });
        if (outcome !== "retry-without-proof") break;
        current = { ...current, directoryWithoutProof: true };
        save(current);
        mode = directoryUploadMode(current);
      }
      if (outcome === "superseded") {
        ports.superseded();
        return { kind: "publishing", owed: remainingRotationSteps(current) };
      }
      if (outcome !== "done") return { kind: "publishing", owed: remainingRotationSteps(current) };
    } else {
      let verdict: PublishVerdict;
      try {
        verdict = await ports.publish(step, current);
      } catch {
        verdict = "later";
      }
      if (verdict !== "done") return { kind: "publishing", owed: remainingRotationSteps(current) };
    }
    current = withRotationStepDone(current, step);
    save(nextRotationStep(current) === null ? null : current);
  }
}

// ---------------------------------------------------------------------------
// The ordering seam: the envelope is sealed AFTER the removal
// ---------------------------------------------------------------------------

/** The envelope was about to be sealed where the removed device could read
 *  it. Nothing was sealed or sent. */
export class KeyRotationOrderError extends Error {
  override name = "KeyRotationOrderError";
  constructor(readonly reason: "device-still-member" | "epoch-not-advanced") {
    super(`key-rotation envelope refused: ${reason}`);
  }
}

/** The DeviceGroup handle that seals the envelope. */
export interface KeyRotationSealPorts {
  /** The leaf signature keys of the DeviceGroup as the sealing handle holds
   *  it NOW. */
  readonly rosterKeys: () => readonly Uint8Array[];
  /** The epoch of the sealing handle NOW. */
  readonly epoch: () => bigint;
  /** Seal `envelope` as a DeviceGroup application message at the handle's
   *  epoch. Advances the sender ratchet. */
  readonly seal: (envelope: Uint8Array) => Uint8Array;
  /** Publish the sealed message to the DeviceGroup journal, and nowhere
   *  else. */
  readonly publish: (wire: Uint8Array) => Promise<void>;
}

function sameKey(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
}

/**
 * ADR-0066 section 3.2: "the stolen device is out first", with the weight of
 * C-61. The `key-rotation` envelope carries the account's NEW entropy, so it
 * must be sealed at a DeviceGroup epoch the removed device never held.
 * Nothing but the sender can check this (T5 item 5), so the two steps are one
 * named seam: the envelope is sealed only when
 *
 *   - no leaf of the sealing handle's roster holds the removed device's key,
 *     and
 *   - the handle's epoch is past the one read before the removal, when that
 *     was read.
 *
 * Returns the epoch the message was sealed at. Throws
 * {@link KeyRotationOrderError} with nothing sealed otherwise.
 */
export async function sealKeyRotationAfterRemoval(
  input: {
    readonly removedDeviceKey: Uint8Array;
    /** The DeviceGroup epoch before the removal; `null` when unknown (the
     *  device had already been removed when this operation started). */
    readonly preRemovalEpoch: bigint | null;
    readonly envelope: Uint8Array;
  },
  ports: KeyRotationSealPorts,
): Promise<{ readonly epoch: bigint }> {
  if (ports.rosterKeys().some((k) => sameKey(k, input.removedDeviceKey))) {
    throw new KeyRotationOrderError("device-still-member");
  }
  const epoch = ports.epoch();
  if (input.preRemovalEpoch !== null && epoch <= input.preRemovalEpoch) {
    throw new KeyRotationOrderError("epoch-not-advanced");
  }
  const wire = ports.seal(input.envelope);
  await ports.publish(wire);
  return { epoch };
}

// ---------------------------------------------------------------------------
// A kept device (ADR-0066 section 3.3)
// ---------------------------------------------------------------------------

/** One verified envelope, held until the server shows its key. */
export interface PendingRotation {
  /** The proof's new key, standard base64: the row's key. */
  readonly newSignaturePubkeyB64: string;
  /** The 192-byte envelope, standard base64 (sealed at rest by the store). */
  readonly envelopeB64: string;
  readonly preparedAtMs: number;
}

/** What to do with a dispatched `KeyRotation` envelope. */
export type RotationIntake = "store" | "drop";

/**
 * The intake rule. The envelope means something ONLY in the account's own
 * DeviceGroup; anywhere else it is dropped and never rendered. It is stored
 * only when the three checks of `verifyKeyRotationEnvelope` passed, and
 * never for the key this device already holds.
 */
export function rotationIntake(input: {
  readonly fromOwnDeviceGroup: boolean;
  /** `verifyKeyRotationEnvelope` returned (did not throw). */
  readonly verified: boolean;
  readonly newKeyIsHeldKey: boolean;
}): RotationIntake {
  if (!input.fromOwnDeviceGroup || !input.verified || input.newKeyIsHeldKey) return "drop";
  return "store";
}

/** The pending set with `next` added (one row per new key; a re-send
 *  replaces its row) and the expired rows dropped. */
export function withPendingRotation(
  pending: readonly PendingRotation[],
  next: PendingRotation | null,
  nowMs: number,
  expired: (preparedAtMs: number, nowMs: number) => boolean,
): PendingRotation[] {
  const kept = pending.filter(
    (p) =>
      !expired(p.preparedAtMs, nowMs) &&
      (next === null || p.newSignaturePubkeyB64 !== next.newSignaturePubkeyB64),
  );
  return next === null || expired(next.preparedAtMs, nowMs) ? kept : [...kept, next];
}

/** The pending envelope for `publishedKeyB64`, or `null`. Which envelope is
 *  adopted is decided by the key the server side shows, never by arrival
 *  order or the stamp (T5 item 2). */
export function pendingFor(
  pending: readonly PendingRotation[],
  publishedKeyB64: string | null,
): PendingRotation | null {
  if (publishedKeyB64 === null) return null;
  return pending.find((p) => p.newSignaturePubkeyB64 === publishedKeyB64) ?? null;
}

/** What a kept device does when it learns its generation is superseded. */
export type SupersededStep =
  /** Try the session upgrade with these keys, in order: the held key first
   *  (an adoption whose upgrade was interrupted), then each pending key. */
  | { readonly kind: "try-keys"; readonly keysB64: readonly string[] }
  /** Nothing to try (or everything tried was not accepted) and the journal
   *  was not read yet: read the DeviceGroup journal to its head, once. */
  | { readonly kind: "read-journal" }
  /** It is a reset (or indistinguishable from one): the staged wipe and
   *  "Was it you?". */
  | { readonly kind: "reset" };

/**
 * The order on a superseded device (ADR-0066 section 3.3). `tried` are the
 * keys whose statement the server did not accept in this pass.
 */
export function supersededStep(input: {
  readonly heldKeyB64: string;
  readonly pending: readonly PendingRotation[];
  readonly tried: readonly string[];
  readonly journal: "not-read" | "read" | "unreadable";
}): SupersededStep {
  const candidates = [input.heldKeyB64, ...input.pending.map((p) => p.newSignaturePubkeyB64)].filter(
    (k, i, all) => all.indexOf(k) === i && !input.tried.includes(k),
  );
  if (candidates.length > 0) return { kind: "try-keys", keysB64: candidates };
  if (input.journal === "not-read") return { kind: "read-journal" };
  return { kind: "reset" };
}

/** What the session upgrade's answer means for the key it was signed with. */
export type UpgradeVerdict =
  /** A pair at the current generation: this key is the account's. */
  | "accepted"
  /** `key-generation-superseded`: this key is not the account's. */
  | "not-accepted"
  /** `device-revoked`: this device was the one removed. The full wipe. */
  | "device-revoked"
  /** `refresh-invalid`: the credential is dead. Sign in again; nothing is
   *  wiped. */
  | "sign-in"
  /** `statement-stale`: sign again at the server's time. */
  | "stale"
  /** No answer, a limit, a server error: try again later. Never a reset. */
  | "later";

export function upgradeVerdict(status: number | null, errorCode: string | null): UpgradeVerdict {
  if (status !== null && status >= 200 && status < 300) return "accepted";
  if (status === 401) {
    if (errorCode === "key-generation-superseded") return "not-accepted";
    if (errorCode === "device-revoked") return "device-revoked";
    if (errorCode === "statement-stale") return "stale";
    if (errorCode === "refresh-invalid") return "sign-in";
    // A 401 this client cannot read never wipes and never adopts.
    return "later";
  }
  return "later";
}

// ---------------------------------------------------------------------------
// Storage of the owed record
// ---------------------------------------------------------------------------

function storageKey(accountIdB64: string): string {
  return `lakko:key-rotation-owed:${accountIdB64}`;
}

function isStep(value: unknown): value is RotationStep {
  return typeof value === "string" && (ROTATION_STEPS as readonly string[]).includes(value);
}

function optionalString(value: unknown): boolean {
  return value === undefined || typeof value === "string";
}

function optionalNumber(value: unknown): boolean {
  return value === undefined || (typeof value === "number" && Number.isFinite(value));
}

/** The record, or `null` (a damaged row cannot drive a retry). */
export function readRotation(storage: MarkerStorage | undefined, accountIdB64: string): RotationOwed | null {
  try {
    const raw = storage?.getItem(storageKey(accountIdB64)) ?? null;
    if (raw === null) return null;
    const r = JSON.parse(raw) as Partial<RotationOwed>;
    if (
      r.v !== 1 ||
      typeof r.deviceIdB64 !== "string" ||
      !/^[A-Za-z0-9_-]{43}$/.test(r.deviceIdB64) ||
      typeof r.startedAtMs !== "number" ||
      !Array.isArray(r.done) ||
      !r.done.every(isStep) ||
      !optionalString(r.preRemovalEpoch) ||
      !optionalString(r.newSignaturePubkeyB64) ||
      !optionalNumber(r.preparedAtMs) ||
      !optionalString(r.envelopeEpoch) ||
      !optionalString(r.keyGeneration) ||
      !optionalString(r.attestationB64Url) ||
      !optionalNumber(r.signedAtMs)
    ) {
      return null;
    }
    // A step can only be done when every earlier one is.
    const done = ROTATION_STEPS.filter((s) => (r.done as RotationStep[]).includes(s));
    for (let i = 0; i < done.length; i += 1) if (done[i] !== ROTATION_STEPS[i]) return null;
    return { ...(r as RotationOwed), done };
  } catch {
    return null;
  }
}

export function writeRotation(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  record: RotationOwed | null,
): void {
  try {
    if (record === null) storage?.removeItem(storageKey(accountIdB64));
    else storage?.setItem(storageKey(accountIdB64), JSON.stringify(record));
  } catch {
    // Storage blocked: the operation still runs to its end in this page.
  }
}

export function clearRotation(storage: MarkerStorage | undefined, accountIdB64: string): void {
  writeRotation(storage, accountIdB64, null);
}

// ---------------------------------------------------------------------------
// The kept device's notice (UX 18.3)
// ---------------------------------------------------------------------------

function noticeKey(accountIdB64: string): string {
  return `lakko:key-rotation-notice:${accountIdB64}`;
}

/** This device adopted keys changed on another device; the line is shown
 *  until dismissed. */
export function markRotatedNotice(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.setItem(noticeKey(accountIdB64), "1");
  } catch {
    /* blocked */
  }
}

export function rotatedNoticeOwed(storage: MarkerStorage | undefined, accountIdB64: string): boolean {
  try {
    return storage?.getItem(noticeKey(accountIdB64)) === "1";
  } catch {
    return false;
  }
}

export function clearRotatedNotice(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(noticeKey(accountIdB64));
  } catch {
    /* blocked */
  }
}
