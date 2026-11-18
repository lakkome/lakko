// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 3 - channels list their members to their members.
 *
 * Every member (subscribers and publishers alike) uploads a member card: its
 * profile (v3) sealed under the channel's CURRENT epoch seed, stored under
 * the subscriber key it registered, and signed by that key
 * (`x-lakko-member-card-sig`). The server keeps ciphertext only. A member
 * lists the cards, opens each with the seed of the epoch it names, and reads
 * the profile.
 *
 * A card may carry the author's `channel` reach grant (when their setting
 * lets this channel's members find them). A reader keeps such a grant ONLY
 * from a card, filed under the author's ACCOUNT key - the key discovery
 * returns for the account id the card claims - and only after the reach
 * check showed the grant is registered on that account's mailbox. A card
 * without the grant withdraws one held from before.
 *
 * The pure decisions live in `member-card-rules.ts`; this module does the
 * wasm, the network and the store writes.
 */
import { base64Decode } from "../auth/account-id";
import { accountIdHexOfBytes } from "../profile/profile-store";
import { DiscoveryClient } from "../discovery/client";
import { DsClient } from "../ds/client";
import { journalHex, type ProofSigner } from "../ds/access";
import { acceptReceivedGrant } from "../reach/grant-rules";
import { verifiedTargetOfGrant } from "../reach/mailbox-write";
import { reachSession, reachSignerForGrant } from "../reach/reach-service";
import {
  fileScopedGrant,
  heldClasses,
  withdrawMissingScopedGrants,
  type ScopedGrantClass,
} from "../reach/received-grants";
import { visibilitySetting } from "../reach/visibility-store";
import { ownCardShareable } from "../profile/card-share";
import { loadWasm, type Wasm } from "../wasm";
import {
  apiError,
  deleteMemberCard,
  listMemberCards,
  putMemberCard,
} from "./api";
import { bytesToHex, channelIdFromHex, type ChannelSubscriberKeys } from "./composer";
import { avatarFromWasm, avatarToWasm, bytesToB64, type AvatarData } from "../profile/avatar-model";
import { OWN_AVATAR_KEY, rememberCardAvatar } from "../profile/avatar-runtime";
import {
  cardContent,
  cardFingerprint,
  cardGrantDecision,
  cardUploadDue,
  memberRowName,
  type UploadedCard,
} from "./member-card-rules";

type WasmReachGrant = InstanceType<Wasm["ReachGrant"]>;

// ---------------------------------------------------------------------------
// Session caches (memory only: a reload re-uploads once and re-checks once)
// ---------------------------------------------------------------------------

/** `${channelIdHex}|${subscriberPkHex}` -> the card this session uploaded. */
const uploaded = new Map<string, UploadedCard>();
/** Per-channel serialisation of the own card's upload. */
const uploadChains = new Map<string, Promise<unknown>>();
/** 32-hex account id -> the account key hex discovery returned (`null` for
 *  an account discovery does not know). */
const accountKeys = new Map<string, string | null>();
/** `${accountKeyHex}|${channelIdHex}|${grantWireB64}` -> reach check result. */
const verifiedGrants = new Map<string, boolean>();

// ---------------------------------------------------------------------------
// The own card
// ---------------------------------------------------------------------------

export interface OwnCardInput {
  readonly channelIdHex: string;
  /** The subscriber key pair this device registered on the channel. */
  readonly keys: ChannelSubscriberKeys;
  /** The channel's current epoch, and its seed. */
  readonly epoch: number;
  readonly seed: Uint8Array;
  readonly displayName: string | null;
  readonly username: string | null;
  readonly updatedAtMs: number;
  /** The 16-byte auth account id, or `null`. */
  readonly accountId: Uint8Array | null;
  /** ADR-0060 section 5 — this account's username certificate, carried with
   *  the username so a member can verify it without a lookup. */
  readonly usernameCertificate?: Uint8Array | null;
  /** ADR-0084 — the saved profile photo, or `null`. */
  readonly avatar?: AvatarData | null;
}

export type OwnCardOutcome =
  /** Uploaded now. */
  | "uploaded"
  /** Already uploaded for this epoch with this content. */
  | "unchanged"
  /** The channel does not know this key: register it and try again. */
  | "not-registered"
  /** The server holds a newer card than this epoch (a later catch-up
   *  uploads again). */
  | "stale"
  | "failed";

function scopeAllows(wasm: Wasm, idBytes: Uint8Array): boolean {
  const setting = visibilitySetting();
  if (setting === null) return false;
  const scope = new wasm.ScopeId(wasm.ScopeKind.Channel, idBytes);
  try {
    return wasm.allowsScope(setting, scope);
  } finally {
    scope.free();
  }
}

/**
 * Upload this device's member card for `input.epoch` when one is due
 * (`cardUploadDue`). Serialised per channel; never throws.
 */
export function ensureOwnMemberCard(input: OwnCardInput): Promise<OwnCardOutcome> {
  const prev = uploadChains.get(input.channelIdHex) ?? Promise.resolve();
  const run = prev.then(
    () => uploadOwnCard(input),
    () => uploadOwnCard(input),
  );
  uploadChains.set(input.channelIdHex, run);
  return run;
}

async function uploadOwnCard(input: OwnCardInput): Promise<OwnCardOutcome> {
  let grant: WasmReachGrant | null = null;
  try {
    const wasm = await loadWasm();
    const idBytes = channelIdFromHex(input.channelIdHex);
    const session = reachSession();
    const content = cardContent({
      displayName: input.displayName,
      username: input.username,
      channelAllowed: scopeAllows(wasm, idBytes),
      reachAvailable: session !== null,
    });
    if (content.includeGrant && session !== null) {
      // ADR-0078 section 9: the account's one `channel` key, the same in
      // every channel; the sync registers it while any channel is allowed.
      session.requestSync();
      grant = session.grant("channel", idBytes);
    }
    const pkHex = bytesToHex(input.keys.public);
    const key = `${input.channelIdHex}|${pkHex}`;
    // ADR-0092 section 7 — a change of the card flag re-uploads the card.
    const cardShareable = ownCardShareable(visibilitySetting());
    const fingerprint = `${cardFingerprint(content, input.updatedAtMs, grant?.version ?? null)}${cardShareable ? "" : "|nocard"}`;
    if (!cardUploadDue(uploaded.get(key), input.epoch, fingerprint)) return "unchanged";

    const profile = wasm.composeProfileEnvelope(
      content.displayName ?? undefined,
      content.username ?? undefined,
      BigInt(input.updatedAtMs),
      input.accountId ?? undefined,
      content.username === null ? undefined : (input.usernameCertificate ?? undefined),
      grant === null ? undefined : [grant],
      input.avatar === undefined || input.avatar === null
        ? undefined
        : avatarToWasm<InstanceType<typeof wasm.ProfileAvatar>>(wasm, input.avatar),
      // ADR-0092 section 7 — off exactly when "who can find me" is Nobody.
      cardShareable,
    );
    // The card is bound to the account (ADR-0060 section 3): without a reach
    // session there is no account seed to sign with, and a reader drops an
    // unbound card.
    if (session === null) {
      profile.fill(0);
      return "failed";
    }
    const card = session.sealMemberCard(input.seed, input.epoch, idBytes, input.keys.public, profile);
    profile.fill(0);
    const kp = new wasm.ChannelSubscriberKeypair(input.keys.secret);
    let signature: string;
    try {
      signature = kp.memberCardRequestSignature(idBytes, card);
    } finally {
      kp.free();
    }
    try {
      await putMemberCard(input.channelIdHex, pkHex, card, signature);
    } catch (err) {
      if (apiError.is(err)) {
        if (err.code === "unknown-subscriber") return "not-registered";
        if (err.code === "member-card-stale") return "stale";
        // eslint-disable-next-line no-console
        console.warn(`[channels] member card upload refused (${err.code})`);
        return "failed";
      }
      throw err;
    }
    uploaded.set(key, { epoch: input.epoch, fingerprint });
    return "uploaded";
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] member card upload failed", err instanceof Error ? err.name : "error");
    return "failed";
  } finally {
    // `composeProfileEnvelope` takes its grants by value: after a compose the
    // handle is already moved, and freeing it again throws.
    try {
      grant?.free();
    } catch {
      /* moved into the profile */
    }
  }
}

/** Forget the card this session uploaded for `(channel, key)`: the server
 *  removed it with the subscription (ADR-0060 section 3), so a later
 *  subscription under the same key uploads afresh. */
export function forgetUploadedCard(channelIdHex: string, keys: ChannelSubscriberKeys): void {
  uploaded.delete(`${channelIdHex}|${bytesToHex(keys.public)}`);
}

/** Remove this device's member card (stop following). Best effort: the
 *  server's 90-day subscriber prune removes it anyway. */
export async function deleteOwnMemberCard(
  channelIdHex: string,
  keys: ChannelSubscriberKeys,
): Promise<void> {
  const pkHex = bytesToHex(keys.public);
  uploaded.delete(`${channelIdHex}|${pkHex}`);
  try {
    const wasm = await loadWasm();
    const kp = new wasm.ChannelSubscriberKeypair(keys.secret);
    let signature: string;
    try {
      signature = kp.memberCardRequestSignature(channelIdFromHex(channelIdHex), new Uint8Array(0));
    } finally {
      kp.free();
    }
    await deleteMemberCard(channelIdHex, pkHex, signature);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] member card delete failed", err instanceof Error ? err.name : "error");
  }
}

// ---------------------------------------------------------------------------
// Reading the member list
// ---------------------------------------------------------------------------

/** One row of a channel's member list. */
export interface ChannelMemberRow {
  /** Stable row key: the author's account key hex, else the subscriber key. */
  readonly key: string;
  readonly name: string;
  /** Shown only when the member lets this channel's members find them (the
   *  card carried a `channel` grant this device verified and keeps). */
  readonly username: string | null;
  readonly isSelf: boolean;
  /** ADR-0084 - the key the member's photo is looked up under (the account
   *  key the card is bound to, standard base64; `self` for the viewer). */
  readonly personKeyB64?: string;
  /** Every subscriber key a card of this member was stored under (one per
   *  device): what `Remove` takes in a private channel (UX 7.3). */
  readonly subscriberPksHex: readonly string[];
  /** The card carries something to call the person by: a display name, or a
   *  username shown here. A card without one is not a named row (UX 15.1). */
  readonly named: boolean;
}

export interface ChannelMembersResult {
  readonly rows: readonly ChannelMemberRow[];
  /** Cards this device could not open (no seed for their epoch, or broken). */
  readonly unreadable: number;
  /** More pages exist than were read. */
  readonly truncated: boolean;
}

export interface LoadMembersInput {
  readonly channelIdHex: string;
  /** The seed of an epoch this device holds, or `undefined`. */
  readonly seedForEpoch: (epoch: number) => Uint8Array | undefined;
  /** This device's own subscriber key hex, when it registered one. */
  readonly ownSubscriberPkHex: string | null;
  /** How many pages of cards to read (default 10). */
  readonly maxPages?: number;
}

async function resolveAccountKeyHex(
  discovery: DiscoveryClient,
  accountId: Uint8Array,
): Promise<string | null> {
  const idHex = accountIdHexOfBytes(accountId);
  if (idHex === null) return null;
  if (accountKeys.has(idHex)) return accountKeys.get(idHex) ?? null;
  try {
    const bundle = await discovery.identityFetch(accountId);
    const hex = bundle === null ? null : journalHex(base64Decode(bundle.signature_pubkey));
    accountKeys.set(idHex, hex);
    return hex;
  } catch {
    // Not cached: a network failure is not "unknown account".
    return null;
  }
}

async function verifyGrant(
  ds: DsClient,
  grant: WasmReachGrant,
  wireB64: string,
  accountKeyHex: string,
  channelIdHex: string,
): Promise<boolean | null> {
  const cacheKey = `${accountKeyHex}|${channelIdHex}|${wireB64}`;
  const cached = verifiedGrants.get(cacheKey);
  if (cached !== undefined) return cached;
  let signer: ProofSigner | null = null;
  try {
    const accountKey = new Uint8Array(accountKeyHex.length / 2);
    for (let i = 0; i < accountKey.length; i++) {
      accountKey[i] = Number.parseInt(accountKeyHex.slice(i * 2, i * 2 + 2), 16);
    }
    // ADR-0078 - the mailbox is the one the grant's binding names, verified
    // against the account key the card's binding verified under. A grant
    // whose binding does not verify is not kept (refused, not "unknown").
    const target = await verifiedTargetOfGrant(grant, [accountKey]);
    if (target === null) {
      verifiedGrants.set(cacheKey, false);
      return false;
    }
    signer = await reachSignerForGrant(grant);
    const ok = await ds.reachCheck(target.address, signer);
    verifiedGrants.set(cacheKey, ok);
    return ok;
  } catch {
    return null;
  } finally {
    try {
      (signer as { free?: () => void } | null)?.free?.();
    } catch {
      /* already freed */
    }
  }
}

/** The scoped classes a card withdrawal keeps: everything but `channel`
 *  (a card speaks only for the channel grant). */
const NOT_CHANNEL: ReadonlySet<ScopedGrantClass> = new Set<ScopedGrantClass>(["group", "member"]);

/**
 * Read a channel's member list: every card this device can open, as rows,
 * filing or withdrawing the `channel` grants the cards carry on the way.
 * Rows are deduplicated per account (a member with several devices holds
 * several subscriber keys). Throws only when the first page cannot be read.
 */
export async function loadChannelMembers(input: LoadMembersInput): Promise<ChannelMembersResult> {
  const wasm = await loadWasm();
  const idBytes = channelIdFromHex(input.channelIdHex);
  const session = reachSession();
  const ownAccountKeyHex = session?.accountKeyHex ?? null;
  const discovery = new DiscoveryClient();
  const ds = new DsClient();
  const maxPages = input.maxPages ?? 10;

  type MutableRow = { -readonly [K in keyof ChannelMemberRow]: ChannelMemberRow[K] } & { subscriberPksHex: string[] };
  const rows: MutableRow[] = [];
  const byKey = new Map<string, MutableRow>();
  let unreadable = 0;
  let after: string | null = null;
  let truncated = false;

  for (let page = 0; page < maxPages; page++) {
    const res = await listMemberCards(input.channelIdHex, after);
    for (const m of res.members) {
      const pkHex = bytesToHex(m.subscriberPk);
      let profile: Wasm["Profile"]["prototype"] | null = null;
      let identityPk: Uint8Array;
      try {
        const epoch = wasm.memberCardEpoch(m.card);
        const seed = input.seedForEpoch(epoch);
        if (seed === undefined) {
          unreadable += 1;
          continue;
        }
        // Opening verifies the card's account binding; a card that does not
        // verify throws and is dropped.
        const opened = wasm.openMemberCard(seed, m.card, idBytes, m.subscriberPk);
        try {
          identityPk = opened.identityPk;
          profile = wasm.decodeProfileEnvelope(opened.profile);
        } finally {
          opened.free();
        }
      } catch {
        unreadable += 1;
        profile?.free();
        continue;
      }
      try {
        const row = await readCard({
          wasm,
          session,
          discovery,
          ds,
          channelIdHex: input.channelIdHex,
          pkHex,
          ownSubscriberPkHex: input.ownSubscriberPkHex,
          ownAccountKeyHex,
          identityPkHex: journalHex(identityPk),
          profile,
        });
        if (row === null) {
          unreadable += 1;
          continue;
        }
        const earlier = byKey.get(row.key);
        if (earlier !== undefined) {
          // Another device of the same member: one row, every key under it.
          if (!earlier.subscriberPksHex.includes(pkHex)) earlier.subscriberPksHex.push(pkHex);
          if (row.named && !earlier.named) {
            earlier.named = true;
            earlier.name = row.name;
            earlier.username = row.username;
          }
          continue;
        }
        // ADR-0084 - the card's profile photo, filed under the account key
        // the card is bound to, so the member row's circle can show it.
        const personKeyB64 = row.isSelf ? OWN_AVATAR_KEY : bytesToB64(identityPk);
        if (!row.isSelf) rememberCardAvatar(personKeyB64, avatarFromWasm(profile.avatar));
        const entry = { ...row, personKeyB64, subscriberPksHex: [pkHex] };
        byKey.set(row.key, entry);
        rows.push(entry);
      } finally {
        profile.free();
      }
    }
    after = res.nextAfter;
    if (after === null) break;
    if (page === maxPages - 1) truncated = true;
  }

  // The viewer first, then everyone else in name order.
  rows.sort((a, b) => {
    if (a.isSelf !== b.isSelf) return a.isSelf ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
  return { rows, unreadable, truncated };
}

async function readCard(args: {
  readonly wasm: Wasm;
  readonly session: ReturnType<typeof reachSession>;
  readonly discovery: DiscoveryClient;
  readonly ds: DsClient;
  readonly channelIdHex: string;
  readonly pkHex: string;
  readonly ownSubscriberPkHex: string | null;
  readonly ownAccountKeyHex: string | null;
  /** The account key the card's binding verified under (hex). */
  readonly identityPkHex: string;
  readonly profile: Wasm["Profile"]["prototype"];
}): Promise<Omit<ChannelMemberRow, "subscriberPksHex"> | null> {
  const { wasm, session, profile } = args;
  const isOwnCard = args.ownSubscriberPkHex !== null && args.pkHex === args.ownSubscriberPkHex;
  // The card is filed under the account key its binding verified under, never
  // under the account id the profile claims. A claimed account id discovery
  // publishes a different key for is a card naming someone else: dropped.
  const accountId = profile.accountId;
  if (!isOwnCard && accountId !== undefined) {
    const published = await resolveAccountKeyHex(args.discovery, accountId);
    if (published !== null && published !== args.identityPkHex) return null;
  }
  const senderHex = isOwnCard ? null : args.identityPkHex;
  const isSelf =
    isOwnCard || (senderHex !== null && args.ownAccountKeyHex !== null && senderHex === args.ownAccountKeyHex);

  const grants = profile.grants;
  let channelGrant: WasmReachGrant | null = null;
  for (const g of grants) {
    if (channelGrant === null && g.class === wasm.ReachClass.Channel) channelGrant = g;
    else g.free();
  }
  let username: string | null = null;
  try {
    if (session !== null && !isSelf) {
      let verified: boolean | null = null;
      let wireB64: string | null = null;
      if (channelGrant !== null && senderHex !== null) {
        wireB64 = session.encodeGrant(channelGrant);
        verified = await verifyGrant(args.ds, channelGrant, wireB64, senderHex, args.channelIdHex);
      }
      const decision = cardGrantDecision({
        isOwnCard,
        senderAccountKeyHex: senderHex,
        ownAccountKeyHex: args.ownAccountKeyHex,
        carriesChannelGrant: channelGrant !== null,
        grantVerified: verified,
      });
      if (
        decision === "file" &&
        senderHex !== null &&
        channelGrant !== null &&
        wireB64 !== null &&
        args.ownAccountKeyHex !== null &&
        acceptReceivedGrant("channel", {
          carrier: { kind: "member-card" },
          senderAccountKeyHex: senderHex,
          ownAccountKeyHex: args.ownAccountKeyHex,
          senderIsMember: true,
        })
      ) {
        const held = {
          wireB64,
          version: channelGrant.version,
          receivedAtMs: Date.now(),
        };
        await session.updateReceivedGrants((g) =>
          fileScopedGrant(g, senderHex, args.channelIdHex, "channel", held),
        );
      } else if (decision === "withdraw" && senderHex !== null) {
        await session.updateReceivedGrants((g) =>
          withdrawMissingScopedGrants(g, senderHex, args.channelIdHex, NOT_CHANNEL),
        );
      }
      // UX 6.2: the username goes with the permission - shown only while the
      // card carries the grant AND this device keeps it for this channel.
      if (
        channelGrant !== null &&
        senderHex !== null &&
        heldClasses(session.receivedGrants(), senderHex, args.channelIdHex).has("channel")
      ) {
        username = profile.username ?? null;
      }
    }
  } finally {
    channelGrant?.free();
  }
  const displayName = profile.displayName?.trim() ?? "";
  return {
    // Every device of the viewer's own account collapses into one "You" row.
    key: isSelf ? "self" : (senderHex ?? `pk:${args.pkHex}`),
    // A card with a username but no display name is called by the username.
    name: !isSelf && displayName.length === 0 && username !== null ? `@${username}` : memberRowName(profile.displayName, isSelf),
    username,
    isSelf,
    named: isSelf || displayName.length > 0 || username !== null,
  };
}
