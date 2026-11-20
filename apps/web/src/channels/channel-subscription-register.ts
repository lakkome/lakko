// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The account's channel subscriptions as its devices agree on them
 * (2026-11-13, ADR-0050 section 2.9 amendment): one `lakko-sync` register per
 * channel in the account's synced state, carried between its devices over
 * the DeviceGroup like the own channel reactions, persisted with the synced
 * state and so in the backup.
 *
 * The DeviceGroup provider (`devices/sync/DeviceGroupSyncContext.tsx`)
 * installs the port once the synced state is loaded and bumps the version on
 * every refresh of the fold; the channel runtime
 * (`channels/ChannelSubscriptionSync.tsx`) reads and writes through the
 * functions below. Before the port is installed the reads answer "nothing
 * held" and a write is refused.
 *
 * The register carries the account's ONE subscriber key for the channel, its
 * read key and a co-publisher's PublisherGroup id; never epoch seeds,
 * publisher signing seeds or MLS state. Those fields are SECRET: never log a
 * view.
 */
import { createSignal } from "solid-js";

/** What the account is in a channel. */
export type ChannelSubscriptionRoleName = "subscriber" | "owner" | "co-publisher";

/** One register, read out of the fold. */
export interface ChannelSubscriptionView {
  /** 32 lower-case hex characters. */
  readonly channelIdHex: string;
  readonly hlc: bigint;
  /** `false`: the account left the channel (every other field empty). */
  readonly subscribed: boolean;
  readonly role: ChannelSubscriptionRoleName;
  /** 32-byte Ed25519 seed of the account's subscriber key, or empty. */
  readonly subscriberSecret: Uint8Array;
  /** 32-byte read key, or empty. */
  readonly readKey: Uint8Array;
  /** 64 lower-case hex characters for a co-publisher, else empty. */
  readonly publisherGroupIdHex: string;
  /** ADR-0090: a co-publisher's record of the channel owner's 32-byte
   *  account key (the `/g/` capability's issuer the joining device
   *  verified); empty for an Owner or a subscriber. */
  readonly ownerAccountKey: Uint8Array;
  readonly name: string;
  /** The `@name` without the `@`, or empty. */
  readonly handle: string;
}

/** A live value to write. */
export interface ChannelSubscriptionValue {
  readonly role: ChannelSubscriptionRoleName;
  readonly subscriberSecret: Uint8Array;
  readonly readKey: Uint8Array;
  readonly publisherGroupIdHex: string;
  /** ADR-0090: 32 bytes for a co-publisher (required there), else empty. */
  readonly ownerAccountKey: Uint8Array;
  readonly name: string;
  readonly handle: string;
}

/** What the provider lends the channel runtime. */
export interface ChannelSubscriptionPort {
  readonly accountIdB64: string;
  /** Every register held, leaves included. */
  all(): ChannelSubscriptionView[];
  /** The register of one channel, or `undefined`. */
  get(channelIdHex: string): ChannelSubscriptionView | undefined;
  /** Write a live register, fold it here and send it to the account's other
   *  devices. `false` when the identical value is held. Throws `bounds:` for
   *  a field out of bounds (or a new channel past the client limit). */
  set(channelIdHex: string, value: ChannelSubscriptionValue): Promise<boolean>;
  /** Write the leave, fold and send. `false` when nothing live was held. */
  leave(channelIdHex: string): Promise<boolean>;
}

let port: ChannelSubscriptionPort | null = null;
const [version, setVersion] = createSignal(0);

/** Bumped by the provider on every refresh of the fold. Reactive. */
export function channelSubscriptionVersion(): number {
  return version();
}

/** The provider: the fold changed. */
export function channelSubscriptionsChanged(): void {
  setVersion((v) => v + 1);
}

/** The synced state is loaded: lend the port. Returns the uninstaller. */
export function installChannelSubscriptionRegister(next: ChannelSubscriptionPort): () => void {
  port = next;
  channelSubscriptionsChanged();
  return () => {
    if (port === next) port = null;
    channelSubscriptionsChanged();
  };
}

/** The port, or `null` before the synced state is loaded. Not reactive by
 *  itself: read {@link channelSubscriptionVersion} first in a reactive scope. */
export function channelSubscriptionPort(): ChannelSubscriptionPort | null {
  return port;
}

/** The register of one channel. Reactive. */
export function channelSubscriptionOf(channelIdHex: string): ChannelSubscriptionView | undefined {
  void version();
  if (port === null) return undefined;
  try {
    return port.get(channelIdHex);
  } catch {
    return undefined;
  }
}

/** Every live register's channel id. Reactive. */
export function liveChannelSubscriptionIds(): Set<string> {
  void version();
  const out = new Set<string>();
  if (port === null) return out;
  try {
    for (const r of port.all()) if (r.subscribed) out.add(r.channelIdHex);
  } catch {
    /* the fold could not be read: nothing live */
  }
  return out;
}

/** The account's register is not loaded yet on this device. */
export class ChannelSubscriptionsNotReadyError extends Error {
  override name = "ChannelSubscriptionsNotReadyError";
  constructor() {
    super("the channel-subscription register is not loaded");
  }
}

/** Write the leave of a channel for `accountIdB64`: `false` when nothing
 *  live was held. Rejects while the register is not loaded for that account
 *  (the caller, a step of the membership end, is retried): a channel left
 *  here whose register stayed live would come back from it. A failed send is
 *  not an error (the provider sends it again at the next start). */
export async function leaveChannelSubscriptionHere(accountIdB64: string, channelIdHex: string): Promise<boolean> {
  const p = port;
  if (p === null || p.accountIdB64 !== accountIdB64) throw new ChannelSubscriptionsNotReadyError();
  return p.leave(channelIdHex);
}

// ---- intents: what this device just did that the registers must follow ----

/** Why this device writes a register for a channel it does not publish in:
 *  it subscribed (again: a held leave is overruled), or it stopped being a
 *  publisher and stays subscribed (the role goes back to Subscriber). In
 *  this page's memory only; the runtime clears an intent once served. */
const intents = new Map<string, "subscribed" | "demoted">();

/** Record an intent and wake the runtime. */
export function noteChannelIntent(channelIdHex: string, intent: "subscribed" | "demoted"): void {
  intents.set(channelIdHex.toLowerCase(), intent);
  channelSubscriptionsChanged();
}

/** The intents not served yet. */
export function channelIntents(): ReadonlyMap<string, "subscribed" | "demoted"> {
  return new Map(intents);
}

/** The runtime served this intent. */
export function clearChannelIntent(channelIdHex: string): void {
  intents.delete(channelIdHex.toLowerCase());
}

/** The live register's subscriber key seed for a channel, or `null`. Not
 *  reactive. One key per account and channel: a device that subscribes where
 *  the account already is uses this key and never makes one. */
export function liveRegisterSecret(channelIdHex: string): Uint8Array | null {
  if (port === null) return null;
  try {
    const r = port.get(channelIdHex);
    return r !== undefined && r.subscribed && r.subscriberSecret.length === 32 ? r.subscriberSecret : null;
  } catch {
    return null;
  }
}
