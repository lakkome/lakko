// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Per-channel persistence for the non-secret `ChannelRotationState`
 * (`lastRotationMs` + `postsSinceRotation`). Web mirror of iOS's
 * `ChannelRotationStateStore` (UserDefaults) / the Android twin.
 *
 * This is operational metadata, NOT key material — no content-key
 * seed, no signing key — so plain `localStorage` is appropriate
 * (invariant #7 governs key material only; the channel list + the
 * subscriber-key store in `channels-state.ts` already live in
 * `localStorage` on the same basis).
 *
 * `lastRotationMs` is a `bigint` (Unix ms); `JSON.stringify` cannot
 * serialise a `bigint`, so it is stored as a decimal string and
 * re-parsed on load.
 */

import type { Wasm } from "../wasm";

/** wasm-bindgen `ChannelRotationState` — `{ lastRotationMs: bigint,
 *  postsSinceRotation: number }`. Derived from the free-function
 *  return type so it stays in lock-step with the Rust core
 *  (`lakko_channels::rotation_policy::RotationState`). */
export type ChannelRotationState = ReturnType<Wasm["channelRotationStateNew"]>;

/** One key per channel; versioned so a future shape change can
 *  migrate rather than silently mis-parse. */
const KEY_PREFIX = "lakko.channels.rotation-state.v1.";

/** On-disk JSON shape — `lastRotationMs` as a decimal string so a
 *  `bigint` survives `JSON.stringify`. */
interface PersistedRotationState {
  readonly lastRotationMs: string;
  readonly postsSinceRotation: number;
}

/**
 * Load the persisted rotation state for a channel, or `null` when
 * absent / corrupt / storage-unavailable. A corrupt entry is treated
 * as absent (the caller genesis-seeds a fresh one) rather than
 * throwing — the worst case is one extra scheduled check window.
 */
export function loadRotationState(
  channelIdHex: string,
): ChannelRotationState | null {
  if (typeof localStorage === "undefined") return null;
  const raw = localStorage.getItem(KEY_PREFIX + channelIdHex);
  if (raw === null) return null;
  try {
    const p = JSON.parse(raw) as PersistedRotationState;
    const lastRotationMs = BigInt(p.lastRotationMs);
    const postsSinceRotation = Number(p.postsSinceRotation);
    if (
      lastRotationMs < 0n ||
      !Number.isInteger(postsSinceRotation) ||
      postsSinceRotation < 0
    ) {
      return null;
    }
    return { lastRotationMs, postsSinceRotation };
  } catch {
    return null;
  }
}

/**
 * Persist the rotation state for a channel. Best-effort — a storage
 * failure (quota, private-mode) is swallowed; the next tick simply
 * re-derives from the last durable value or the genesis seed.
 */
export function saveRotationState(
  channelIdHex: string,
  state: ChannelRotationState,
): void {
  if (typeof localStorage === "undefined") return;
  const p: PersistedRotationState = {
    lastRotationMs: state.lastRotationMs.toString(),
    postsSinceRotation: state.postsSinceRotation,
  };
  try {
    localStorage.setItem(KEY_PREFIX + channelIdHex, JSON.stringify(p));
  } catch {
    // ignore — see the doc comment above.
  }
}
