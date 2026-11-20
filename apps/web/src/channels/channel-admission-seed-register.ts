// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 open question 4 (closed 2026-11-14) — a private channel's
 * admission seeds as the account's devices agree on them: one `lakko-sync`
 * register per (channel, admission version, seed), carried between the
 * account's devices over the DeviceGroup, persisted with the synced state
 * and so in the backup. TWO SEEDS OF ONE VERSION COEXIST: two owner devices
 * that mint the same version at once each keep theirs, and a ticket of that
 * version is tried with every seed of it.
 *
 * The DeviceGroup provider installs the port once the synced state is loaded
 * (`devices/sync/DeviceGroupSyncContext.tsx`) and bumps the version on every
 * fold refresh. The channel runtime folds the register into each channel's
 * access record and writes every seed it holds that the register lacks
 * ({@link syncAdmissionSeeds}). The seeds are SECRET: never logged.
 */
import { createSignal } from "solid-js";

/** What the provider lends the channel runtime. */
export interface ChannelAdmissionSeedPort {
  readonly accountIdB64: string;
  /** The distinct versions held for a channel, ascending. */
  versions(channelIdHex: string): number[];
  /** Every seed held for `(channel, version)`. */
  seedsFor(channelIdHex: string, version: number): Uint8Array[];
  /** Write one seed (write-once per seed), fold it here and send it to the
   *  account's other devices. `false` when that seed was held already. */
  set(channelIdHex: string, version: number, seed: Uint8Array): Promise<boolean>;
}

let port: ChannelAdmissionSeedPort | null = null;
const [version, setVersion] = createSignal(0);

/** Bumped on every fold refresh. Reactive. */
export function channelAdmissionSeedVersion(): number {
  return version();
}

/** The provider: the fold changed. */
export function channelAdmissionSeedsChanged(): void {
  setVersion((v) => v + 1);
}

/** Lend the port; returns the uninstaller. */
export function installChannelAdmissionSeedRegister(next: ChannelAdmissionSeedPort): () => void {
  port = next;
  channelAdmissionSeedsChanged();
  return () => {
    if (port === next) port = null;
    channelAdmissionSeedsChanged();
  };
}

/** The port, or `null` before the synced state is loaded. */
export function channelAdmissionSeedPort(): ChannelAdmissionSeedPort | null {
  return port;
}

/** One admission seed, as the access record keeps it. */
export interface AdmissionSeedEntry {
  readonly version: number;
  readonly seedB64: string;
}

/**
 * The two-way reconcile of a channel's admission seeds, pure: what the
 * access record should hold (every local seed and every register seed, each
 * once, by seed) and which local seeds the register lacks (to be written and
 * sent). Ordered by version, then as found.
 */
export function reconcileAdmissionSeeds(
  local: readonly AdmissionSeedEntry[],
  register: readonly AdmissionSeedEntry[],
): { readonly merged: AdmissionSeedEntry[]; readonly toWrite: AdmissionSeedEntry[] } {
  const merged: AdmissionSeedEntry[] = [];
  for (const s of [...local, ...register]) {
    if (!merged.some((m) => m.seedB64 === s.seedB64)) merged.push(s);
  }
  merged.sort((a, b) => a.version - b.version);
  const toWrite = local.filter((s) => !register.some((r) => r.seedB64 === s.seedB64));
  return { merged, toWrite };
}

function b64Of(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 1) bin += String.fromCharCode(bytes[i] ?? 0);
  return btoa(bin);
}

function bytesOf(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

/** Every seed the register holds for a channel. Empty without a port. */
export function registerAdmissionSeeds(channelIdHex: string): AdmissionSeedEntry[] {
  const p = port;
  if (p === null) return [];
  const out: AdmissionSeedEntry[] = [];
  try {
    for (const v of p.versions(channelIdHex)) {
      for (const seed of p.seedsFor(channelIdHex, v)) {
        out.push({ version: v, seedB64: b64Of(seed) });
        seed.fill(0);
      }
    }
  } catch {
    return [];
  }
  return out;
}

/**
 * Bring a channel's seeds and the account's register together: write every
 * locally held seed the register lacks (sent to the account's other
 * devices), and return the union the access record should hold. Without a
 * port (the synced state is not loaded yet) the local seeds are returned
 * unchanged and nothing is written. A failed write is retried by the next
 * call.
 */
export async function syncAdmissionSeeds(
  channelIdHex: string,
  local: readonly AdmissionSeedEntry[],
): Promise<AdmissionSeedEntry[]> {
  const p = port;
  if (p === null) return [...local];
  const { merged, toWrite } = reconcileAdmissionSeeds(local, registerAdmissionSeeds(channelIdHex));
  for (const s of toWrite) {
    const seed = bytesOf(s.seedB64);
    try {
      await p.set(channelIdHex, s.version, seed);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("channel admission seed not written to the register; retried", err instanceof Error ? err.name : "");
    } finally {
      seed.fill(0);
    }
  }
  return merged;
}
