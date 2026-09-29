// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0059 section 6 — the channels group members share inside a group, and
 * the switch that shares this person's own.
 *
 * Three parts, all pure:
 *
 *   * {@link sniffSharedChannels} recognises the envelope in an application
 *     plaintext BEFORE the chat parser (whose lossy-UTF-8 fallback would
 *     render it as a bubble reading the domain string and the channel
 *     names). Three outcomes like the pin sniff: `list`, `malformed`
 *     (dropped), `none` (carry on to the chat parse);
 *   * the row this device keeps ({@link SharedChannelsRow}): the newest list
 *     per member per group, keyed on the member's ACCOUNT key resolved at
 *     receipt (C-68), replaced only by a strictly newer stamp; the groups the
 *     switch is on in; and what this device last sent to each, so a growth
 *     re-send carries the original stamp (the ADR-0044 lesson);
 *   * {@link channelsToShare}: only subscribed channels whose own metadata
 *     says `listed`, with a handle, at most {@link SHARED_CHANNELS_MAX}.
 */

/** Most channels one list carries (`lakko-contacts::shared_channels`). */
export const SHARED_CHANNELS_MAX = 32;

/** One shared channel. */
export interface SharedChannel {
  /** 32-char lowercase hex. */
  readonly channelIdHex: string;
  readonly handle: string;
  readonly name: string;
}

/** One member's list. */
export interface SharedList {
  readonly updatedAtMs: bigint;
  readonly channels: readonly SharedChannel[];
}

// ----- Sniff -----------------------------------------------------------------

export type SharedChannelsSniff =
  | { readonly kind: "none" }
  | { readonly kind: "malformed" }
  | { readonly kind: "list"; readonly value: SharedList };

/** The one wasm export the sniff needs, structurally. */
export interface SharedChannelsDecoder {
  decodeSharedChannelsEnvelope(bytes: Uint8Array): {
    readonly updatedAtMs: bigint;
    readonly channelIds: Uint8Array;
    readonly handles: string[];
    readonly names: string[];
    free?(): void;
  };
}

/** First byte of every `lakko/…` service-envelope domain. */
const SERVICE_DOMAIN_FIRST_BYTE = 0x6c;

function hex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** Classify an application plaintext. Never throws. */
export function sniffSharedChannels(decoder: SharedChannelsDecoder, bytes: Uint8Array): SharedChannelsSniff {
  if (bytes.length === 0 || bytes[0] !== SERVICE_DOMAIN_FIRST_BYTE) return { kind: "none" };
  let decoded: ReturnType<SharedChannelsDecoder["decodeSharedChannelsEnvelope"]>;
  try {
    decoded = decoder.decodeSharedChannelsEnvelope(bytes);
  } catch (err) {
    const code =
      typeof err === "object" && err !== null
        ? (err as { sharedChannelsError?: unknown }).sharedChannelsError
        : undefined;
    return code === undefined || code === "wrongDomain" ? { kind: "none" } : { kind: "malformed" };
  }
  const ids = decoded.channelIds;
  const handles = decoded.handles;
  const names = decoded.names;
  const updatedAtMs = decoded.updatedAtMs;
  try {
    decoded.free?.();
  } catch {
    // Freeing is best-effort.
  }
  if (ids.length !== handles.length * 16 || handles.length !== names.length) return { kind: "malformed" };
  const channels: SharedChannel[] = handles.map((handle, i) => ({
    channelIdHex: hex(ids.slice(i * 16, i * 16 + 16)),
    handle,
    name: names[i] as string,
  }));
  return { kind: "list", value: { updatedAtMs, channels } };
}

// ----- The row ---------------------------------------------------------------

export interface SharedChannelsRow {
  /** groupIdB64 -> member account key (base64) -> newest list. */
  readonly received: ReadonlyMap<string, ReadonlyMap<string, SharedList>>;
  /** groupIdB64 -> the last list this device sent there. */
  readonly sent: ReadonlyMap<string, SharedList>;
  /** Groups the "Share my channels" switch is on in. */
  readonly shareOn: ReadonlySet<string>;
}

export function emptySharedChannelsRow(): SharedChannelsRow {
  return { received: new Map(), sent: new Map(), shareOn: new Set() };
}

/** The verdict on an incoming list. */
export type SharedChannelsVerdict = "accept" | "reject-stale" | "reject-unattributed";

/**
 * File `incoming` from `memberKeyB64` in `groupIdB64` when it is strictly
 * newer than what is held for that member (the core's
 * `sharedChannelsIsNewer`). An unattributed sender (no account key resolved
 * at receipt) is refused: a list speaks only for its sender.
 */
export function applyIncomingList(
  row: SharedChannelsRow,
  groupIdB64: string,
  memberKeyB64: string | undefined,
  incoming: SharedList,
  isNewer: (incoming: bigint, stored: bigint) => boolean,
): { readonly row: SharedChannelsRow; readonly verdict: SharedChannelsVerdict } {
  if (memberKeyB64 === undefined || memberKeyB64 === "") return { row, verdict: "reject-unattributed" };
  const group = row.received.get(groupIdB64);
  const held = group?.get(memberKeyB64);
  if (held !== undefined && !isNewer(incoming.updatedAtMs, held.updatedAtMs)) {
    return { row, verdict: "reject-stale" };
  }
  const nextGroup = new Map(group ?? []);
  nextGroup.set(memberKeyB64, incoming);
  const received = new Map(row.received);
  received.set(groupIdB64, nextGroup);
  return { row: { ...row, received }, verdict: "accept" };
}

/** The group was deleted from this device: forget everything about it. */
export function forgetGroup(row: SharedChannelsRow, groupIdB64: string): SharedChannelsRow {
  const received = new Map(row.received);
  received.delete(groupIdB64);
  const sent = new Map(row.sent);
  sent.delete(groupIdB64);
  const shareOn = new Set(row.shareOn);
  shareOn.delete(groupIdB64);
  return { received, sent, shareOn };
}

/** A stamp strictly after `previous` (a clock that went backwards still
 *  produces a list every member takes as newer). */
export function freshListStamp(nowMs: number, previous: SharedList | undefined): bigint {
  const now = BigInt(Math.max(0, Math.floor(nowMs)));
  if (previous === undefined || now > previous.updatedAtMs) return now;
  return previous.updatedAtMs + 1n;
}

/** Whether two lists name the same channels, handles and names in order. */
export function sameChannels(a: readonly SharedChannel[], b: readonly SharedChannel[]): boolean {
  if (a.length !== b.length) return false;
  return a.every(
    (c, i) => c.channelIdHex === b[i]?.channelIdHex && c.handle === b[i]?.handle && c.name === b[i]?.name,
  );
}

/** What the share decision reads about one channel on this device. */
export interface ShareableChannel {
  readonly channelIdHex: string;
  /** This device follows it as a subscriber. */
  readonly subscribed: boolean;
  /** The channel's own metadata v2 says `listed`. */
  readonly listed: boolean;
  /** The handle it was found under; `undefined` when none. */
  readonly handle: string | undefined;
  /** The name the channel's metadata carries (or the local name). */
  readonly name: string;
}

const HANDLE_RE = /^[a-z0-9_]{3,32}$/;

/**
 * The list to share: subscribed, listed, with a canonical handle and a name,
 * in channel-id order (so an unchanged set is an unchanged list), at most
 * {@link SHARED_CHANNELS_MAX}. A channel reached through a private handle is
 * never listed, so never shared.
 */
export function channelsToShare(channels: readonly ShareableChannel[]): SharedChannel[] {
  return channels
    .filter((c) => c.subscribed && c.listed && c.handle !== undefined && HANDLE_RE.test(c.handle))
    .filter((c) => c.name.trim().length > 0)
    .map((c) => ({ channelIdHex: c.channelIdHex, handle: c.handle as string, name: c.name.trim() }))
    .sort((a, b) => (a.channelIdHex < b.channelIdHex ? -1 : a.channelIdHex > b.channelIdHex ? 1 : 0))
    .slice(0, SHARED_CHANNELS_MAX);
}

/** One channel as the members of the person's groups share it. */
export interface GroupShared {
  readonly channelIdHex: string;
  readonly handle: string;
  readonly name: string;
  /** groupIdB64 -> the member keys sharing it there. */
  readonly byGroup: ReadonlyMap<string, readonly string[]>;
}

/**
 * Every channel shared with this person, across groups. The person's own key
 * is skipped (their own list is not a suggestion to themselves), and so is a
 * group this device no longer holds (`liveGroups`). When members disagree on
 * a channel's handle or name, the most common claim wins, then the smallest.
 */
export function aggregateShared(
  row: SharedChannelsRow,
  ownKeyB64: string | null,
  liveGroups: ReadonlySet<string>,
): GroupShared[] {
  const byChannel = new Map<
    string,
    { claims: Map<string, number>; byGroup: Map<string, string[]> }
  >();
  for (const [groupIdB64, members] of row.received) {
    if (!liveGroups.has(groupIdB64)) continue;
    for (const [memberKey, list] of members) {
      if (memberKey === ownKeyB64) continue;
      for (const c of list.channels) {
        const entry = byChannel.get(c.channelIdHex) ?? { claims: new Map(), byGroup: new Map() };
        const claim = `${c.handle}\n${c.name}`;
        entry.claims.set(claim, (entry.claims.get(claim) ?? 0) + 1);
        const keys = entry.byGroup.get(groupIdB64) ?? [];
        if (!keys.includes(memberKey)) keys.push(memberKey);
        entry.byGroup.set(groupIdB64, keys);
        byChannel.set(c.channelIdHex, entry);
      }
    }
  }
  const out: GroupShared[] = [];
  for (const [channelIdHex, e] of byChannel) {
    const [claim] = [...e.claims].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1))[0] ?? ["\n", 0];
    const [handle, name] = claim.split("\n") as [string, string];
    out.push({ channelIdHex, handle, name, byGroup: e.byGroup });
  }
  return out.sort((a, b) => (a.channelIdHex < b.channelIdHex ? -1 : 1));
}

// ----- Row codec (JSON, sealed by the store) --------------------------------

interface WireList {
  u: string;
  c: Array<[string, string, string]>;
}

function encodeList(l: SharedList): WireList {
  return { u: l.updatedAtMs.toString(), c: l.channels.map((c) => [c.channelIdHex, c.handle, c.name]) };
}

function decodeList(raw: unknown): SharedList | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.u !== "string" || !/^\d{1,20}$/.test(r.u) || !Array.isArray(r.c)) return null;
  const channels: SharedChannel[] = [];
  for (const c of r.c) {
    if (!Array.isArray(c) || c.length !== 3) continue;
    const [id, handle, name] = c as unknown[];
    if (typeof id !== "string" || !/^[0-9a-f]{32}$/.test(id)) continue;
    if (typeof handle !== "string" || typeof name !== "string") continue;
    channels.push({ channelIdHex: id, handle, name });
    if (channels.length >= SHARED_CHANNELS_MAX) break;
  }
  return { updatedAtMs: BigInt(r.u), channels };
}

export function encodeSharedChannelsRow(row: SharedChannelsRow): string {
  const received: Record<string, Record<string, WireList>> = {};
  for (const [g, members] of row.received) {
    const m: Record<string, WireList> = {};
    for (const [k, l] of members) m[k] = encodeList(l);
    received[g] = m;
  }
  const sent: Record<string, WireList> = {};
  for (const [g, l] of row.sent) sent[g] = encodeList(l);
  return JSON.stringify({ version: 1, received, sent, shareOn: [...row.shareOn] });
}

export function decodeSharedChannelsRow(json: string): SharedChannelsRow {
  let raw: unknown;
  try {
    raw = JSON.parse(json);
  } catch {
    return emptySharedChannelsRow();
  }
  if (typeof raw !== "object" || raw === null) return emptySharedChannelsRow();
  const r = raw as Record<string, unknown>;
  if (r.version !== 1) return emptySharedChannelsRow();
  const received = new Map<string, Map<string, SharedList>>();
  if (typeof r.received === "object" && r.received !== null) {
    for (const [g, members] of Object.entries(r.received as Record<string, unknown>)) {
      if (typeof members !== "object" || members === null) continue;
      const m = new Map<string, SharedList>();
      for (const [k, l] of Object.entries(members as Record<string, unknown>)) {
        const list = decodeList(l);
        if (list !== null) m.set(k, list);
      }
      received.set(g, m);
    }
  }
  const sent = new Map<string, SharedList>();
  if (typeof r.sent === "object" && r.sent !== null) {
    for (const [g, l] of Object.entries(r.sent as Record<string, unknown>)) {
      const list = decodeList(l);
      if (list !== null) sent.set(g, list);
    }
  }
  const shareOn = new Set<string>(
    Array.isArray(r.shareOn) ? r.shareOn.filter((g): g is string => typeof g === "string") : [],
  );
  return { received, sent, shareOn };
}
