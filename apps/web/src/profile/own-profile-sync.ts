// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 section 4a — the own display name between the account's own
 * devices, over the ADR-0060 section 7 DeviceGroup `ServiceEnvelope` carrier.
 *
 * Outgoing: the `profile` kind of `reach/service-envelope-outbox.ts`. Its
 * composer returns the own profile envelope composed with the STORED stamp
 * (never a fresh one), the display name (absent = no name), this account's
 * id, no username (the receiver keeps the session's), no username
 * certificate and no grants. An account that never saved a name (stamp 0)
 * sends nothing. `ProfileContext` marks the kind dirty on every save, and the
 * pairing hand-over (`resendServiceEnvelopes`) sends it to a device just
 * added.
 *
 * Incoming: `reach/service-envelope-intake.ts` classifies the envelope
 * against {@link ownProfileHeld} and hands a strictly newer one to
 * {@link receiveOwnProfile}, which stores the display name with the received
 * stamp and tells the listeners (the own-name UI). It is never republished
 * into groups: the device that made the change already did.
 */
import { loadWasm } from "../wasm";
import { markServiceEnvelopeDirty, setServiceEnvelopeComposer } from "../reach/service-envelope-outbox";
import { adoptOwnProfile, loadOwnProfile, type OwnProfile, type OwnProfileStorage } from "./own-profile";

/** The one wasm export the composer needs, structurally. */
export interface OwnProfileComposer {
  composeProfileEnvelope(
    displayName: string | null | undefined,
    username: string | null | undefined,
    updatedAtMs: bigint,
    accountId?: Uint8Array | null,
  ): Uint8Array;
}

/**
 * The envelope the account's other devices get for `own`, or `null` when no
 * name was ever saved. Pure over the core.
 */
export function composeOwnProfileForDevices(
  wasm: OwnProfileComposer,
  own: OwnProfile,
  accountIdBytes: Uint8Array | null,
): Uint8Array | null {
  if (own.updatedAtMs <= 0) return null;
  return wasm.composeProfileEnvelope(
    own.displayName,
    undefined,
    BigInt(own.updatedAtMs),
    accountIdBytes ?? undefined,
  );
}

interface Binding {
  readonly accountId: string;
  readonly accountIdBytes: Uint8Array | null;
  readonly storage: OwnProfileStorage | undefined;
}

let bound: Binding | null = null;
const listeners = new Set<(own: OwnProfile) => void>();

setServiceEnvelopeComposer("profile", async () => {
  const b = bound;
  if (b === null) return null;
  const own = loadOwnProfile(b.accountId, b.storage);
  if (own.updatedAtMs <= 0) return null;
  return composeOwnProfileForDevices(await loadWasm(), own, b.accountIdBytes);
});

/**
 * The signed-in account: what the composer reads and what an incoming
 * envelope is compared with. `storage` is for tests (default `localStorage`).
 */
export function bindOwnProfileSync(
  accountId: string,
  accountIdBytes: Uint8Array | null,
  storage?: OwnProfileStorage,
): void {
  bound = { accountId, accountIdBytes: accountIdBytes?.slice() ?? null, storage };
}

/** Sign-out. */
export function unbindOwnProfileSync(): void {
  bound = null;
}

/** A local save of the own name: send it to the other devices. */
export function ownProfileChanged(): void {
  markServiceEnvelopeDirty("profile");
}

/** What an incoming own profile is compared with, `null` while unbound. */
export function ownProfileHeld(): { readonly stamp: bigint; readonly accountId: Uint8Array | null } | null {
  const b = bound;
  if (b === null) return null;
  return { stamp: BigInt(loadOwnProfile(b.accountId, b.storage).updatedAtMs), accountId: b.accountIdBytes };
}

/** Whether an account is bound (an incoming profile can be adopted). */
export function isOwnProfileBound(): boolean {
  return bound !== null;
}

/**
 * Adopt a display name another device of this account sent, when its stamp
 * is strictly newer than the stored one. Returns whether it was adopted.
 */
export function receiveOwnProfile(displayName: string | null, updatedAtMs: bigint): boolean {
  const b = bound;
  if (b === null) return false;
  const adopted = adoptOwnProfile(b.accountId, displayName, updatedAtMs, b.storage);
  if (adopted === null) return false;
  for (const listener of listeners) {
    try {
      listener(adopted);
    } catch {
      /* a listener's failure is its own */
    }
  }
  return true;
}

/** Called after a name from another device was adopted. Returns the
 *  unsubscribe function. */
export function subscribeOwnProfileAdopted(listener: (own: OwnProfile) => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
