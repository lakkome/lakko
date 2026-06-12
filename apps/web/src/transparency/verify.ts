// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Transparency-log inclusion verification (KT-log S3c) — the
 * equivocation-detection / gossip layer on top of the ADR-0016
 * supersession/pin gate.
 *
 * `verifyIdentityInLog(accountId, publishedSignaturePubkey)` consults
 * the operator-signed, append-only transparency log to confirm that
 * the identity discovery just served for a contact is the SAME one the
 * public log commits to. A divergence (the log holds a different key,
 * or the log shrank) is surfaced so the caller can act.
 *
 * State machine (`LogVerifyStatus`):
 *
 *   * `"off"`         — the integration is unconfigured (no URL or no
 *                       pinned operator key). The whole check is
 *                       skipped.
 *   * `"rollback"`    — the signed root's `log_size` is SMALLER than
 *                       the highest we have seen. An append-only log
 *                       cannot shrink → an unambiguous attack. HARD.
 *   * `"absent"`      — `/latest` 404s: the account is not yet in the
 *                       log. Benign under S2.5 eventual consistency OR
 *                       an off-log equivocation — surfaced, not
 *                       auto-trusted.
 *   * `"mismatch"`    — the log's latest entry pubkey differs from what
 *                       discovery served, OR the inclusion proof failed
 *                       to verify. Suspicious.
 *   * `"unreachable"` — any network error fetching root/latest/proof.
 *   * `"confirmed"`   — root is operator-signed (pinned key), no
 *                       rollback, and the served pubkey is proven
 *                       included at the latest version.
 *
 * The function is BEST-EFFORT and never throws on a network/log fault:
 * it maps faults to a status. The CALLER decides the posture (see
 * `contacts/client.ts`) — for S3c, only `"rollback"` is a hard failure.
 */

import {
  TransparencyClient,
  TransparencyNetworkError,
} from "./client";
import type { SignedRoot, InclusionProof } from "./client";
import { accountKey } from "./account-key";
import { checkAndAdvance } from "./last-seen-root";
import { pinnedOperatorKey } from "./config";
import { loadWasm } from "../wasm";

/** The verdict of a log-inclusion check for one fetched identity. */
export type LogVerifyStatus =
  | "confirmed"
  | "absent"
  | "rollback"
  | "mismatch"
  | "unreachable"
  | "off";

/**
 * The outcome of `verifyIdentityInLog` (KT-log S4).
 *
 * Adds the `is_reset` marker the log serves on its `/latest` entry to
 * the bare status. `isReset` lets a contact DISTINGUISH an
 * owner-initiated identity RESET (the lost-everything fresh-identity
 * mint — logged with `is_reset=true`, ADR-0017) from an unexplained key
 * change (a possible compromise — `is_reset` false/absent).
 *
 * `isReset` is the `/latest` entry's marker WHENEVER a latest entry was
 * observed — i.e. on `confirmed` AND on `mismatch` (both imply a latest
 * exists). It is `null` when there was no latest entry to read it from:
 * `absent` (account not yet logged), `rollback` (short-circuited before
 * the latest lookup), `unreachable` (network/log fault), and `off` (the
 * integration is unconfigured).
 */
export interface LogVerifyResult {
  /** The S3c state-machine verdict. */
  status: LogVerifyStatus;
  /** The `/latest` entry's `is_reset` marker when a latest entry was
   *  read (`confirmed` / `mismatch`); `null` otherwise. */
  isReset: boolean | null;
}

/**
 * Optional injection seam for tests — lets a caller supply a mocked
 * `TransparencyClient` and a fixed pinned operator key without going
 * through the build-time env config. Production callers pass nothing;
 * `verifyIdentityInLog` resolves both from the build config.
 */
export interface VerifyDeps {
  /** Pre-built client, or `null` to mean "integration off". When
   *  omitted, `TransparencyClient.forOptionalConfig()` is used. */
  client?: TransparencyClient | null;
  /** The pinned operator verifying key (32 bytes), or `null` for
   *  "off". When omitted, `pinnedOperatorKey()` is used. */
  operatorKey?: Uint8Array | null;
  /**
   * Inclusion-gate override (TEST SEAM ONLY). When supplied, this
   * replaces the real wasm `verifyLogInclusion` call: it should THROW
   * to simulate an inclusion/root-signature failure and RETURN to
   * simulate success — exactly the wasm contract. Production callers
   * never pass this; building a valid Merkle proof requires the Rust
   * log builder (not exposed to wasm), so tests stub this seam to
   * cover the `confirmed` / proof-fail `mismatch` branches.
   */
  verifyInclusion?: (args: {
    operatorKey: Uint8Array;
    root: SignedRoot;
    accountKey: Uint8Array;
    servedPubkey: Uint8Array;
    proof: InclusionProof;
    version: bigint;
  }) => void;
}

/**
 * Verify that `publishedSignaturePubkey` (the AccountIdentity Ed25519
 * key discovery served for `accountId`) is committed by the
 * operator-signed transparency log.
 *
 * @param accountId               the peer's raw 16-byte account_id.
 * @param publishedSignaturePubkey the 32-byte Ed25519 key discovery
 *                                returned for that account.
 * @param deps                    test injection seam (see `VerifyDeps`).
 */
export async function verifyIdentityInLog(
  accountId: Uint8Array,
  publishedSignaturePubkey: Uint8Array,
  deps: VerifyDeps = {},
): Promise<LogVerifyResult> {
  const client =
    deps.client !== undefined
      ? deps.client
      : TransparencyClient.forOptionalConfig();
  const operatorKey =
    deps.operatorKey !== undefined ? deps.operatorKey : pinnedOperatorKey();

  // off — the integration is unconfigured. Skip entirely. No latest
  // entry was read, so `isReset` is null.
  if (client === null || operatorKey === null) {
    return { status: "off", isReset: null };
  }

  try {
    // 1. Fetch the operator-signed root and run the no-rollback gate
    //    against our persisted high-water mark BEFORE trusting it.
    const root = await client.fetchRoot();
    if (checkAndAdvance(root.logSize) === "rollback") {
      // The log shrank — hard, unambiguous attack. (We do NOT verify
      // the root signature first: a shrink is suspicious regardless of
      // who signed it, and the rollback verdict is the strongest.) We
      // short-circuit BEFORE the latest lookup, so no `isReset`.
      return { status: "rollback", isReset: null };
    }

    // 2. Look up the account's latest logged entry.
    const ak = await accountKey(accountId);
    const latest = await client.fetchLatest(ak);
    if (latest === null) {
      // Not yet logged. Benign under eventual consistency OR an
      // off-log equivocation — surfaced, not auto-trusted. No latest
      // entry, so no `isReset`.
      return { status: "absent", isReset: null };
    }

    // From here a latest entry EXISTS — capture its `is_reset` marker
    // (KT-log S4). It rides out on both the `mismatch` and `confirmed`
    // verdicts below; the caller surfaces it to distinguish an
    // owner-initiated reset from an unexplained key change.
    const isReset = latest.isReset;

    // 3. The log's latest pubkey must equal what discovery served.
    //    A divergence is the equivocation signal.
    if (!bytesEqual(latest.identityPubkey, publishedSignaturePubkey)) {
      return { status: "mismatch", isReset };
    }

    // 4. Prove the exact (account_key, pubkey, version) leaf is
    //    included under the operator-signed root. A tampered proof,
    //    forged root, or substituted identity fails here.
    const proof = await client.fetchProof(latest.idx);
    try {
      if (deps.verifyInclusion !== undefined) {
        // Test seam — stubbed inclusion gate (throws to fail).
        deps.verifyInclusion({
          operatorKey,
          root,
          accountKey: ak,
          servedPubkey: publishedSignaturePubkey,
          proof,
          version: latest.version,
        });
      } else {
        const wasm = await loadWasm();
        wasm.verifyLogInclusion(
          operatorKey,
          root.treeRoot,
          root.logSize,
          root.timestampUnix,
          root.signature,
          latest.idx,
          proof.auditPath,
          ak,
          publishedSignaturePubkey,
          latest.version,
        );
      }
    } catch {
      // The inclusion / root-signature gate failed: the served key is
      // not provably committed by the pinned operator → treat as a
      // mismatch (a log/server divergence from what we were told). A
      // latest entry still existed, so the marker rides out.
      return { status: "mismatch", isReset };
    }
    return { status: "confirmed", isReset };
  } catch (err) {
    if (err instanceof TransparencyNetworkError) {
      return { status: "unreachable", isReset: null };
    }
    // A non-404 HTTP fault or a malformed body — the log is effectively
    // unusable for this check. Fail-open to "unreachable" (the caller
    // does not block on it); the supersession/pin gate is the primary
    // trust.
    return { status: "unreachable", isReset: null };
  }
}

/**
 * KT-log S4 — a LIGHTER, best-effort lookup of the `is_reset` marker
 * for a SERVED key, used to ENRICH a fail-closed `SupersessionRefused`
 * (an unproven changed key — the moment the reset/compromise
 * distinction matters most).
 *
 * Unlike `verifyIdentityInLog`, this does NOT run the inclusion proof
 * or the no-rollback gate: it is a context-only enrichment that can
 * never change a trust decision (gate 1 throws regardless). It only
 * answers: "for this account, does the log's latest entry COMMIT TO
 * EXACTLY the served key, and was that entry an owner reset?"
 *
 * Returns:
 *   * `true`  — the log's latest pubkey matches `servedPubkey` AND its
 *     `is_reset` marker is set (owner-initiated fresh-start).
 *   * `false` — the log's latest pubkey matches `servedPubkey` AND its
 *     `is_reset` marker is NOT set (an unexplained change — possible
 *     compromise).
 *   * `null`  — the integration is off, the log is unreachable, the
 *     account is absent from the log, OR the log's latest pubkey does
 *     NOT match the served key (the marker is then about a different
 *     key and tells us nothing about the served one).
 *
 * Best-effort: ANY fault maps to `null`. This helper NEVER throws.
 */
export async function lookupServedKeyReset(
  accountId: Uint8Array,
  servedSignaturePubkey: Uint8Array,
  deps: Pick<VerifyDeps, "client"> = {},
): Promise<boolean | null> {
  const client =
    deps.client !== undefined
      ? deps.client
      : TransparencyClient.forOptionalConfig();
  if (client === null) {
    return null;
  }
  try {
    const ak = await accountKey(accountId);
    const latest = await client.fetchLatest(ak);
    if (latest === null) {
      // Account not yet logged — no marker to read.
      return null;
    }
    // The marker is only meaningful for the served key if the log's
    // latest entry actually commits to it. A divergence means the
    // marker describes some other key — return null (unknown).
    if (!bytesEqual(latest.identityPubkey, servedSignaturePubkey)) {
      return null;
    }
    return latest.isReset;
  } catch {
    // Best-effort enrichment — any fault is "unknown".
    return null;
  }
}

/** Constant-shape byte equality (length + per-byte). Not
 *  constant-TIME — the inputs are public keys, not secrets. */
function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}
