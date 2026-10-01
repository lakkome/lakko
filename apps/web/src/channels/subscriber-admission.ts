// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Letting new subscribers in (ADR-0010, ADR-0059 section 4): for a channel
 * this device can post to, read the subscriber list and seal the current
 * epoch's seed to every key that has not had it yet.
 *
 * One module for both callers, so they share what has been delivered: the
 * app-wide `SubscriberAdmission` loop, which lets people in while the app is
 * open whatever screen is showing, and the Manage screen's subscriber list,
 * which shows the same state and polls faster while it is open. Before this
 * the let-in loop lived in the Manage panel alone, so a person who
 * subscribed from Discover waited until the owner happened to open it.
 *
 * What was delivered is kept in memory per (channel, key, epoch). A reload
 * delivers everyone once more, which the server takes as an upsert; a new
 * epoch delivers again, which the rotation fan-out would also do. Every
 * envelope is sealed on this device under the publisher's seed; the server
 * only stores it (invariant #2).
 */
import { createSignal, type Accessor } from "solid-js";

import { deliverEnvelope, fetchEpoch, listSubscribers } from "./api";
import { sealSubscriberSeedEnvelope, type WasmIdentity } from "./composer";
import { rememberSubscriberCount } from "./ui-cache";
import { channelsActions, channelsState } from "../state/channels-state";

/** How one subscriber's envelope stands on this device. */
export type AdmissionStatus = "ready" | "failed";

interface ChannelAdmission {
  /** The list as last read, `null` before the first read. */
  readonly subscribers: readonly string[] | null;
  /** Key -> status at the epoch it was sealed for. */
  readonly status: ReadonlyMap<string, { readonly epoch: number; readonly status: AdmissionStatus }>;
}

const EMPTY: ChannelAdmission = { subscribers: null, status: new Map() };

const [admissions, setAdmissions] = createSignal<ReadonlyMap<string, ChannelAdmission>>(new Map());

/** Reactive view of one channel's admission state. */
export function channelAdmission(channelIdHex: string): Accessor<ChannelAdmission> {
  return () => admissions().get(channelIdHex) ?? EMPTY;
}

function update(channelIdHex: string, f: (a: ChannelAdmission) => ChannelAdmission): void {
  setAdmissions((m) => new Map(m).set(channelIdHex, f(m.get(channelIdHex) ?? EMPTY)));
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/** The channel's current epoch: the cached value, else the server's, cached.
 *  Never a guessed 0, which after a rotation would file the seed under the
 *  wrong epoch in the subscriber's map. */
async function currentEpoch(channelIdHex: string): Promise<number> {
  const cached = channelsState.epochByChannel().get(channelIdHex);
  if (cached !== undefined) return cached;
  const view = await fetchEpoch(channelIdHex);
  channelsActions.setEpoch(channelIdHex, view.epoch);
  return view.epoch;
}

/** Seal and deliver the current seed to one key, signed with this account's
 *  publisher role (ADR-0059 section 9). Throws on failure after recording
 *  it, so a manual Retry can word the error. */
export async function admitSubscriber(
  channelIdHex: string,
  pubkeyHex: string,
  identity: WasmIdentity,
): Promise<void> {
  const keys = channelsState.keysByChannel().get(channelIdHex);
  if (keys === undefined) throw new Error("This device cannot post to this channel.");
  const epoch = await currentEpoch(channelIdHex);
  try {
    const wire = await sealSubscriberSeedEnvelope(hexToBytes(pubkeyHex), channelIdHex, epoch, keys.seed);
    // ADR-0031: filed under the epoch it was sealed for.
    await deliverEnvelope(channelIdHex, pubkeyHex, wire, identity, undefined, epoch);
    update(channelIdHex, (a) => ({
      ...a,
      status: new Map(a.status).set(pubkeyHex, { epoch, status: "ready" }),
    }));
  } catch (err) {
    update(channelIdHex, (a) => ({
      ...a,
      status: new Map(a.status).set(pubkeyHex, { epoch, status: "failed" }),
    }));
    throw err;
  }
}

const inFlight = new Set<string>();

/**
 * Read the list and let in every key not yet delivered at the current epoch
 * (or whose last delivery failed). One pass per channel at a time; a second
 * caller while one runs returns at once. Sequential, so a burst of new
 * subscribers does not open many wasm calls at once.
 */
export async function admitNewSubscribers(channelIdHex: string, identity: WasmIdentity): Promise<void> {
  if (inFlight.has(channelIdHex)) return;
  if (!channelsState.keysByChannel().has(channelIdHex)) return;
  inFlight.add(channelIdHex);
  try {
    const list = await listSubscribers(channelIdHex, identity);
    rememberSubscriberCount(channelIdHex, list.subscribers.length);
    update(channelIdHex, (a) => ({ ...a, subscribers: list.subscribers }));
    const epoch = await currentEpoch(channelIdHex);
    for (const pk of list.subscribers) {
      const st = (admissions().get(channelIdHex) ?? EMPTY).status.get(pk);
      if (st !== undefined && st.status === "ready" && st.epoch === epoch) continue;
      try {
        await admitSubscriber(channelIdHex, pk, identity);
      } catch (err) {
        // Recorded as failed; the next pass tries again.
        // eslint-disable-next-line no-console
        console.warn("[channels] letting a subscriber in failed", err);
      }
    }
  } finally {
    inFlight.delete(channelIdHex);
  }
}
