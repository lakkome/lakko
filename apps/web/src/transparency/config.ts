// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Build-time configuration for the transparency-log integration
 * (KT-log S3c).
 *
 * Two env vars, both inlined by Vite at build:
 *
 *   * `VITE_LAKKO_TRANSPARENCY_URL` — the transparency service base
 *     URL (e.g. `https://transparency.lakko.me`, no trailing path).
 *     **Unset → the whole log check is OFF.**
 *
 *   * `VITE_LAKKO_TRANSPARENCY_OPERATOR_KEY` — base64 (standard
 *     alphabet) of the operator's 32-byte Ed25519 verifying key. This
 *     is the trust anchor for every signed root and is **pinned
 *     out-of-band at build time**. The client NEVER trusts the
 *     `/transparency/operator-key` endpoint for verification — a
 *     malicious serving tier cannot swap the operator key. **Unset →
 *     the log check is OFF** (the same skip path as a missing URL).
 *
 * The skip path is deliberately fail-OPEN for availability: the
 * transparency log is an equivocation-detection / gossip layer ON TOP
 * of the existing supersession/pin gate (ADR-0016), which remains the
 * primary, fail-closed trust mechanism. A deployment that has not yet
 * provisioned the operator key (or points at no transparency service)
 * simply runs without the extra check rather than refusing to start.
 */

import { base64Decode, base64UrlDecodeNoPad } from "../auth/account-id";

/**
 * The configured transparency-service base URL, or `null` if unset.
 * Trailing slashes are trimmed so callers can append
 * `/transparency/...` cleanly.
 */
export function transparencyBaseUrl(): string | null {
  const env = import.meta.env as Record<string, string | undefined>;
  const raw = env["VITE_LAKKO_TRANSPARENCY_URL"];
  if (typeof raw !== "string") return null;
  const trimmed = raw.trim().replace(/\/+$/, "");
  return trimmed.length > 0 ? trimmed : null;
}

/**
 * The build-time pinned operator verifying key (32 bytes), or `null`
 * when the env var is unset / malformed / wrong-length.
 *
 * Standard-alphabet base64 (the wire shape every binary auth field
 * uses, and what `GET /transparency/operator-key` itself returns) —
 * but here it is read from the build env, never from the network.
 */
export function pinnedOperatorKey(): Uint8Array | null {
  const env = import.meta.env as Record<string, string | undefined>;
  const raw = env["VITE_LAKKO_TRANSPARENCY_OPERATOR_KEY"];
  if (typeof raw !== "string" || raw.trim().length === 0) return null;
  try {
    const key = base64Decode(raw.trim());
    return key.length === 32 ? key : null;
  } catch {
    return null;
  }
}

/**
 * The build-time pinned WITNESS verifying key (32 bytes), or `null` when the
 * env var is unset / malformed (ADR-0020). Standard-alphabet base64, read from
 * the build env (never the network — like the operator key).
 *
 * Retained as the BACKWARD-COMPAT source for `pinnedWitnessKeys()`: a
 * deployment pinning a single witness via the pre-ADR-0039
 * `VITE_LAKKO_TRANSPARENCY_WITNESS_KEY` env var keeps working (the key-set
 * helper folds it into a one-element set at threshold 1). New deployments use
 * the `..._WITNESS_KEYS` set below.
 */
export function pinnedWitnessKey(): Uint8Array | null {
  const env = import.meta.env as Record<string, string | undefined>;
  const raw = env["VITE_LAKKO_TRANSPARENCY_WITNESS_KEY"];
  if (typeof raw !== "string" || raw.trim().length === 0) return null;
  try {
    const key = base64Decode(raw.trim());
    return key.length === 32 ? key : null;
  } catch {
    return null;
  }
}

/**
 * The build-time pinned WITNESS verifying key SET (each 32 bytes) for the
 * K-of-N witness QUORUM gate (ADR-0039, G-30 finding-1). Read from
 * `VITE_LAKKO_TRANSPARENCY_WITNESS_KEYS` — a comma-separated list of base64url
 * (standard-alphabet base64 is also accepted; padding is optional) 32-byte
 * Ed25519 verifying keys.
 *
 * Parsing is lenient in the same spirit as `pinnedWitnessKey` (which mapped a
 * bad key to "unconfigured"): a blank entry is skipped, and an entry that does
 * not decode to exactly 32 bytes is DROPPED. Dropping is fail-SAFE — a smaller
 * pinned-key pool can only make the quorum HARDER to satisfy (fewer distinct
 * keys can contribute), never easier; a set that shrinks below the threshold
 * simply makes the gate refuse every root (fail-closed).
 *
 * Returns `[]` when unset / all-blank / all-malformed → the witness gate is
 * OFF (dormant, the operator-only transition posture, exactly the single-key
 * no-key case).
 *
 * BACKWARD-COMPAT: when `..._WITNESS_KEYS` is unset (or contributes no valid
 * key) but the OLD singular `VITE_LAKKO_TRANSPARENCY_WITNESS_KEY` is set, that
 * one key is returned as a one-element set (pair it with the default
 * `witnessThreshold()` of 1) — so an existing single-witness deployment keeps
 * working unchanged.
 */
export function pinnedWitnessKeys(): Uint8Array[] {
  const env = import.meta.env as Record<string, string | undefined>;
  const raw = env["VITE_LAKKO_TRANSPARENCY_WITNESS_KEYS"];
  if (typeof raw === "string" && raw.trim().length > 0) {
    const keys: Uint8Array[] = [];
    for (const part of raw.split(",")) {
      const entry = part.trim();
      if (entry.length === 0) continue; // skip blanks
      try {
        const key = base64UrlDecodeNoPad(entry);
        if (key.length === 32) keys.push(key); // drop a wrong-length entry
      } catch {
        // drop an undecodable entry (mirrors pinnedWitnessKey's leniency)
      }
    }
    if (keys.length > 0) return keys;
  }
  // Backward-compat: fall back to the OLD singular pin as a 1-element set.
  const single = pinnedWitnessKey();
  return single === null ? [] : [single];
}

/**
 * The K-of-N witness quorum THRESHOLD (ADR-0039) — the minimum number of
 * DISTINCT pinned witnesses that must each have co-signed the served root.
 * Read from `VITE_LAKKO_TRANSPARENCY_WITNESS_THRESHOLD`; defaults to `1`
 * (unset / unparseable / non-positive). Only consulted when
 * `pinnedWitnessKeys()` is non-empty. A threshold LARGER than the pinned-key
 * count makes the quorum unsatisfiable → the gate fails closed (refuses every
 * root), the correct posture for that misconfiguration.
 */
export function witnessThreshold(): number {
  const env = import.meta.env as Record<string, string | undefined>;
  const raw = env["VITE_LAKKO_TRANSPARENCY_WITNESS_THRESHOLD"];
  if (typeof raw !== "string") return 1;
  const n = Number.parseInt(raw.trim(), 10);
  return Number.isInteger(n) && n >= 1 ? n : 1;
}
