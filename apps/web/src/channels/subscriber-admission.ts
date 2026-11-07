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

import {
  deliverEnvelope,
  fetchEpoch,
  listInviteKeys,
  listSubscriberPage,
  listSubscribers,
  removeSubscriberAsPublisher,
} from "./api";
import { rowsToAdmit, ticketKeyAdmits } from "./channel-access-record";
import { loadWasm } from "../wasm";
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

/** One page of the admission pass (ADR-0087 section 7). */
const ADMISSION_PAGE = 500;

/**
 * Let new subscribers in. Per channel kind:
 *
 * - **private** (this device set or learned the flag, and holds an admission
 *   seed): read rows above `admittedThroughSeq`, open each sealed ticket with
 *   the channel's admission seeds, and admit the row when the invite key it
 *   names is live on the service; a row with no valid ticket is removed. Each
 *   admitted row gets the current seed sealed to its key. The high-water mark
 *   advances durably after each page.
 * - **public**: no per-row work; the readers open the public seed. The rows
 *   of the time it was private stay below the mark and keep their envelopes.
 * - **unknown** (a channel from before phase B on this device): every row is
 *   let in, as before.
 *
 * One pass per channel at a time; a second caller while one runs returns at
 * once. Sequential, so a burst of new subscribers does not open many wasm
 * calls at once.
 */
export async function admitNewSubscribers(channelIdHex: string, identity: WasmIdentity): Promise<void> {
  if (inFlight.has(channelIdHex)) return;
  if (!channelsState.keysByChannel().has(channelIdHex)) return;
  inFlight.add(channelIdHex);
  try {
    const access = channelsActions.accessOf(channelIdHex);
    if (access.visibility === "public") return;
    if (access.visibility === null) {
      // A co-publisher that holds no admission seed cannot check a ticket:
      // the owner's device lets people in. A channel this device owns from
      // before phase B lets every row in, as before.
      if (channelsActions.coPublisherGroupIdOf(channelIdHex) !== undefined) return;
      await admitEveryRow(channelIdHex, identity);
      return;
    }
    if (access.admissionSeeds.length === 0) return;
    await admitTicketedRows(channelIdHex, identity);
  } finally {
    inFlight.delete(channelIdHex);
  }
}

/** The pre-phase-B pass: read the list and let in every key not yet
 *  delivered at the current epoch (or whose last delivery failed). */
async function admitEveryRow(channelIdHex: string, identity: WasmIdentity): Promise<void> {
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
}

/** The invite key a sealed ticket names, opened under any of the channel's
 *  admission seeds, or `null` when none opens it. */
export async function ticketInviteKeyHex(
  channelIdHex: string,
  subscriberHex: string,
  ticket: Uint8Array | null,
): Promise<string | null> {
  if (ticket === null) return null;
  const wasm = await loadWasm();
  const access = channelsActions.accessOf(channelIdHex);
  const version = wasm.channelTicketAdmissionVersion(ticket);
  const seeds = [...access.admissionSeeds].sort((a, b) =>
    a.version === version ? -1 : b.version === version ? 1 : b.version - a.version,
  );
  const channelId = hexToBytes(channelIdHex);
  const subscriber = hexToBytes(subscriberHex);
  for (const s of seeds) {
    const seed = b64ToBytes(s.seedB64);
    try {
      const pk = wasm.openChannelAdmissionTicket(seed, channelId, subscriber, ticket);
      return bytesToHexLower(pk);
    } catch {
      /* another version's seed */
    } finally {
      seed.fill(0);
    }
  }
  return null;
}

async function admitTicketedRows(channelIdHex: string, identity: WasmIdentity): Promise<void> {
  let live: Set<string> | null = null;
  for (;;) {
    const mark = channelsActions.accessOf(channelIdHex).admittedThroughSeq;
    const page = await listSubscriberPage(channelIdHex, identity, mark, ADMISSION_PAGE);
    const rows = rowsToAdmit(page.entries, mark);
    if (rows.length === 0) return;
    live ??= new Set((await listInviteKeys(channelIdHex, identity)).map((k) => k.invitePkHex.toLowerCase()));
    let through = mark;
    for (const row of rows) {
      const invitePk = await ticketInviteKeyHex(channelIdHex, row.subscriberId, row.sealedTicket);
      if (ticketKeyAdmits(invitePk, live)) {
        await admitSubscriber(channelIdHex, row.subscriberId, identity);
      } else {
        // ADR-0087 section 7: a row with no valid ticket is removed.
        await removeSubscriberAsPublisher(channelIdHex, row.subscriberId, identity);
      }
      through = row.seq;
    }
    await channelsActions.updateAccessDurable(channelIdHex, (r) => ({
      ...r,
      admittedThroughSeq: Math.max(r.admittedThroughSeq, through),
    }));
    if (page.entries.length < ADMISSION_PAGE) return;
  }
}

function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

function bytesToHexLower(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}
