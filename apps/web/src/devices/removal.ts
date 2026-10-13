// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Removing a device is ONE operation of four steps with an owed record
 * (ADR-0074 section 2.3; `docs/ux/identity-recovery.md` section 18.1).
 *
 *   1. `revoke`       `POST /account/devices/revoke` at auth: the device is
 *                     signed out NOW and wipes itself at its next contact.
 *                     Needs a fresh proof and a statement signed by the
 *                     account key. If it is refused, nothing else happens.
 *   2. `directory`    `DELETE /discovery/keypackage/device/{id}` with the
 *                     statement: the directory stops serving the device and
 *                     tombstones its id, so it cannot publish itself back.
 *   3. `device-group` the DeviceGroup Remove commit (attested when the
 *                     account's policy asks for it).
 *   4. `chats`        the device's leaf is proposed out of every chat.
 *
 * The record is written BEFORE step 1 is sent, and every later step is
 * retried from it at each landing until it has an answer, so a crash or an
 * offline step never leaves a device signed out at auth but still in the
 * groups with nobody coming back for it. Steps 2 to 4 need no proof and no
 * gesture (except an attested DeviceGroup removal, see below), so they finish
 * by themselves; step 1 needs the person, so a record whose revoke has no
 * answer waits for the same button again and runs nothing else meanwhile.
 *
 * The statement is valid for 120 s and is never stored: every step that
 * needs one signs it right before sending.
 *
 * Everything that decides is a pure function over the record; the network
 * and the MLS calls are ports.
 */
import type { MarkerStorage } from "../auth/phrase-gate";

/** The steps, in the order they run. */
export const REMOVAL_STEPS = ["revoke", "directory", "device-group", "chats"] as const;
export type RemovalStep = (typeof REMOVAL_STEPS)[number];

/** One owed removal. */
export interface RemovalOwed {
  readonly v: 1;
  /** The removed device's id: its leaf signature key, URL-safe base64
   *  without padding (the directory row id, the token's device segment). */
  readonly deviceIdB64: string;
  /** When the person asked, unix ms. */
  readonly startedAtMs: number;
  readonly done: {
    readonly revoke: boolean;
    readonly directory: boolean;
    readonly deviceGroup: boolean;
    readonly chats: boolean;
  };
  /** ADR-0015: the passkey attestation for the DeviceGroup removal, URL-safe
   *  base64, and when it stops being accepted (unix ms). A public,
   *  auth-signed artefact bound to this one removal; kept so a retry within
   *  its lifetime needs no second gesture. Absent on an account without a
   *  passkey. */
  readonly attestationB64?: string;
  readonly attestationExpiresAtMs?: number;
  /** The DeviceGroup step asked for a passkey attestation and none was
   *  usable: the removal waits on the person (the row's `Finish removing`).
   *  Stored, so the row still offers the button after a relaunch; cleared
   *  when a new attestation is attached and when the step lands. */
  readonly waitsOnPerson?: boolean;
}

/** A new record: nothing done. */
export function newRemoval(deviceIdB64: string, nowMs: number): RemovalOwed {
  return {
    v: 1,
    deviceIdB64,
    startedAtMs: nowMs,
    done: { revoke: false, directory: false, deviceGroup: false, chats: false },
  };
}

function doneOf(record: RemovalOwed, step: RemovalStep): boolean {
  switch (step) {
    case "revoke":
      return record.done.revoke;
    case "directory":
      return record.done.directory;
    case "device-group":
      return record.done.deviceGroup;
    case "chats":
      return record.done.chats;
  }
}

/** The record with `step` marked done. */
export function withStepDone(record: RemovalOwed, step: RemovalStep): RemovalOwed {
  const done = { ...record.done };
  if (step === "revoke") done.revoke = true;
  else if (step === "directory") done.directory = true;
  else if (step === "device-group") done.deviceGroup = true;
  else done.chats = true;
  if (step !== "device-group") return { ...record, done };
  // The step that could wait on the person has its answer.
  const { waitsOnPerson: _waited, ...rest } = record;
  return { ...rest, done };
}

/** The record once the DeviceGroup step found it needs the person. */
export function withWaitingOnPerson(record: RemovalOwed): RemovalOwed {
  return { ...record, waitsOnPerson: true };
}

/** The record with a freshly made attestation attached: it no longer waits
 *  on the person. */
export function withAttestation(
  record: RemovalOwed,
  attestationB64: string,
  expiresAtMs: number,
): RemovalOwed {
  const { waitsOnPerson: _waited, ...rest } = record;
  return { ...rest, attestationB64, attestationExpiresAtMs: expiresAtMs };
}

/** The steps still owed, in order. */
export function remainingSteps(record: RemovalOwed): RemovalStep[] {
  return REMOVAL_STEPS.filter((s) => !doneOf(record, s));
}

/** The next step to run, or `null` when the removal is complete. Strictly
 *  in order: a later step never runs before an earlier one has its answer. */
export function nextStep(record: RemovalOwed): RemovalStep | null {
  return remainingSteps(record)[0] ?? null;
}

export function removalComplete(record: RemovalOwed): boolean {
  return nextStep(record) === null;
}

/**
 * Whether the record may be continued WITHOUT the person (a landing, a
 * timer). Only once the revoke has its answer: before that nothing was
 * removed, and the revoke itself needs a fresh proof.
 */
export function resumesByItself(record: RemovalOwed): boolean {
  return record.done.revoke && !removalComplete(record);
}

/** What the Devices list shows for a device with a record (UX 18.1). */
export type RemovalRowState =
  /** No record, or one whose revoke never got an answer: the normal row
   *  with Remove (the same button retries). */
  | "normal"
  /** Signed out at the server, the rest still finishing by itself:
   *  `Removing...`, no control, no error. */
  | "finishing"
  /** Signed out at the server, and the owed DeviceGroup step needs the
   *  person's passkey again: `Removing...` and the one button `Finish
   *  removing`. Never a device revoked at auth and left in the DeviceGroup
   *  with no way to finish. */
  | "needs-person";

export function removalRowState(record: RemovalOwed | null): RemovalRowState {
  if (record === null || !resumesByItself(record)) return "normal";
  return record.waitsOnPerson === true && !record.done.deviceGroup ? "needs-person" : "finishing";
}

/** The stored attestation when it is still inside its lifetime. */
export function usableAttestation(record: RemovalOwed, nowMs: number): string | null {
  if (record.attestationB64 === undefined || record.attestationExpiresAtMs === undefined) return null;
  return record.attestationExpiresAtMs > nowMs ? record.attestationB64 : null;
}

// ---------------------------------------------------------------------------
// What each step's answer means
// ---------------------------------------------------------------------------

/** What the answer to one step means for the operation. */
export type StepVerdict =
  /** The step has its answer: mark it and go on. */
  | "done"
  /** No verdict yet (offline, a `5xx`, a limit): keep the record, stop
   *  this run, try again later. */
  | "later"
  /** The revoke was refused: nothing was removed. Drop the record. */
  | "refused";

/** Why a revoke did not go through, for the sheet (UX 18.1's table). */
export type RevokeFailure =
  /** `403 fresh-auth-required`: the proof block, no error. */
  | "proof-required"
  /** `403 account-key-required`: auth has no recorded account key yet, or
   *  the statement did not verify. `devices.remove.error.try_later`. */
  | "account-key-required"
  /** `429`: the shared countdown. */
  | "rate-limited"
  /** No answer: `devices.remove.error.not_revoked` (the sign-out could not
   *  be confirmed; the same button retries). */
  | "unconfirmed"
  /** Any other refusal (`400`, an unexpected status). */
  | "other";

/**
 * The revoke's answer. `status` is `null` when no answer arrived.
 *
 * A request with no answer is the one case where the client cannot know
 * whether the device was signed out: the record is KEPT (not continued by
 * itself, see {@link resumesByItself}) so the same button completes it.
 */
export function revokeVerdict(
  status: number | null,
  errorCode: string | null,
): { readonly verdict: StepVerdict; readonly failure: RevokeFailure | null } {
  if (status === 204 || status === 200) return { verdict: "done", failure: null };
  if (status === null || status >= 500) return { verdict: "later", failure: "unconfirmed" };
  if (status === 429) return { verdict: "refused", failure: "rate-limited" };
  if (status === 403 && errorCode === "fresh-auth-required") {
    return { verdict: "refused", failure: "proof-required" };
  }
  if (status === 403 && errorCode === "account-key-required") {
    return { verdict: "refused", failure: "account-key-required" };
  }
  return { verdict: "refused", failure: "other" };
}

/**
 * The directory delete's answer. The revoke already landed, so nothing here
 * drops the record: an answer that is not `204` waits. `403
 * account-key-required` (the statement did not verify under the directory's
 * key, e.g. a clock more than two minutes off) and a `401` both wait for a
 * later run with a new statement.
 */
export function directoryVerdict(status: number | null): StepVerdict {
  return status === 204 || status === 200 || status === 404 ? "done" : "later";
}

/** What the DeviceGroup step found. */
export type DeviceGroupStepResult =
  /** The Remove commit was published, or the device holds no leaf in the
   *  DeviceGroup any more (another device removed it). */
  | "removed"
  /** The account's policy needs a passkey attestation and none is usable:
   *  the person must make the gesture (the row's button). */
  | "needs-gesture"
  /** Could not be done now. */
  | "later";

/** What the chats step found: how many chats did not answer. */
export function chatsVerdict(result: { readonly failed: number } | null): StepVerdict {
  return result !== null && result.failed === 0 ? "done" : "later";
}

// ---------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------

/** The ports of {@link runRemoval}. Each returns its step's answer and
 *  never throws for an HTTP refusal (a throw reads as "no answer"). */
export interface RemovalPorts {
  /** Step 1. `null` status: no answer. Called only when `grant` is given. */
  readonly revoke: (
    deviceIdB64: string,
    grant: string,
  ) => Promise<{ readonly status: number | null; readonly errorCode: string | null }>;
  /** Step 2: sign a statement and delete the directory row. */
  readonly deleteDirectoryRow: (deviceIdB64: string) => Promise<number | null>;
  /** Step 3. `attestationB64` is the usable stored attestation, or `null`. */
  readonly removeFromDeviceGroup: (
    deviceIdB64: string,
    attestationB64: string | null,
  ) => Promise<DeviceGroupStepResult>;
  /** Step 4: propose the leaf out of every chat. `null`: not runnable now. */
  readonly retireFromChats: (deviceIdB64: string) => Promise<{ readonly failed: number } | null>;
  /** Persist the record after every step (or drop it: `null`). */
  readonly save: (deviceIdB64: string, record: RemovalOwed | null) => void;
}

/** How one run of the operation ended. */
export type RemovalOutcome =
  /** Every step has its answer; the record is gone. */
  | { readonly kind: "complete" }
  /** The revoke was refused or unconfirmed: nothing was removed. */
  | { readonly kind: "not-revoked"; readonly failure: RevokeFailure }
  /** Signed out at the server; `owed` still finishing by itself. */
  | { readonly kind: "finishing"; readonly owed: readonly RemovalStep[]; readonly needsGesture: boolean };

/**
 * Run the operation from `record` as far as it goes.
 *
 * `grant` is the fresh-auth grant for the revoke; without one a record whose
 * revoke is not done runs nothing (`not-revoked` / `proof-required`).
 */
export async function runRemoval(
  record: RemovalOwed,
  ports: RemovalPorts,
  options: { readonly grant: string | null; readonly now: () => number },
): Promise<RemovalOutcome> {
  let current = record;
  const save = (next: RemovalOwed | null): void => ports.save(record.deviceIdB64, next);
  if (!current.done.revoke && options.grant === null) {
    // Nothing can be sent without the proof; no record is written for an
    // operation that did not start.
    return { kind: "not-revoked", failure: "proof-required" };
  }
  // Written before anything is sent.
  save(current);
  let needsGesture = false;
  for (;;) {
    const step = nextStep(current);
    if (step === null) {
      save(null);
      return { kind: "complete" };
    }
    let verdict: StepVerdict;
    if (step === "revoke") {
      const grant = options.grant;
      if (grant === null) return { kind: "not-revoked", failure: "proof-required" };
      let answer: { readonly status: number | null; readonly errorCode: string | null };
      try {
        answer = await ports.revoke(current.deviceIdB64, grant);
      } catch {
        answer = { status: null, errorCode: null };
      }
      const read = revokeVerdict(answer.status, answer.errorCode);
      if (read.verdict !== "done") {
        // A refusal means nothing was removed: no record. No answer keeps
        // the record so the same button completes it.
        if (read.verdict === "refused") save(null);
        return { kind: "not-revoked", failure: read.failure ?? "other" };
      }
      verdict = "done";
    } else if (step === "directory") {
      let status: number | null;
      try {
        status = await ports.deleteDirectoryRow(current.deviceIdB64);
      } catch {
        status = null;
      }
      verdict = directoryVerdict(status);
    } else if (step === "device-group") {
      let result: DeviceGroupStepResult;
      try {
        result = await ports.removeFromDeviceGroup(
          current.deviceIdB64,
          usableAttestation(current, options.now()),
        );
      } catch {
        result = "later";
      }
      if (result === "needs-gesture") {
        needsGesture = true;
        // Stored: the row offers `Finish removing` after a relaunch too.
        current = withWaitingOnPerson(current);
        save(current);
      }
      verdict = result === "removed" ? "done" : "later";
    } else {
      let result: { readonly failed: number } | null;
      try {
        result = await ports.retireFromChats(current.deviceIdB64);
      } catch {
        result = null;
      }
      verdict = chatsVerdict(result);
    }
    if (verdict !== "done") {
      return { kind: "finishing", owed: remainingSteps(current), needsGesture };
    }
    current = withStepDone(current, step);
    save(removalComplete(current) ? null : current);
  }
}

// ---------------------------------------------------------------------------
// Storage: the owed records of an account
// ---------------------------------------------------------------------------

function storageKey(accountIdB64: string): string {
  return `lakko:device-removal-owed:${accountIdB64}`;
}

function isRecord(value: unknown): value is RemovalOwed {
  if (typeof value !== "object" || value === null) return false;
  const r = value as Partial<RemovalOwed>;
  const d = r.done as Partial<RemovalOwed["done"]> | undefined;
  return (
    r.v === 1 &&
    typeof r.deviceIdB64 === "string" &&
    /^[A-Za-z0-9_-]{43}$/.test(r.deviceIdB64) &&
    typeof r.startedAtMs === "number" &&
    typeof d === "object" &&
    d !== null &&
    typeof d.revoke === "boolean" &&
    typeof d.directory === "boolean" &&
    typeof d.deviceGroup === "boolean" &&
    typeof d.chats === "boolean" &&
    (r.attestationB64 === undefined || typeof r.attestationB64 === "string") &&
    (r.attestationExpiresAtMs === undefined || typeof r.attestationExpiresAtMs === "number") &&
    (r.waitsOnPerson === undefined || typeof r.waitsOnPerson === "boolean")
  );
}

/** Every owed removal of the account. A damaged row reads as none. */
export function readRemovals(storage: MarkerStorage | undefined, accountIdB64: string): RemovalOwed[] {
  try {
    const raw = storage?.getItem(storageKey(accountIdB64)) ?? null;
    if (raw === null) return [];
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter(isRecord) : [];
  } catch {
    return [];
  }
}

export function readRemoval(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  deviceIdB64: string,
): RemovalOwed | null {
  return readRemovals(storage, accountIdB64).find((r) => r.deviceIdB64 === deviceIdB64) ?? null;
}

/** Write (or, with `null`, drop) the record of one device. */
export function writeRemoval(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  deviceIdB64: string,
  record: RemovalOwed | null,
): void {
  try {
    const rest = readRemovals(storage, accountIdB64).filter((r) => r.deviceIdB64 !== deviceIdB64);
    const next = record === null ? rest : [...rest, record];
    if (next.length === 0) storage?.removeItem(storageKey(accountIdB64));
    else storage?.setItem(storageKey(accountIdB64), JSON.stringify(next));
  } catch {
    // Storage blocked: the operation still runs to its end in this page.
  }
}

export function clearRemovals(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(storageKey(accountIdB64));
  } catch {
    // nothing to clear
  }
}

/**
 * Whether a removal may START (or continue) now (ADR-0074 T1 point 15):
 * while a key-reset publish is owed, auth verifies a `device-revoke`
 * statement under the new key and the directory under the old one, so no
 * removal runs in that window.
 */
export function removalAllowed(input: { readonly keyResetPublishOwed: boolean }): boolean {
  return !input.keyResetPublishOwed;
}
