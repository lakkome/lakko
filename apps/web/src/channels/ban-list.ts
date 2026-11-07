// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A channel's ban list and allowed reactions, as this device last read them
 * from the channel's posts (ADR-0087 section 13a; UX
 * `group-and-channel-privacy.md` 13.3, `reactions-and-emoji.md` 9).
 *
 * The ban list is a sealed, publisher-signed post carrying the per-channel
 * comment author keys whose comments every reader hides. It hides comments in
 * a public and a private channel alike; it removes nobody (a comment names an
 * author key, not the subscriber key `Remove` needs). It is folded like the
 * metadata record: the newest stamp wins (`banListIsNewer`). The allowed
 * reactions ride the metadata record (v3) and are folded here from the same
 * plaintexts, so the reaction screens read them in one place.
 *
 * The ban lists (and the names the banned authors' comments carried) are kept
 * per account, sealed under the device key (`storage/account-kv.ts`, row
 * `channel-ban-lists`, behind the ADR-0077 store gate; Forget device deletes
 * it with the rest of the account's rows). The allowed reactions are in memory:
 * the metadata record is re-issued after each rotation, so the channel view's
 * decode finds it again.
 */
import { createSignal } from "solid-js";

import { publishChannelPost } from "./channel-access";
import { ALLOW_ALL, type AllowedSetting } from "../chat/reaction-rules";
import { loadWasm, wasmSync, type Wasm } from "../wasm";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";
import {
  decodePersistedBanLists,
  encodePersistedBanLists,
  foldBanList,
  joinAuthorKeys,
  nextBanStamp,
  splitAuthorKeys,
  withBanned,
  withoutBanned,
  type FoldedBanList,
} from "./feedback-rules";

export { foldBanList, joinAuthorKeys, nextBanStamp, splitAuthorKeys, withBanned, withoutBanned, type FoldedBanList };

type Identity = InstanceType<Wasm["Identity"]>;

const [banLists, setBanLists] = createSignal<ReadonlyMap<string, FoldedBanList>>(new Map());
const [allowedByChannel, setAllowedByChannel] = createSignal<
  ReadonlyMap<string, { readonly setting: AllowedSetting; readonly updatedAtMs: bigint }>
>(new Map());
/** Author key hex -> the name its comments carry, as this device read them.
 *  Only for the `Banned` list (a ban list names keys, not people). */
const [authorNames, setAuthorNames] = createSignal<ReadonlyMap<string, string>>(new Map());

/** The account-kv row the ban lists are kept in. */
export const BAN_LISTS_ROW = "channel-ban-lists";

let boundAccount: string | null = null;
let bindGeneration = 0;
let persistTimer: ReturnType<typeof setTimeout> | null = null;

/**
 * The signed-in account (or `null`): drops what another account held and
 * reads this one's kept lists. Idempotent; every screen that shows comments
 * calls it, so no mount order matters.
 */
export function bindBanListAccount(accountIdB64: string | null): void {
  if (accountIdB64 === boundAccount) return;
  boundAccount = accountIdB64;
  bindGeneration += 1;
  const generation = bindGeneration;
  if (persistTimer !== null) clearTimeout(persistTimer);
  persistTimer = null;
  setBanLists(new Map());
  setAllowedByChannel(new Map());
  setAuthorNames(new Map());
  if (accountIdB64 === null) return;
  void loadAccountJson(accountIdB64, BAN_LISTS_ROW)
    .then((raw) => {
      if (generation !== bindGeneration) return;
      const kept = decodePersistedBanLists(raw);
      for (const [channel, list] of kept.lists) storeBanList(channel, list, false);
      if (kept.names.size > 0) {
        setAuthorNames((m) => {
          const next = new Map(kept.names);
          for (const [k, v] of m) next.set(k, v);
          return next;
        });
      }
    })
    .catch(() => undefined);
}

function schedulePersist(): void {
  const account = boundAccount;
  if (account === null || persistTimer !== null) return;
  persistTimer = setTimeout(() => {
    persistTimer = null;
    if (boundAccount !== account) return;
    const value = encodePersistedBanLists(banLists(), authorNames());
    void saveAccountJson(account, BAN_LISTS_ROW, value).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[channels] keeping the ban lists failed", err instanceof Error ? err.name : "unknown");
    });
  }, 200);
}

/** The channel's ban list as last read, or `undefined`. Reactive. */
export function channelBanList(channelIdHex: string): FoldedBanList | undefined {
  return banLists().get(channelIdHex.toLowerCase());
}

/** Whether a comment author is banned in this channel. Reactive. */
export function isBannedAuthor(channelIdHex: string, authorHex: string): boolean {
  return channelBanList(channelIdHex)?.authors.includes(authorHex.toLowerCase()) ?? false;
}

/** The allowed reactions of a channel (from its metadata record); every
 *  reaction until a record says otherwise. Reactive. */
export function channelAllowedReactions(channelIdHex: string): AllowedSetting {
  return allowedByChannel().get(channelIdHex.toLowerCase())?.setting ?? ALLOW_ALL;
}

/** Remember the name a comment author typed, for the `Banned` list. */
export function rememberCommentAuthor(authorHex: string, name: string): void {
  const key = authorHex.toLowerCase();
  if (authorNames().get(key) === name) return;
  setAuthorNames((m) => new Map(m).set(key, name));
  // Kept only for authors on a ban list (see `encodePersistedBanLists`).
  if (banLists().size > 0) schedulePersist();
}

/** The name a comment author typed, when this device read one. Reactive. */
export function commentAuthorName(authorHex: string): string | undefined {
  return authorNames().get(authorHex.toLowerCase());
}

function isNewerFn(): (a: bigint, b: bigint) => boolean {
  try {
    const w = wasmSync();
    return (a, b) => w.banListIsNewer(a, b);
  } catch {
    return (a, b) => a > b;
  }
}

function storeBanList(channelIdHex: string, incoming: FoldedBanList, persist = true): void {
  const key = channelIdHex.toLowerCase();
  const stored = banLists().get(key);
  const next = foldBanList(stored, incoming, isNewerFn());
  if (next === stored) return;
  setBanLists((m) => new Map(m).set(key, next));
  if (persist) schedulePersist();
}

/**
 * Fold one decrypted post plaintext: a ban list is folded into the channel's
 * ban list, a metadata record's allowed reactions into the channel's setting;
 * any other plaintext is ignored. Call it for every post plaintext this device
 * opens, in any order: the newest stamp wins. Never throws.
 */
export function foldBanListFromPlaintext(channelIdHex: string, plaintext: Uint8Array): void {
  let w: Wasm;
  try {
    w = wasmSync();
  } catch {
    return;
  }
  let c: ReturnType<Wasm["classifyChannelPostPlaintext"]>;
  try {
    c = w.classifyChannelPostPlaintext(plaintext);
  } catch {
    return;
  }
  try {
    if (c.kind === "ban-list") {
      const list = c.banList;
      if (list === undefined) return;
      try {
        storeBanList(channelIdHex, { updatedAtMs: list.updatedAtMs, authors: splitAuthorKeys(list.authors) });
      } finally {
        list.free();
      }
      return;
    }
    if (c.kind === "meta") {
      const r = c.reactions;
      const stamp = c.updatedAtMs;
      if (r === undefined || stamp === undefined) return;
      try {
        const setting: AllowedSetting =
          r.mode === "off" ? { mode: "off" } : r.mode === "only" ? { mode: "only", emoji: r.emoji } : ALLOW_ALL;
        const key = channelIdHex.toLowerCase();
        const stored = allowedByChannel().get(key);
        if (stored === undefined || stamp > stored.updatedAtMs) {
          setAllowedByChannel((m) => new Map(m).set(key, { setting, updatedAtMs: stamp }));
        }
      } finally {
        r.free();
      }
    }
  } finally {
    try {
      c.free();
    } catch {
      // Freeing is best-effort.
    }
  }
}

/** The ban-list plaintext to re-issue after a rotation, or `null` when the
 *  channel has none. A re-issue carries the stored stamp verbatim. */
export async function currentBanListPlaintext(channelIdHex: string): Promise<Uint8Array | null> {
  const list = channelBanList(channelIdHex);
  if (list === undefined) return null;
  const w = await loadWasm();
  const value = new w.BanList(list.updatedAtMs, joinAuthorKeys(list.authors));
  try {
    return w.composeBanList(value);
  } finally {
    value.free();
  }
}

async function publishList(identity: Identity, channelIdHex: string, authors: readonly string[]): Promise<void> {
  const w = await loadWasm();
  const stamp = nextBanStamp(channelBanList(channelIdHex)?.updatedAtMs, BigInt(Date.now()));
  const value = new w.BanList(stamp, joinAuthorKeys(authors));
  let plaintext: Uint8Array;
  try {
    plaintext = w.composeBanList(value);
  } finally {
    value.free();
  }
  await publishChannelPost(identity, channelIdHex, plaintext);
  storeBanList(channelIdHex, { updatedAtMs: stamp, authors: [...authors] });
}

/** `Ban from comments` (owner, publishers): the author's comments disappear
 *  for everyone and new ones are not shown. */
export async function banFromComments(identity: Identity, channelIdHex: string, authorHex: string): Promise<void> {
  const w = await loadWasm();
  const current = channelBanList(channelIdHex)?.authors ?? [];
  await publishList(identity, channelIdHex, withBanned(current, authorHex, w.banListMaxKeys()));
}

/** `Unban` in the `Banned` list. */
export async function unbanFromComments(identity: Identity, channelIdHex: string, authorHex: string): Promise<void> {
  const current = channelBanList(channelIdHex)?.authors ?? [];
  await publishList(identity, channelIdHex, withoutBanned(current, authorHex));
}

/** Drop everything held (sign-out, tests). */
export function forgetBanLists(): void {
  boundAccount = null;
  bindGeneration += 1;
  if (persistTimer !== null) clearTimeout(persistTimer);
  persistTimer = null;
  setBanLists(new Map());
  setAllowedByChannel(new Map());
  setAuthorNames(new Map());
}
