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
 *
 * ## One row per (account, channel) (2026-09-30)
 *
 * The rotation schedule is the publishing account's, so the row is
 * `lakko.channels.rotation-state.v2.<accountIdB64>.<channelIdHex>`. The v1
 * row was keyed by the channel alone, so a second account signed in on the
 * same browser read (and on a co-published channel, overwrote) the first
 * account's schedule, and Forget device left it behind. Which account a
 * call means: the trailing `owner` argument, otherwise the signed-in
 * account bound through `bindContactsAccount` (`contacts/local-store.ts`).
 * With no account a read is `null` and a write is a no-op. Log out keeps the
 * rows; Forget device drops the account's
 * ({@link clearRotationStatesForAccount}).
 */

import type { Wasm } from "../wasm";
import { boundContactsAccount } from "../contacts/local-store";

/** wasm-bindgen `ChannelRotationState` — `{ lastRotationMs: bigint,
 *  postsSinceRotation: number }`. Derived from the free-function
 *  return type so it stays in lock-step with the Rust core
 *  (`lakko_channels::rotation_policy::RotationState`). */
export type ChannelRotationState = ReturnType<Wasm["channelRotationStateNew"]>;

/** One key per channel; versioned so a future shape change can
 *  migrate rather than silently mis-parse. */
const KEY_PREFIX = "lakko.channels.rotation-state.v2.";
/** The retired device-wide rows (`<prefix><channelIdHex>`). Never read;
 *  dropped by Forget device. */
const RETIRED_PREFIX = "lakko.channels.rotation-state.v1.";

function accountOf(owner: string | undefined): string | null {
  const account = owner ?? boundContactsAccount();
  return account === null || account.length === 0 ? null : account;
}

function keyFor(account: string, channelIdHex: string): string {
  return `${KEY_PREFIX}${account}.${channelIdHex}`;
}

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
  owner?: string,
): ChannelRotationState | null {
  const account = accountOf(owner);
  if (account === null || typeof localStorage === "undefined") return null;
  const raw = localStorage.getItem(keyFor(account, channelIdHex));
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
  owner?: string,
): void {
  const account = accountOf(owner);
  if (account === null || typeof localStorage === "undefined") return;
  const p: PersistedRotationState = {
    lastRotationMs: state.lastRotationMs.toString(),
    postsSinceRotation: state.postsSinceRotation,
  };
  try {
    localStorage.setItem(keyFor(account, channelIdHex), JSON.stringify(p));
  } catch {
    // ignore — see the doc comment above.
  }
}

/** Forget device: drop every rotation row of `accountIdB64` and the retired
 *  device-wide rows. Another account's rows are left alone. */
export function clearRotationStatesForAccount(accountIdB64: string): void {
  try {
    if (typeof localStorage === "undefined") return;
    const own = `${KEY_PREFIX}${accountIdB64}.`;
    const doomed: string[] = [];
    for (let i = 0; i < localStorage.length; i += 1) {
      const key = localStorage.key(i);
      if (key !== null && (key.startsWith(own) || key.startsWith(RETIRED_PREFIX))) doomed.push(key);
    }
    for (const key of doomed) localStorage.removeItem(key);
  } catch {
    // storage unavailable: nothing to clear
  }
}
