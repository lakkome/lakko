// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Backup-head restore evaluation — the IO that resolves the [`RestoreGateFacts`]
 * (ADR-0037 §6, G-11) and applies the pure [`restoreVerdict`] policy.
 *
 * This is the impure half of the restore gate: it peeks the storage blob, parses
 * the framing, computes the envelope hash, fetches the pinned attest key, reads
 * the persisted floor, and runs the wasm verify — then hands the resolved facts
 * to `head-gate.ts`. Kept separate from the pure classifier so the security
 * policy stays unit-testable without wasm / IDB / network.
 */

import { loadWasm } from "../wasm";
import {
  fetchDeviceAttestKey,
  pinnedDeviceAttestKey,
} from "../auth/device-attest-key";
import { getFloor } from "./floor-store";
import {
  backupHeadStalenessMillis,
  NO_FACTS,
  restoreVerdict,
  type RestoreGateFacts,
  type RestoreVerdict,
} from "./head-gate";

/** The resolved outcome of evaluating a storage blob against the restore gate. */
export interface RestoreEvaluation {
  /** The terminal verdict from [`restoreVerdict`]. */
  readonly verdict: RestoreVerdict;
  /** The envelope portion to open with the phrase (the whole blob for a legacy
   *  blob; the tail after `TAG ‖ head` for a framed one). */
  readonly envelope: Uint8Array;
  /** `BLAKE3(envelope)` — carried so a clean `proceed` can persist the floor
   *  without recomputing. `null` when the blob failed to parse. */
  readonly envelopeHash: Uint8Array | null;
  /** The verified `backup_version` on a `proceed`, else `null`. */
  readonly verifiedVersion: bigint | null;
}

/** Inputs the restore seam threads in. */
export interface RestoreEvaluationInput {
  /** The raw bytes fetched from `GET /backup`. */
  readonly blob: Uint8Array;
  /** The 16-byte account_id the client authenticated as (head binding). */
  readonly accountId: Uint8Array;
  /** Base64 account id — the floor-store key. */
  readonly accountIdB64: string;
  /** Non-extractable device key for the floor store. */
  readonly deviceKey: CryptoKey;
  /** Auth base for the attest-key fetch (default the shared `/api/auth`). */
  readonly authBaseUrl?: string | undefined;
  /** Injectable clock in ms (default `Date.now()`). */
  readonly now?: number | undefined;
  /** Injectable staleness window in ms (default from the env knob). */
  readonly stalenessMillis?: bigint | undefined;
}

function errText(err: unknown): string {
  if (err instanceof Error) return err.message;
  const maybe = err as { message?: unknown };
  if (maybe && typeof maybe.message === "string") return maybe.message;
  return String(err);
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

/** Build a [`RestoreEvaluation`] from resolved facts (runs the pure classifier). */
function evalOf(
  facts: RestoreGateFacts,
  envelope: Uint8Array,
  envelopeHash: Uint8Array | null,
  verifiedVersion: bigint | null,
): RestoreEvaluation {
  return { verdict: restoreVerdict(facts), envelope, envelopeHash, verifiedVersion };
}

/**
 * Evaluate a storage blob against the ADR-0037 §6 restore gate.
 *
 * Resolves each fact via IO then returns the [`restoreVerdict`]. The caller
 * (BackupClient.restore) acts on the verdict: `hard-fail` → refuse + alarm;
 * `stale-override` → ask the user; `fail-open` / `proceed` → open the envelope
 * (and, on a clean `proceed`, persist the floor from `verifiedVersion` +
 * `envelopeHash`).
 */
export async function evaluateRestoreBlob(
  input: RestoreEvaluationInput,
): Promise<RestoreEvaluation> {
  const wasm = await loadWasm();
  const now = input.now ?? Date.now();
  const stalenessMillis = input.stalenessMillis ?? backupHeadStalenessMillis();

  // 1. Parse the framing. A truncated / corrupt FRAMED blob throws `invalid:`
  //    → hard-fail (active tamper). A legacy / empty blob parses as unframed.
  let parsed: {
    framed: boolean;
    head: Uint8Array | undefined;
    envelope: Uint8Array;
  };
  try {
    parsed = wasm.parseBackupBlob(input.blob);
  } catch {
    return evalOf({ ...NO_FACTS, parseError: true }, input.blob, null, null);
  }

  const envelope = parsed.envelope;

  // 2. Read the persisted floor as a discriminated absent / present / corrupt
  //    (F5a). A CORRUPT anchor dominates every framing case below — the local
  //    anchor once existed and is now untrustworthy, so the classifier routes it
  //    to stale-override, never silently demoting to the weaker fresh path.
  const floorRead = await getFloor(input.accountIdB64, input.deviceKey);
  const hasFloor = floorRead.kind === "present";
  const floorCorrupt = floorRead.kind === "corrupt";
  const floor = floorRead.kind === "present" ? floorRead.floor : null;

  if (floorCorrupt) {
    return evalOf(
      { ...NO_FACTS, framed: parsed.framed, floorCorrupt: true },
      envelope,
      null,
      null,
    );
  }

  // 3. Absent head (F4). Split on the floor: no floor → fail-open (true
  //    zero-anchor fresh install); floor + byte-identical envelope → proceed;
  //    floor + a DIFFERENT headless envelope → stale-override (head-stripping
  //    downgrade against head-protected history). The phrase-AEAD still protects
  //    the content in every case — invariant #4 untouched.
  if (!parsed.framed || parsed.head === undefined) {
    if (!hasFloor) {
      return evalOf({ ...NO_FACTS, framed: false, hasFloor: false }, envelope, null, null);
    }
    const envelopeHash = wasm.backupEnvelopeHash(envelope);
    const absentHashMatchesFloor =
      floor !== null && bytesEqual(envelopeHash, floor.envelopeHash);
    return evalOf(
      { ...NO_FACTS, framed: false, hasFloor: true, absentHashMatchesFloor },
      envelope,
      envelopeHash,
      null,
    );
  }

  const head = parsed.head;
  const envelopeHash = wasm.backupEnvelopeHash(envelope);

  // 4. A framed head we cannot verify (no pinned + no JWKS attest key) is a
  //    misconfig, NOT an absent head — fail-closed (hard), never silently
  //    accept an unverified framed head (ADR-0037 §6).
  let attestKey: Uint8Array;
  try {
    attestKey = await fetchDeviceAttestKey(input.authBaseUrl);
  } catch {
    return {
      verdict: {
        kind: "hard-fail",
        reason: "attest key unavailable — cannot verify the backup head",
      },
      envelope,
      envelopeHash,
      verifiedVersion: null,
    };
  }

  // F1: the rollback guarantee is only as strong as the verifying key. When the
  //     key came from the JWKS endpoint (pin unset) rather than the build-time
  //     pin, a compromised auth origin could serve a forged key + head — surface
  //     the weakened posture loudly. Do NOT hard-fail (that would brick unpinned
  //     rollouts); the head is still verified, just against a less-trusted key.
  if (pinnedDeviceAttestKey() === null) {
    // eslint-disable-next-line no-console
    console.warn(
      "[backup] backup-head verified against an UNPINNED (JWKS-served) attest " +
        "key — set VITE_LAKKO_DEVICE_ATTEST_KEY for full rollback protection",
    );
  }

  // 5. Framed + valid floor state.
  if (hasFloor && floor !== null) {
    // Returning client: signature + account binding only, then hash + floor.
    try {
      const verified = wasm.verifyBackupHeadBinding(
        head,
        attestKey,
        input.accountId,
      );
      const hashMismatch = !bytesEqual(verified.envelopeHash, envelopeHash);
      const versionBelowFloor = verified.backupVersion < floor.version;
      const verdict = restoreVerdict({
        ...NO_FACTS,
        framed: true,
        hasFloor: true,
        hashMismatch,
        versionBelowFloor,
      });
      return {
        verdict,
        envelope,
        envelopeHash,
        verifiedVersion: verdict.kind === "proceed" ? verified.backupVersion : null,
      };
    } catch {
      return evalOf(
        { ...NO_FACTS, framed: true, hasFloor: true, bindingInvalid: true },
        envelope,
        envelopeHash,
        null,
      );
    }
  }

  // Fresh install: fold signature + account + hash + freshness into one call.
  try {
    const verified = wasm.verifyBackupHead(
      head,
      attestKey,
      input.accountId,
      envelopeHash,
      BigInt(now),
      stalenessMillis,
    );
    return evalOf(
      { ...NO_FACTS, framed: true, hasFloor: false },
      envelope,
      envelopeHash,
      verified.backupVersion,
    );
  } catch (err) {
    const stale = errText(err).startsWith("stale:");
    return evalOf(
      {
        ...NO_FACTS,
        framed: true,
        hasFloor: false,
        // Any non-`stale:` failure (including `invalid:`) is a hard tamper.
        freshInvalid: !stale,
        freshStale: stale,
      },
      envelope,
      envelopeHash,
      null,
    );
  }
}
