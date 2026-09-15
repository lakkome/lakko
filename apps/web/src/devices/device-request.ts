// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The camera-less another-device route (ADR-0049 section 2).
 *
 * The pairing QR carries the fresh device's pairing payload to a signed-in
 * device by being scanned. Here the same bytes go through the auth service
 * instead: the fresh device posts them, a signed-in device polls, both show
 * a six-digit comparison code derived from the payload, and on Confirm the
 * signed-in device runs the unchanged add-device flow over the payload it
 * fetched.
 *
 * The comparison code is what makes the relay safe: the server could hand
 * the signed-in device a payload of its own making, but then the code it
 * derives differs from the one on the fresh device's screen, and the user
 * is asked to compare them before confirming. The code is computed on both
 * ends from the bytes each holds; the server never sees it.
 */

const SAS_DOMAIN = "lakko/device-request/sas/v1";

/** Six decimal digits from SHA-256(domain || payload), shown as "47 19 03". */
export async function comparisonCode(pairingPayload: Uint8Array): Promise<string> {
  const domain = new TextEncoder().encode(SAS_DOMAIN);
  const input = new Uint8Array(domain.length + pairingPayload.length);
  input.set(domain, 0);
  input.set(pairingPayload, domain.length);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", input));
  // Six digits from the first four bytes; a million values, uniform enough
  // for a comparison code (the bias from 2^32 mod 10^6 is below 1e-3).
  const n = ((digest[0]! << 24) >>> 0) + (digest[1]! << 16) + (digest[2]! << 8) + digest[3]!;
  const six = (n % 1_000_000).toString().padStart(6, "0");
  return `${six.slice(0, 2)} ${six.slice(2, 4)} ${six.slice(4, 6)}`;
}

/** How often the signed-in device asks for pending requests while a
 *  session is open. */
export const DEVICE_REQUEST_POLL_MS = 8_000;

/** How often the fresh device checks that its request is still open. */
export const DEVICE_REQUEST_STATUS_POLL_MS = 3_000;
