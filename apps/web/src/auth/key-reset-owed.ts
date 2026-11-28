// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "This device still owes the directory and the device list the new keys"
 * (ADR-0073 section 2.3; `docs/ux/identity-recovery.md` section 15.4).
 *
 * A reset is one server operation (`POST /account/keys/reset/finish`): once
 * it answered, the account is at a new key generation, every other device is
 * being signed out, and this device holds the new seed, the new session pair
 * and an auth-signed key-reset attestation. What can still fail afterwards
 * is PUBLISHING: the identity bundle upload that carries the attestation to
 * discovery, and the attested reset of the DeviceGroup journal on the
 * Delivery Service. Both are retried with the SAME attestation (a replay is
 * harmless: a generation that already landed answers `409
 * key-generation-not-newer`, which reads as done), so the attestation and
 * the two step marks are kept here, in plain storage, until both landed.
 * Only public, auth-signed material is stored; the seed stays under the
 * device key.
 *
 * The successor of the "recovery credential owed" marker of ADR-0072: the
 * credential now moves with the generation in the reset transaction, so
 * there is nothing of it left to owe.
 *
 * While the publish is owed the new words are withheld (a phrase for an
 * identity nobody can find is not yet a recovery phrase), and an attestation
 * older than {@link KEY_RESET_ATTESTATION_MAX_AGE_MS} is not applied until
 * the person signs in again (ADR-0073 section 4.2, last paragraph).
 */
import type { MarkerStorage } from "./phrase-gate";

/** The client-side bound on applying a stored attestation: 30 days from its
 *  `signed_at`. Chosen (ADR-0073 section 4.2), not measured. */
export const KEY_RESET_ATTESTATION_MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000;

/** The two server steps a reset still owes after `reset/finish`. */
export type KeyResetStep = "directory" | "device-group";

/** The stored record. */
export interface KeyResetOwed {
  readonly v: 1;
  /** URL-safe base64 (no pad) of the 145-byte attestation, as auth returned
   *  it. Re-encoded to standard base64 for discovery and the DS. */
  readonly attestationB64Url: string;
  /** The generation the reset moved the account to (decimal string: JSON
   *  has no u64). */
  readonly keyGeneration: string;
  /** Standard base64 of the new 32-byte account signature key, so a retry
   *  re-verifies the attestation against the key it binds. */
  readonly newSignaturePubkeyB64: string;
  /** The attestation's `signed_at`, unix milliseconds. */
  readonly signedAtMs: number;
  /** Which steps landed. */
  readonly done: { readonly directory: boolean; readonly deviceGroup: boolean };
  /** Unix milliseconds until which the directory refuses the new keys (its
   *  cool-down, ADR-0073 section 4.1; UX 15.4 "Waiting for the directory"),
   *  from the last refusal. Absent when the last attempt was not refused so. */
  readonly waitUntilMs?: number;
  /** ADR-0101 section 10: standard base64 of the 214-byte reset seal the
   *  attested publish carries, so a retry sends the same one. Absent for an
   *  unsealed reset. A public artefact (a signature), not a secret. */
  readonly resetSealB64?: string;
}

function markerKey(accountIdB64: string): string {
  return `lakko:key-reset-owed:${accountIdB64}`;
}

/** Write the record (a fresh reset: nothing done yet, or a step update). */
export function writeKeyResetOwed(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  record: KeyResetOwed,
): void {
  try {
    storage?.setItem(markerKey(accountIdB64), JSON.stringify(record));
  } catch {
    // Storage blocked: the in-memory copy still drives this page.
  }
}

/** The record, or `null` when nothing is owed (or the row is unreadable:
 *  a damaged row cannot drive a retry and is treated as absent). */
export function readKeyResetOwed(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
): KeyResetOwed | null {
  try {
    const raw = storage?.getItem(markerKey(accountIdB64));
    if (raw === null || raw === undefined) return null;
    const parsed = JSON.parse(raw) as Partial<KeyResetOwed>;
    if (
      parsed.v !== 1 ||
      typeof parsed.attestationB64Url !== "string" ||
      typeof parsed.keyGeneration !== "string" ||
      typeof parsed.newSignaturePubkeyB64 !== "string" ||
      typeof parsed.signedAtMs !== "number" ||
      typeof parsed.done !== "object" ||
      parsed.done === null
    ) {
      return null;
    }
    const waitUntilMs =
      typeof parsed.waitUntilMs === "number" && Number.isFinite(parsed.waitUntilMs)
        ? parsed.waitUntilMs
        : undefined;
    return {
      v: 1,
      attestationB64Url: parsed.attestationB64Url,
      keyGeneration: parsed.keyGeneration,
      newSignaturePubkeyB64: parsed.newSignaturePubkeyB64,
      signedAtMs: parsed.signedAtMs,
      done: {
        directory: parsed.done.directory === true,
        deviceGroup: parsed.done.deviceGroup === true,
      },
      ...(waitUntilMs !== undefined ? { waitUntilMs } : {}),
      ...(typeof parsed.resetSealB64 === "string" && parsed.resetSealB64.length > 0
        ? { resetSealB64: parsed.resetSealB64 }
        : {}),
    };
  } catch {
    return null;
  }
}

/** Nothing is owed any more (both steps landed), or the account's data is
 *  wiped. */
export function clearKeyResetOwed(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(markerKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}

/** Whether a publish is owed for `accountIdB64`. */
export function isKeyResetOwed(storage: MarkerStorage | undefined, accountIdB64: string): boolean {
  const record = readKeyResetOwed(storage, accountIdB64);
  return record !== null && remainingKeyResetSteps(record.done).length > 0;
}

// ---------------------------------------------------------------------------
// The pure rules
// ---------------------------------------------------------------------------

/** The fixed length of the attestation (`lakko-attest`, ADR-0073 2.2;
 *  145 bytes since ADR-0078 phase 6 dropped the DeviceGroup journal id). */
export const KEY_RESET_ATTESTATION_LEN = 145;

/** Byte offset of `key_generation` in the fixed layout:
 *  `version(1) ‖ account_ref(32) ‖ key_generation(8) ‖
 *  new_signature_pubkey(32) ‖ signed_at(8 BE i64) ‖ sig(64)`. */
const GENERATION_OFFSET = 1 + 32;

/** Byte offset of `signed_at_unix_millis` in the same layout. */
const SIGNED_AT_OFFSET = GENERATION_OFFSET + 8 + 32;

/**
 * The attestation's `signed_at`, read off the wire layout for the record
 * (the verifier returns it too, but only once a key is in hand). Throws on
 * a blob of the wrong length. Reading a field is not verifying it: the
 * signature is checked under the pinned key before the attestation is used.
 */
export function keyResetAttestationSignedAtMs(attestation: Uint8Array): number {
  if (attestation.length !== KEY_RESET_ATTESTATION_LEN) {
    throw new Error(`key-reset attestation is ${attestation.length} bytes, expected ${KEY_RESET_ATTESTATION_LEN}`);
  }
  const view = new DataView(attestation.buffer, attestation.byteOffset + SIGNED_AT_OFFSET, 8);
  return Number(view.getBigInt64(0, false));
}

/** The attestation's `key_generation`, read off the wire layout. */
export function keyResetAttestationGeneration(attestation: Uint8Array): bigint {
  if (attestation.length !== KEY_RESET_ATTESTATION_LEN) {
    throw new Error(`key-reset attestation is ${attestation.length} bytes, expected ${KEY_RESET_ATTESTATION_LEN}`);
  }
  const view = new DataView(attestation.buffer, attestation.byteOffset + GENERATION_OFFSET, 8);
  return view.getBigUint64(0, false);
}

/** The steps still to run, in the order they run: the directory first (the
 *  published identity is what every contact and device reads), then the
 *  DeviceGroup journal. */
export function remainingKeyResetSteps(done: KeyResetOwed["done"]): KeyResetStep[] {
  const out: KeyResetStep[] = [];
  if (!done.directory) out.push("directory");
  if (!done.deviceGroup) out.push("device-group");
  return out;
}

/** The record with `step` marked done (and no directory wait left). */
export function withKeyResetStepDone(record: KeyResetOwed, step: KeyResetStep): KeyResetOwed {
  return withDirectoryWait(
    {
      ...record,
      done: {
        directory: record.done.directory || step === "directory",
        deviceGroup: record.done.deviceGroup || step === "device-group",
      },
    },
    null,
  );
}

/** The record with the directory's wait set (`untilMs`) or cleared (`null`). */
export function withDirectoryWait(record: KeyResetOwed, untilMs: number | null): KeyResetOwed {
  const rest: KeyResetOwed = {
    v: 1,
    attestationB64Url: record.attestationB64Url,
    keyGeneration: record.keyGeneration,
    newSignaturePubkeyB64: record.newSignaturePubkeyB64,
    signedAtMs: record.signedAtMs,
    done: record.done,
    ...(record.resetSealB64 !== undefined ? { resetSealB64: record.resetSealB64 } : {}),
  };
  return untilMs === null ? rest : { ...rest, waitUntilMs: untilMs };
}

/**
 * The seconds the directory asked the reset's identity upload to wait, or
 * `null` when the refusal is not its cool-down or names no usable time
 * (`429 identity-changed-too-recently` with `retry_after_secs`, or a
 * `Retry-After` header; UX 15.4: without a readable time the "could not
 * finish" copy and Try again stay).
 */
export function directoryCooldownSecs(input: {
  readonly status: number;
  readonly errorCode: string | null;
  readonly retryAfterSecs: number | null;
}): number | null {
  if (input.status !== 429 || input.errorCode !== "identity-changed-too-recently") return null;
  const secs = input.retryAfterSecs;
  return secs !== null && Number.isFinite(secs) && secs > 0 ? Math.ceil(secs) : null;
}

/** What the "publish owed" screen shows (UX 15.4). */
export interface PublishOwedView {
  /** The catalog key of the explanation. */
  readonly textKey: string;
  /** `{minutes}` for the waiting copy, `null` for the others. */
  readonly minutes: number | null;
  /** Whether "Try again" is offered. */
  readonly tryAgain: boolean;
  /** Whether the directory's wait is running (the screen retries by itself
   *  when it ends). */
  readonly waiting: boolean;
}

/**
 * The rule of the "publish owed" screen. While the directory's wait runs
 * (or the retry it ends in is out), the waiting copy with the minutes left
 * (`ceil(seconds / 60)`, at least 1) and no Try again; otherwise the
 * existing copy and Try again. An account that was never shown words has
 * its own wordless copies (`account.keys_changed.finishing`, and
 * `account.keys_changed.waiting` while it waits, also without Try again).
 * The sign-in-needed state is unchanged.
 */
export function publishOwedView(input: {
  readonly waitUntilMs: number | null;
  readonly nowMs: number;
  /** A retry is out right now (the automatic one at the end of the wait). */
  readonly retrying: boolean;
  readonly wordsShown: boolean;
  readonly signInNeeded: boolean;
}): PublishOwedView {
  if (input.signInNeeded) {
    return { textKey: "recovery_phrase.fresh.sign_in_to_finish", minutes: null, tryAgain: false, waiting: false };
  }
  const owedKey = input.wordsShown ? "recovery_phrase.fresh.publish_owed" : "account.keys_changed.finishing";
  const leftMs = input.waitUntilMs === null ? 0 : input.waitUntilMs - input.nowMs;
  if (input.waitUntilMs === null || (leftMs <= 0 && !input.retrying)) {
    return { textKey: owedKey, minutes: null, tryAgain: true, waiting: false };
  }
  const minutes = Math.max(1, Math.ceil(Math.max(0, leftMs) / 60_000));
  const waitingKey = input.wordsShown ? "recovery_phrase.fresh.publish_waiting" : "account.keys_changed.waiting";
  return { textKey: waitingKey, minutes, tryAgain: false, waiting: true };
}

/** Whether a stored attestation may still be applied, or the person must
 *  sign in first (section 15.4: "Sign in again to finish publishing your new
 *  keys."). A clock behind the mint reads as fresh. */
export type AttestationAge = "usable" | "too-old";

export function attestationAge(signedAtMs: number, nowMs: number): AttestationAge {
  return nowMs - signedAtMs > KEY_RESET_ATTESTATION_MAX_AGE_MS ? "too-old" : "usable";
}

/**
 * Whether a stored attestation is applied now: always while it is within
 * the bound; past it, only after an explicit sign-in in this page (a
 * password, a passkey, a recovery factor), which issues nothing new but
 * confirms the account is still the person's before a weeks-old reset is
 * pushed to the directory (ADR-0073 section 4.2). A silent restore does not
 * count.
 */
export function attestationGate(
  signedAtMs: number,
  nowMs: number,
  explicitSignInThisPage: boolean,
): "apply" | "sign-in-needed" {
  if (attestationAge(signedAtMs, nowMs) === "usable") return "apply";
  return explicitSignInThisPage ? "apply" : "sign-in-needed";
}

/** What one publishing step came to. */
export type KeyResetStepOutcome =
  /** The step landed, or had already landed (a retry after a crash). */
  | "done"
  /** A transient refusal: keep the record, offer "Try again". */
  | "retry"
  /** The session is of a superseded generation: another reset overtook this
   *  one. The signed-out path, not a retry. */
  | "superseded"
  /** The server refused the attestation itself (invalid, or the service has
   *  no pinned key): keep the record, offer "Try again", say what it was. */
  | "refused";

/**
 * The directory upload's answer (ADR-0073 section 4.1, contract section 5
 * step 3). A `409 key-generation-not-newer` on a retry means the reset
 * already landed there; `409 identity-change-needs-attestation` means the
 * attestation did not reach the server (a client defect, not retryable as
 * is, but reported rather than looped).
 */
export function directoryResetOutcome(input: {
  readonly status: number;
  readonly errorCode: string | null;
}): KeyResetStepOutcome {
  if (input.status >= 200 && input.status < 300) return "done";
  if (input.status === 409 && input.errorCode === "key-generation-not-newer") return "done";
  if (input.status === 401 && input.errorCode === "key-generation-superseded") return "superseded";
  if (input.status === 400 && input.errorCode === "key-reset-attestation-invalid") return "refused";
  if (input.status === 409) return "refused";
  return "retry";
}

// ---------------------------------------------------------------------------
// Settings > Security > Recovery > "Show recovery phrase"
// ---------------------------------------------------------------------------

/**
 * What the row does when the person asks for the words. While the publish
 * is owed the stored phrase IS the account's recovery phrase (the credential
 * moved with the reset), but the identity it belongs to is not published
 * yet, so the words are withheld exactly as the automatic phrase screen
 * withholds them (`SaveFreshPhrasePrompt`): the pending explanation with
 * "Try again". The words are shown only once nothing is owed.
 */
export type PhraseRevealStep =
  /** No phrase is stored on this device: nothing to show. */
  | "not-saved"
  /** A publish is owed: the pending explanation and "Try again". */
  | "publish-pending"
  /** Nothing is owed: re-authenticate, then show the words. */
  | "reveal";

export function phraseRevealStep(facts: {
  readonly phraseOnDevice: boolean;
  readonly publishOwed: boolean;
}): PhraseRevealStep {
  if (!facts.phraseOnDevice) return "not-saved";
  return facts.publishOwed ? "publish-pending" : "reveal";
}
