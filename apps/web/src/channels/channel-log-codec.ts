// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Pure codec for the per-account channel log — the plaintext that
 * `channel-log-store.ts` seals under the device root key.
 *
 * Kept free of IndexedDB and WebCrypto so `vitest` (plain `node`, no
 * DOM) can exercise the rules that could actually be wrong: which
 * entries survive a decode, what a wrong-length secret does, and
 * that encode/decode round-trips.
 *
 * ## What is in the blob, and why
 *
 * Before this store the channel list, the subscriber-side seeds and
 * the creator's publisher keys were in-memory only, so a page reload
 * emptied the channel list and, for a creator, dropped the Ed25519
 * signing seed (posts would sign under a new key). The subscriber
 * X25519 keypairs were persisted, but in PLAINTEXT `localStorage`.
 * All of it now rides one encrypted blob per account:
 *
 *   * `channels` — the local list (ids, local names, the `@handle`
 *     the channel was found under; handle -> id is a one-way OPRF,
 *     so the handle cannot be recovered from the id later).
 *   * `subscriberKeys` — the per-channel Ed25519 keypair a reader
 *     registered with; losing the secret half loses every future
 *     seed envelope.
 *   * `subscribedSeeds` — the recovered `epoch -> seed` map, so a
 *     reader can decrypt posts immediately after a reload rather
 *     than waiting for the next catch-up.
 *   * `publisherSigningSeeds` — the creator's / co-publisher's
 *     per-channel Ed25519 signing seed.
 *   * `channelMeta` — the newest ADR-0051 metadata record per channel
 *     (`name`, `description`, `updated_at_ms`): a subscriber's shared
 *     title, and for a publisher its own copy, which is the source of
 *     the next record it posts (after a rotation, on Save).
 *   * `publisherProviderSnapshotB64` + `publisherLeafKeyB64` — the
 *     MLS provider storage of the ACCOUNT identity the PublisherGroups
 *     live in, and the signature key it was taken under. The channel
 *     content seed is an MLS exporter over that group, so without
 *     this the group (and the seed every subscriber holds) would be
 *     re-minted on the next open. The leaf key is what lets a restore
 *     refuse a snapshot taken under a different account identity —
 *     the same guard `chat/rehydrate.ts` applies to the chat provider.
 *   * `coPublisherGroupIds` — ADR-0078 section 6: for a channel this
 *     account co-publishes, the PublisherGroup id its `/g/` capability
 *     conveyed. The id is derived from the OWNER's seed, so a co-publisher
 *     cannot compute it and opens the group it joined under this id.
 *   * `publisherCommits` — G-50: the durable record of each PublisherGroup
 *     commit whose steps have not all landed (`publisher-commit-outbox.ts`),
 *     written in the same row as the provider snapshot it belongs to.
 *
 * Decoding is lenient PER ENTRY and strict on the envelope: an
 * unknown version yields `null` (caller starts empty), a malformed
 * entry is dropped alone so one bad row does not cost the whole list.
 * A secret of the wrong length is a malformed entry — it is never
 * padded or truncated into a "valid" key.
 */

import type { ChannelHandle } from "./api";
import type { ChannelSubscriberKeys } from "./composer";
import { parsePublisherCommitEntry, type PublisherCommitEntry } from "./publisher-commit-outbox";

/** Payload schema version this client writes. */
export const CHANNEL_LOG_VERSION = 1;

/** 32-char lowercase hex channel id. */
const CHANNEL_ID_HEX_LEN = 32;
/** Every secret / seed / public key here is 32 bytes. */
const KEY_HEX_LEN = 64;

// ----- Wire shapes (JSON) ---------------------------------------------

export interface PersistedChannel {
  readonly id: string;
  readonly localName: string;
  readonly localDescription: string;
  readonly addedAtMs: number;
  readonly localHandle?: string;
}

export interface PersistedSubscriberKey {
  readonly channelId: string;
  /** Hex-encoded 32-byte Ed25519 seed. */
  readonly secretHex: string;
  /** Hex-encoded 32-byte Ed25519 public key. */
  readonly publicHex: string;
}

export interface PersistedSubscribedSeed {
  readonly channelId: string;
  readonly epoch: number;
  /** Hex-encoded 32-byte channel content seed for `epoch`. */
  readonly seedHex: string;
}

export interface PersistedPublisherSigningSeed {
  readonly channelId: string;
  /** Hex-encoded 32-byte Ed25519 signing seed. */
  readonly seedHex: string;
}

/** ADR-0051 §4 — the newest channel metadata record this device holds. */
export interface ChannelSharedMeta {
  readonly name: string;
  readonly description: string;
  /** The record's `updated_at_ms` (a `u64`). */
  readonly updatedAtMs: bigint;
  /** ADR-0059 metadata v2: the topic id; absent = Other (0). */
  readonly topic?: number;
  /** ADR-0059 metadata v2: the ISO 639 language; absent or empty = not set. */
  readonly language?: string;
  /** ADR-0059 metadata v2: the owner listed the channel in Discover. What
   *  lets a member share only listed channels in a group (section 6). */
  readonly listed?: boolean;
}

export interface PersistedChannelMeta {
  readonly channelId: string;
  readonly name: string;
  readonly description: string;
  /** Decimal string — a `u64` does not survive `JSON.parse` as a number. */
  readonly updatedAtMs: string;
  /** ADR-0059 v2 fields; absent in a row written before them. */
  readonly topic?: number;
  readonly language?: string;
  readonly listed?: boolean;
}

/** Wire shape of the encrypted blob, post-decrypt + JSON-parse. */
export interface ChannelLogPayload {
  readonly version: number;
  readonly channels: readonly PersistedChannel[];
  readonly subscriberKeys: readonly PersistedSubscriberKey[];
  readonly subscribedSeeds: readonly PersistedSubscribedSeed[];
  readonly publisherSigningSeeds: readonly PersistedPublisherSigningSeed[];
  /** ADR-0051 — one newest record per channel. Absent in a row written by
   *  a narrower writer (decodes as none). */
  readonly channelMeta?: readonly PersistedChannelMeta[];
  /** Base64 of `Identity.snapshotStorage()` on the account identity the
   *  PublisherGroups were created in. Absent / `null` when this device has
   *  never held a publisher group. */
  readonly publisherProviderSnapshotB64?: string | null;
  /** Base64 of the account identity's signature public key at snapshot
   *  time. A restore under a different key is refused. */
  readonly publisherLeafKeyB64?: string | null;
  /** ADR-0078 section 6 — co-published channel id -> PublisherGroup id
   *  (64-char hex). */
  readonly coPublisherGroupIds?: readonly { readonly channelId: string; readonly groupIdHex: string }[];
  /** G-50 — the PublisherGroup commits whose steps are still owed. */
  readonly publisherCommits?: readonly PublisherCommitEntry[];
}

// ----- Decoded shape ---------------------------------------------------

/** The in-memory view of the log, byte arrays decoded. */
export interface ChannelLogSnapshot {
  readonly channels: readonly ChannelHandle[];
  readonly subscriberKeys: ReadonlyMap<string, ChannelSubscriberKeys>;
  readonly subscribedSeeds: ReadonlyMap<string, ReadonlyMap<number, Uint8Array>>;
  readonly publisherSigningSeeds: ReadonlyMap<string, Uint8Array>;
  /** ADR-0051 — channel id -> newest metadata record. */
  readonly channelMeta: ReadonlyMap<string, ChannelSharedMeta>;
  readonly publisherProviderSnapshot: Uint8Array | null;
  readonly publisherLeafKeyB64: string | null;
  /** ADR-0078 section 6 — co-published channel id -> PublisherGroup id hex. */
  readonly coPublisherGroupIds: ReadonlyMap<string, string>;
  /** G-50 — owed PublisherGroup commits. */
  readonly publisherCommits: readonly PublisherCommitEntry[];
}

/** An empty snapshot — what a fresh account starts from. */
export function emptyChannelLogSnapshot(): ChannelLogSnapshot {
  return {
    channels: [],
    subscriberKeys: new Map(),
    subscribedSeeds: new Map(),
    publisherSigningSeeds: new Map(),
    channelMeta: new Map(),
    publisherProviderSnapshot: null,
    publisherLeafKeyB64: null,
    coPublisherGroupIds: new Map(),
    publisherCommits: [],
  };
}

// ----- Hex / base64 helpers (local so the codec stays dependency-free) --

/** Lowercase hex, no separators. */
export function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** Strict hex decode: `null` unless `hex` is exactly `expectedLen`
 *  characters of `[0-9a-fA-F]`. Never pads, never truncates. */
export function hexToBytesExact(hex: unknown, expectedLen: number): Uint8Array | null {
  if (typeof hex !== "string" || hex.length !== expectedLen) return null;
  if (!/^[0-9a-fA-F]*$/.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += 1) binary += String.fromCharCode(bytes[i] ?? 0);
  return btoa(binary);
}

function base64ToBytes(b64: unknown): Uint8Array | null {
  if (typeof b64 !== "string") return null;
  try {
    const binary = atob(b64);
    const out = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

function isChannelIdHex(v: unknown): v is string {
  return typeof v === "string" && v.length === CHANNEL_ID_HEX_LEN && /^[0-9a-f]+$/.test(v);
}

// ----- Encode ------------------------------------------------------------

/** Serialise the in-memory view into the JSON wire shape. Deterministic
 *  given its input; never mutates it. */
export function encodeChannelLog(snapshot: ChannelLogSnapshot): ChannelLogPayload {
  const channels: PersistedChannel[] = snapshot.channels.map((c) => ({
    id: c.id,
    localName: c.localName,
    localDescription: c.localDescription,
    addedAtMs: c.addedAtMs,
    ...(c.localHandle !== undefined ? { localHandle: c.localHandle } : {}),
  }));
  const subscriberKeys: PersistedSubscriberKey[] = [];
  for (const [channelId, k] of snapshot.subscriberKeys) {
    subscriberKeys.push({
      channelId,
      secretHex: bytesToHex(k.secret),
      publicHex: bytesToHex(k.public),
    });
  }
  const subscribedSeeds: PersistedSubscribedSeed[] = [];
  for (const [channelId, inner] of snapshot.subscribedSeeds) {
    for (const [epoch, seed] of inner) {
      subscribedSeeds.push({ channelId, epoch, seedHex: bytesToHex(seed) });
    }
  }
  const publisherSigningSeeds: PersistedPublisherSigningSeed[] = [];
  for (const [channelId, seed] of snapshot.publisherSigningSeeds) {
    publisherSigningSeeds.push({ channelId, seedHex: bytesToHex(seed) });
  }
  const channelMeta: PersistedChannelMeta[] = [];
  for (const [channelId, m] of snapshot.channelMeta) {
    channelMeta.push({
      channelId,
      name: m.name,
      description: m.description,
      updatedAtMs: m.updatedAtMs.toString(),
      ...(m.topic === undefined ? {} : { topic: m.topic }),
      ...(m.language === undefined ? {} : { language: m.language }),
      ...(m.listed === undefined ? {} : { listed: m.listed }),
    });
  }
  return {
    version: CHANNEL_LOG_VERSION,
    channels,
    subscriberKeys,
    subscribedSeeds,
    publisherSigningSeeds,
    channelMeta,
    publisherProviderSnapshotB64:
      snapshot.publisherProviderSnapshot === null
        ? null
        : bytesToBase64(snapshot.publisherProviderSnapshot),
    publisherLeafKeyB64: snapshot.publisherLeafKeyB64,
    coPublisherGroupIds: [...snapshot.coPublisherGroupIds].map(([channelId, groupIdHex]) => ({
      channelId,
      groupIdHex,
    })),
    publisherCommits: [...snapshot.publisherCommits],
  };
}

// ----- Decode ------------------------------------------------------------

/**
 * Parse a decrypted payload. `null` when the envelope is not a v1 log at
 * all (wrong / missing version, not an object); otherwise every malformed
 * entry is skipped and the rest is returned.
 */
export function decodeChannelLog(payload: unknown): ChannelLogSnapshot | null {
  if (typeof payload !== "object" || payload === null) return null;
  const p = payload as Record<string, unknown>;
  if (p.version !== CHANNEL_LOG_VERSION) return null;

  const channels: ChannelHandle[] = [];
  const seenChannel = new Set<string>();
  for (const raw of asArray(p.channels)) {
    const c = raw as Record<string, unknown>;
    if (typeof c !== "object" || c === null) continue;
    if (!isChannelIdHex(c.id) || seenChannel.has(c.id)) continue;
    if (typeof c.localName !== "string" || typeof c.localDescription !== "string") continue;
    if (typeof c.addedAtMs !== "number" || !Number.isFinite(c.addedAtMs)) continue;
    const handle = c.localHandle;
    if (handle !== undefined && typeof handle !== "string") continue;
    seenChannel.add(c.id);
    channels.push({
      id: c.id,
      localName: c.localName,
      localDescription: c.localDescription,
      addedAtMs: c.addedAtMs,
      ...(typeof handle === "string" && handle.length > 0 ? { localHandle: handle } : {}),
    });
  }

  const subscriberKeys = new Map<string, ChannelSubscriberKeys>();
  for (const raw of asArray(p.subscriberKeys)) {
    const k = raw as Record<string, unknown>;
    if (typeof k !== "object" || k === null) continue;
    if (!isChannelIdHex(k.channelId)) continue;
    const secret = hexToBytesExact(k.secretHex, KEY_HEX_LEN);
    const pub = hexToBytesExact(k.publicHex, KEY_HEX_LEN);
    if (secret === null || pub === null) continue;
    subscriberKeys.set(k.channelId, { secret, public: pub });
  }

  const subscribedSeeds = new Map<string, Map<number, Uint8Array>>();
  for (const raw of asArray(p.subscribedSeeds)) {
    const s = raw as Record<string, unknown>;
    if (typeof s !== "object" || s === null) continue;
    if (!isChannelIdHex(s.channelId)) continue;
    if (typeof s.epoch !== "number" || !Number.isInteger(s.epoch) || s.epoch < 0) continue;
    const seed = hexToBytesExact(s.seedHex, KEY_HEX_LEN);
    if (seed === null) continue;
    const inner = subscribedSeeds.get(s.channelId) ?? new Map<number, Uint8Array>();
    inner.set(s.epoch, seed);
    subscribedSeeds.set(s.channelId, inner);
  }

  const publisherSigningSeeds = new Map<string, Uint8Array>();
  for (const raw of asArray(p.publisherSigningSeeds)) {
    const s = raw as Record<string, unknown>;
    if (typeof s !== "object" || s === null) continue;
    if (!isChannelIdHex(s.channelId)) continue;
    const seed = hexToBytesExact(s.seedHex, KEY_HEX_LEN);
    if (seed === null) continue;
    publisherSigningSeeds.set(s.channelId, seed);
  }

  const channelMeta = new Map<string, ChannelSharedMeta>();
  for (const raw of asArray(p.channelMeta)) {
    const m = raw as Record<string, unknown>;
    if (typeof m !== "object" || m === null) continue;
    if (!isChannelIdHex(m.channelId)) continue;
    if (typeof m.name !== "string" || typeof m.description !== "string") continue;
    if (typeof m.updatedAtMs !== "string" || !/^\d+$/.test(m.updatedAtMs)) continue;
    channelMeta.set(m.channelId, {
      name: m.name,
      description: m.description,
      updatedAtMs: BigInt(m.updatedAtMs),
      ...(typeof m.topic === "number" && Number.isInteger(m.topic) && m.topic >= 0 && m.topic <= 255
        ? { topic: m.topic }
        : {}),
      ...(typeof m.language === "string" && /^([a-z]{2,3})?$/.test(m.language)
        ? { language: m.language }
        : {}),
      ...(typeof m.listed === "boolean" ? { listed: m.listed } : {}),
    });
  }

  const coPublisherGroupIds = new Map<string, string>();
  for (const raw of asArray(p.coPublisherGroupIds)) {
    const c = raw as Record<string, unknown>;
    if (typeof c !== "object" || c === null) continue;
    if (!isChannelIdHex(c.channelId)) continue;
    if (hexToBytesExact(c.groupIdHex, KEY_HEX_LEN) === null) continue;
    coPublisherGroupIds.set(c.channelId, (c.groupIdHex as string).toLowerCase());
  }

  const publisherCommits: PublisherCommitEntry[] = [];
  for (const raw of asArray(p.publisherCommits)) {
    const entry = parsePublisherCommitEntry(raw);
    if (entry !== null && !publisherCommits.some((e) => e.id === entry.id)) publisherCommits.push(entry);
  }

  const snapshotB64 = p.publisherProviderSnapshotB64;
  const leafKey = p.publisherLeafKeyB64;
  const publisherProviderSnapshot =
    snapshotB64 === undefined || snapshotB64 === null ? null : base64ToBytes(snapshotB64);
  const publisherLeafKeyB64 = typeof leafKey === "string" && leafKey.length > 0 ? leafKey : null;

  return {
    channels,
    subscriberKeys,
    subscribedSeeds,
    publisherSigningSeeds,
    channelMeta,
    // A snapshot with no key to check it against is unusable: drop both.
    publisherProviderSnapshot: publisherLeafKeyB64 === null ? null : publisherProviderSnapshot,
    publisherLeafKeyB64: publisherProviderSnapshot === null ? null : publisherLeafKeyB64,
    coPublisherGroupIds,
    publisherCommits,
  };
}

function asArray(v: unknown): readonly unknown[] {
  return Array.isArray(v) ? v : [];
}
