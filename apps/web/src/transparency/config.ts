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

import { base64Decode } from "../auth/account-id";

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
