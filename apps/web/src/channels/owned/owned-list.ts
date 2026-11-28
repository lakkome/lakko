// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 - the owned-channels list as the web client holds it:
 * plain data, the codec and the merge of the core (`OwnedChannelsList`,
 * `lakko_channels::owned`), and the client rules the core leaves to it.
 *
 * The core owns the bytes (always 65 532 of them, list v2), the bounds and the merge
 * after a version conflict. The client decides what an entry holds
 * ({@link trimEntry}: the newest four epoch seeds, the eight highest
 * admission seeds), which channels are listed past the cap of 64
 * ({@link idsBeyondCap}: the ones rotated most recently stay) and whether a
 * write is needed at all ({@link sameList}).
 *
 * Every seed, key and root here is secret: nothing in this module logs.
 */
import type { Wasm } from "../../wasm";

/** Most channels listed (`lakko_channels::owned::MAX_OWNED_CHANNELS`). */
export const MAX_OWNED_CHANNELS = 64;
/** Most admission seeds per channel (`MAX_ADMISSION_SEEDS`). */
export const MAX_ADMISSION_SEEDS = 8;
/** Most epoch seeds per channel (`MAX_EPOCH_SEEDS`, the ADR-0031 retention). */
export const MAX_EPOCH_SEEDS = 4;
/** Most passkey locks (`MAX_PASSKEY_LOCKS`). */
export const MAX_PASSKEY_LOCKS = 8;

/** One passkey lock the list names. */
export interface OwnedPasskeyLock {
  /** Lower-case hex of the passkey's 16-byte tag (its tag in the seal
   *  announcement). */
  readonly tagHex: string;
  /** The lock's 32-byte vault root. */
  readonly root: Uint8Array;
}

/** One admission seed of a private channel. */
export interface OwnedAdmissionSeed {
  readonly version: number;
  readonly seed: Uint8Array;
}

/** One epoch seed. `epoch` is the channel's epoch on the channels service. */
export interface OwnedEpochSeed {
  readonly epoch: number;
  readonly seed: Uint8Array;
}

/** One owned channel, as the list carries it. */
export interface OwnedEntry {
  /** Lower-case hex of the 16-byte channel id. */
  readonly idHex: string;
  /** The read key of a public channel, else `null`. */
  readonly readKey: Uint8Array | null;
  readonly admission: readonly OwnedAdmissionSeed[];
  readonly epochs: readonly OwnedEpochSeed[];
  /**
   * The channel's ADR-0087 handle write seed (32 bytes): the key discovery
   * holds the channel's `@name` under, so a retake can sign `rekey` from it
   * to the key the new account's root derives (ADR-0101 section 7 item 4,
   * list v2). `null` / absent: none listed.
   */
  readonly handleSeed?: Uint8Array | null;
}

/** The decoded list. */
export interface OwnedList {
  /** The current password lock's vault root. */
  readonly passwordLockRoot: Uint8Array;
  readonly passkeyLocks: readonly OwnedPasskeyLock[];
  readonly channels: readonly OwnedEntry[];
}

// ----------------------------------------------------------------------------
// Bytes
// ----------------------------------------------------------------------------

/** Lower-case hex. */
export function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** The bytes of an even-length hex string; throws on anything else. */
export function bytesOfHex(hex: string): Uint8Array {
  if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) throw new Error("not hex");
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(2 * i, 2 * i + 2), 16);
  return out;
}

/** Whether two byte strings are equal. */
export function sameBytes(a: Uint8Array | null, b: Uint8Array | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

function compareBytes(a: Uint8Array, b: Uint8Array): number {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i += 1) {
    const d = (a[i] ?? 0) - (b[i] ?? 0);
    if (d !== 0) return d;
  }
  return a.length - b.length;
}

function concat(parts: readonly Uint8Array[], each: number): Uint8Array {
  const out = new Uint8Array(parts.length * each);
  parts.forEach((p, i) => out.set(p, i * each));
  return out;
}

// ----------------------------------------------------------------------------
// The client's rules over one entry and over the list
// ----------------------------------------------------------------------------

/**
 * What an entry holds: 32-byte seeds only; the admission seeds each once,
 * ascending by `(version, seed)`, the eight highest kept; one seed per
 * epoch (the first given wins), ascending, the newest four kept. Pure. The
 * core's encoder refuses anything past these bounds, so every entry goes
 * through here before it is written.
 */
export function trimEntry(entry: OwnedEntry): OwnedEntry {
  const admission: OwnedAdmissionSeed[] = [];
  for (const a of entry.admission) {
    if (a.seed.length !== 32 || !Number.isInteger(a.version) || a.version < 0 || a.version > 0xffff_ffff) continue;
    if (!admission.some((m) => m.version === a.version && sameBytes(m.seed, a.seed))) admission.push(a);
  }
  admission.sort((a, b) => a.version - b.version || compareBytes(a.seed, b.seed));
  const epochs: OwnedEpochSeed[] = [];
  for (const e of entry.epochs) {
    if (e.seed.length !== 32 || !Number.isInteger(e.epoch) || e.epoch < 0 || e.epoch > 0xffff_ffff) continue;
    if (!epochs.some((m) => m.epoch === e.epoch)) epochs.push(e);
  }
  epochs.sort((a, b) => a.epoch - b.epoch);
  const handleSeed = entry.handleSeed ?? null;
  return {
    idHex: entry.idHex.toLowerCase(),
    readKey: entry.readKey !== null && entry.readKey.length === 32 ? entry.readKey : null,
    admission: admission.slice(-MAX_ADMISSION_SEEDS),
    epochs: epochs.slice(-MAX_EPOCH_SEEDS),
    handleSeed: handleSeed !== null && handleSeed.length === 32 ? handleSeed : null,
  };
}

/** Whether two entries say the same thing (both trimmed). Pure. */
export function sameEntry(a: OwnedEntry, b: OwnedEntry): boolean {
  if (a.idHex !== b.idHex || !sameBytes(a.readKey, b.readKey)) return false;
  if (!sameBytes(a.handleSeed ?? null, b.handleSeed ?? null)) return false;
  if (a.admission.length !== b.admission.length || a.epochs.length !== b.epochs.length) return false;
  for (let i = 0; i < a.admission.length; i += 1) {
    const x = a.admission[i];
    const y = b.admission[i];
    if (x === undefined || y === undefined || x.version !== y.version || !sameBytes(x.seed, y.seed)) return false;
  }
  for (let i = 0; i < a.epochs.length; i += 1) {
    const x = a.epochs[i];
    const y = b.epochs[i];
    if (x === undefined || y === undefined || x.epoch !== y.epoch || !sameBytes(x.seed, y.seed)) return false;
  }
  return true;
}

/** Whether two lists say the same thing (no write needed). Pure. */
export function sameList(a: OwnedList, b: OwnedList): boolean {
  if (!sameBytes(a.passwordLockRoot, b.passwordLockRoot)) return false;
  if (a.passkeyLocks.length !== b.passkeyLocks.length || a.channels.length !== b.channels.length) return false;
  for (let i = 0; i < a.passkeyLocks.length; i += 1) {
    const x = a.passkeyLocks[i];
    const y = b.passkeyLocks[i];
    if (x === undefined || y === undefined || x.tagHex !== y.tagHex || !sameBytes(x.root, y.root)) return false;
  }
  for (let i = 0; i < a.channels.length; i += 1) {
    const x = a.channels[i];
    const y = b.channels[i];
    if (x === undefined || y === undefined || !sameEntry(x, y)) return false;
  }
  return true;
}

/**
 * This device's entry written over the one the list holds: the seeds of
 * both (this device's seed on an epoch both name), capped as
 * {@link trimEntry} caps; the read key is THIS DEVICE'S, absent included.
 * The read key says the channel is public, so a channel this device knows
 * went private must lose it: the core's merge keeps a read key from either
 * side, which would list a private channel as public (and a retake would
 * file its seed where the old read key opens it). The handle seed is this
 * device's when it has one (the key the name is held under moves with a
 * retake), else the one listed. Pure.
 */
export function entryOver(mine: OwnedEntry, held: OwnedEntry | undefined): OwnedEntry {
  if (held === undefined) return trimEntry(mine);
  return trimEntry({
    idHex: mine.idHex,
    readKey: mine.readKey,
    admission: [...mine.admission, ...held.admission],
    epochs: [...mine.epochs, ...held.epochs],
    handleSeed: mine.handleSeed ?? held.handleSeed ?? null,
  });
}

/**
 * Whether the list already says everything `entry` says: the channel is
 * listed with the same read key (or the same absence of one) and every seed
 * of the entry that would survive the merge is there. Then writing the
 * entry changes nothing. Pure.
 */
export function listCovers(list: OwnedList, entry: OwnedEntry): boolean {
  const mine = trimEntry(entry);
  const held = list.channels.find((c) => c.idHex === mine.idHex);
  if (held === undefined) return false;
  return sameEntry(entryOver(mine, held), trimEntry(held));
}

/**
 * ADR-0101 section 7 item 4: "an account owning more than 64 channels lists
 * the 64 it rotated most recently". Of `ids`, the ones that do NOT fit: the
 * least recently rotated first to go (`rotatedAtMs` is this device's record
 * of a channel's last rotation; a channel it has none for counts as never
 * rotated), a tie broken by id so every device drops the same ones. Pure.
 */
export function idsBeyondCap(
  ids: readonly string[],
  rotatedAtMs: (idHex: string) => number,
  max: number = MAX_OWNED_CHANNELS,
): string[] {
  const unique = [...new Set(ids.map((id) => id.toLowerCase()))];
  if (unique.length <= max) return [];
  const ranked = unique
    .map((id) => ({ id, at: rotatedAtMs(id) }))
    .sort((a, b) => b.at - a.at || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  return ranked.slice(max).map((r) => r.id);
}

/**
 * Item 6, pure: what this device has to write. `entries` are
 * the entries of the channels it owns that the list does not cover (the
 * ones past the cap of 64 left out), `deletes` the owed deletes the list
 * still carries.
 */
export function listWritePlan(
  list: OwnedList,
  desired: readonly OwnedEntry[],
  owedDeletes: readonly string[],
  rotatedAtMs: (idHex: string) => number,
): { readonly entries: OwnedEntry[]; readonly deletes: string[] } {
  const deleted = new Set(owedDeletes.map((id) => id.toLowerCase()));
  const mine = desired.filter((e) => !deleted.has(e.idHex));
  const beyond = new Set(
    idsBeyondCap(
      [...mine.map((e) => e.idHex), ...list.channels.map((c) => c.idHex).filter((id) => !deleted.has(id))],
      rotatedAtMs,
    ),
  );
  return {
    entries: mine.filter((e) => !beyond.has(e.idHex) && !listCovers(list, e)),
    deletes: [...deleted].filter((id) => list.channels.some((c) => c.idHex === id)),
  };
}

// ----------------------------------------------------------------------------
// Over the core
// ----------------------------------------------------------------------------

type ListHandle = InstanceType<Wasm["OwnedChannelsList"]>;

function toHandle(wasm: Wasm, list: OwnedList): ListHandle {
  const h = new wasm.OwnedChannelsList(list.passwordLockRoot);
  try {
    for (const l of list.passkeyLocks) h.addPasskeyLock(bytesOfHex(l.tagHex), l.root);
    for (const raw of list.channels) {
      const c = trimEntry(raw);
      h.addChannel(
        bytesOfHex(c.idHex),
        c.readKey,
        Uint32Array.from(c.admission.map((a) => a.version)),
        concat(
          c.admission.map((a) => a.seed),
          32,
        ),
        Uint32Array.from(c.epochs.map((e) => e.epoch)),
        concat(
          c.epochs.map((e) => e.seed),
          32,
        ),
        c.handleSeed ?? undefined,
      );
    }
    return h;
  } catch (err) {
    h.free();
    throw err;
  }
}

function fromHandle(h: ListHandle): OwnedList {
  const tags = h.passkeyLockTags;
  const roots = h.passkeyLockRoots;
  const passkeyLocks: OwnedPasskeyLock[] = [];
  for (let i = 0; i * 16 < tags.length; i += 1) {
    passkeyLocks.push({ tagHex: hexOf(tags.slice(i * 16, i * 16 + 16)), root: roots.slice(i * 32, i * 32 + 32) });
  }
  const channels: OwnedEntry[] = [];
  for (let i = 0; i < h.channelCount; i += 1) {
    const versions = h.admissionVersions(i);
    const admissionSeeds = h.admissionSeeds(i);
    const epochIds = h.epochIds(i);
    const epochSeeds = h.epochSeeds(i);
    channels.push({
      idHex: hexOf(h.channelId(i)),
      readKey: h.readKey(i) ?? null,
      admission: [...versions].map((version, j) => ({ version, seed: admissionSeeds.slice(j * 32, j * 32 + 32) })),
      epochs: [...epochIds].map((epoch, j) => ({ epoch, seed: epochSeeds.slice(j * 32, j * 32 + 32) })),
      handleSeed: h.handleSeed(i) ?? null,
    });
  }
  return { passwordLockRoot: h.passwordLockRoot, passkeyLocks, channels };
}

/** An empty list naming `passwordLockRoot`. */
export function emptyOwnedList(passwordLockRoot: Uint8Array): OwnedList {
  return { passwordLockRoot: passwordLockRoot.slice(), passkeyLocks: [], channels: [] };
}

/** Decode an opened list head. Throws the core's `ownedChannels.<code>`. */
export function decodeOwnedList(wasm: Wasm, bytes: Uint8Array): OwnedList {
  const h = wasm.OwnedChannelsList.decode(bytes);
  try {
    return fromHandle(h);
  } finally {
    h.free();
  }
}

/** Encode to the list's one length. Throws the core's refusal (a bound). */
export function encodeOwnedList(wasm: Wasm, list: OwnedList): Uint8Array {
  const h = toHandle(wasm, list);
  try {
    return h.encode();
  } finally {
    h.free();
  }
}

/**
 * The core's merge (`OwnedChannelsList::merge`): `mine` is what this device
 * wants to write, `theirs` what it read, `deletedIdsHex` the channels this
 * device deleted. By channel id; seeds unioned and capped; this device's
 * password lock root; passkey locks unioned by tag.
 */
export function mergeOwnedLists(
  wasm: Wasm,
  mine: OwnedList,
  theirs: OwnedList,
  deletedIdsHex: readonly string[],
): OwnedList {
  const a = toHandle(wasm, mine);
  try {
    const b = toHandle(wasm, theirs);
    try {
      const deleted = concat(
        deletedIdsHex.map((id) => bytesOfHex(id)),
        16,
      );
      const merged = a.merge(b, deleted);
      try {
        return fromHandle(merged);
      } finally {
        merged.free();
      }
    } finally {
      b.free();
    }
  } finally {
    a.free();
  }
}

/**
 * The list after this device's channel changes: `entries` written over
 * `current` ({@link entryOver}: seeds unioned, the newest kept, this
 * device's read key), `deletedIdsHex` dropped, and past 64 channels the
 * least recently rotated dropped ({@link idsBeyondCap}). The lock roots are
 * `current`'s. The core's merge puts the result together (bounds, order).
 * Pure over the core.
 */
export function withChannelChanges(
  wasm: Wasm,
  current: OwnedList,
  entries: readonly OwnedEntry[],
  deletedIdsHex: readonly string[],
  rotatedAtMs: (idHex: string) => number,
): OwnedList {
  const deleted = new Set(deletedIdsHex.map((id) => id.toLowerCase()));
  const held = new Map(current.channels.map((c) => [c.idHex, c]));
  const mine = entries
    .map(trimEntry)
    .filter((e) => !deleted.has(e.idHex))
    .map((e) => entryOver(e, held.get(e.idHex)));
  const written = new Set(mine.map((e) => e.idHex));
  const all = [...written, ...current.channels.map((c) => c.idHex)].filter((id) => !deleted.has(id));
  const beyond = idsBeyondCap(all, rotatedAtMs);
  const dropped = new Set([...deleted, ...beyond]);
  return mergeOwnedLists(
    wasm,
    {
      passwordLockRoot: current.passwordLockRoot,
      passkeyLocks: current.passkeyLocks,
      channels: mine.filter((e) => !dropped.has(e.idHex)),
    },
    // The entries this device writes are already merged above.
    { ...current, channels: current.channels.filter((c) => !written.has(c.idHex)) },
    [...dropped],
  );
}
