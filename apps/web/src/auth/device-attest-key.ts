// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Device-attestation verifying key (G-13, ADR-0015 sprint 5).
 *
 * Peer devices verifying a removal attestation need the 32-byte Ed25519
 * public key the auth service signs attestations with. It is published as an
 * OKP / Ed25519 JWK at `GET /.well-known/device-attest-jwks.json` (the
 * dedicated device-attest key, separate from the call-JWT key).
 *
 * NOTE on trust: in the zero-trust posture this key is ideally PINNED via
 * the signed bootstrap / trust config rather than fetched from the same
 * origin it protects against. This module is the fetch path for discovery /
 * the current single-origin deployment; pinning is a follow-up. The fetched
 * key is only ever handed to `DeviceGroup.processIncomingVerifyingRemovals`,
 * which performs the Ed25519 verification in the audited Rust core.
 */

import { DEFAULT_BASE_URL } from "./client";
import { base64UrlDecodeNoPad } from "./account-id";

const DEVICE_ATTEST_JWKS_PATH = "/.well-known/device-attest-jwks.json";

/** Default in-memory cache lifetime — 24 h, mirroring the call-JWT JWKS. */
const DEFAULT_TTL_MS = 24 * 60 * 60 * 1000;

/** One JWK entry (only the fields we read). */
interface Jwk {
  /** base64url (no pad) of the 32-byte Ed25519 public key. */
  readonly x?: string;
}

interface Jwks {
  readonly keys?: readonly Jwk[];
}

interface CacheEntry {
  readonly key: Uint8Array;
  readonly fetchedAtMs: number;
}

let cache: CacheEntry | null = null;

/**
 * Fetch (and cache) the 32-byte Ed25519 device-attestation verifying key.
 *
 * @param baseUrl auth-service base (defaults to the shared `/api/auth`).
 * @param nowMs   clock for cache-freshness (injectable for tests).
 * @throws if the endpoint is unreachable / non-2xx, the JWKS is empty, or
 *   the key is not 32 bytes.
 */
export async function fetchDeviceAttestKey(
  baseUrl: string = DEFAULT_BASE_URL,
  nowMs: number = Date.now(),
): Promise<Uint8Array> {
  if (cache && nowMs - cache.fetchedAtMs < DEFAULT_TTL_MS) {
    return cache.key;
  }
  const response = await fetch(`${baseUrl}${DEVICE_ATTEST_JWKS_PATH}`);
  if (!response.ok) {
    throw new Error(`device-attest JWKS fetch failed: ${response.status}`);
  }
  const jwks = (await response.json()) as Jwks;
  const x = jwks.keys?.[0]?.x;
  if (x === undefined) {
    throw new Error("device-attest JWKS has no key");
  }
  const key = base64UrlDecodeNoPad(x);
  if (key.length !== 32) {
    throw new Error(`device-attest key is ${key.length} bytes, expected 32`);
  }
  cache = { key, fetchedAtMs: nowMs };
  return key;
}

/** Drop the cached key (e.g. after a key-rotation signal). */
export function clearDeviceAttestKeyCache(): void {
  cache = null;
}
