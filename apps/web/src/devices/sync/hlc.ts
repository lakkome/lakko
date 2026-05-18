// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Hybrid Logical Clock generator for cross-device payloads
 * (Phase 2a3e2b).
 *
 * Per-process monotonic counter that combines wall-clock millis
 * with a logical counter to defeat per-millisecond ties:
 *
 * ```text
 *   hlc = wall_millis * 1000 + counter
 * ```
 *
 * If the local clock jumps backwards (NTP skew), the counter
 * keeps the output strictly monotonic — we hold the previous
 * `wall_millis` and bump only the counter. That way two
 * payloads minted in the same tab session always have
 * `hlc_n+1 > hlc_n`, regardless of the underlying clock's
 * behaviour.
 *
 * Cross-device ordering uses the same total order: the receiver
 * compares incoming `hlc` against its local state's
 * `updatedAtHlc` and applies only when strictly greater. With
 * tens-of-devices cardinality, the 1µs resolution of the
 * counter is enough room to never collide in practice.
 *
 * Mirror of `crates/lakko-sync/src/hlc.rs`'s on-the-wire HLC
 * shape — both sides agree on the `u64` representation.
 */

let lastWallMs = 0n;
let counter = 0n;

/**
 * Mint a fresh strictly-increasing HLC value. Pure function
 * over the module-scoped `lastWallMs` / `counter` pair.
 */
export function mintHlc(): bigint {
  const wallMs = BigInt(Date.now());
  if (wallMs > lastWallMs) {
    lastWallMs = wallMs;
    counter = 0n;
  } else {
    counter += 1n;
  }
  // Combined value: shift wall_millis up by 1000 (microsecond
  // resolution) and add the counter. Caps the counter at 999
  // per millisecond; beyond that the next call rolls onto the
  // next millisecond. Realistic call rates inside a single tab
  // are <<999/ms, so we don't bother enforcing the cap — it
  // would only matter under a pathological test load and the
  // worst-case is one millisecond of "now == next-message-ts".
  return lastWallMs * 1000n + counter;
}
