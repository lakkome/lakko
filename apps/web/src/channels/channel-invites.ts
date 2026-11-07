// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 sections 7 and 14 — a channel's invitations on web.
 *
 * A link is a `/g/` capability of preview kind 3 (a subscriber invitation),
 * sealed under its link secret and held in the group-invite ledger like every
 * link. A PRIVATE channel's link carries a fresh invite seed: its public half
 * is registered on the channels service with the link's expiry and use limit,
 * and the subscriber proves it at the door and seals an admission ticket with
 * it. A PUBLIC channel's link carries the read key and needs no invite key.
 *
 * `Invite people` sends each contact a one-use link wrapped as an invitation
 * card ({@link mintChannelInviteCard}); the card and its URL travel together
 * in the chat envelope so a client that does not draw the card still shows a
 * link that opens the link page.
 *
 * Records of what this device minted (the invite seeds and the ledger
 * revocation seeds, SECRET) live in the channel's access record, in the
 * encrypted channel log. Links minted on another device or by a co-publisher
 * are not listed here (no sync of these records yet).
 */

import { createSignal } from "solid-js";

import { loadWasm, wasmSync } from "../wasm";
import { recordGroupLink, registeredLinks, revokeRegisteredLink } from "../devices/sync/link-register";
import { hexOf as hexOfBytes } from "../devices/sync/link-records";
import { mergeChannelLinks, type ChannelLinkView } from "./channel-link-merge";
import { DiscoveryClient, DiscoveryHttpError, DiscoveryNetworkError } from "../discovery/client";
import { sessionTokens } from "../auth/session-token-provider";
import { publicOrigin } from "../platform";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { newRevocationKey } from "../contacts/own-links";
import {
  composeSealedInviteLink,
  inviteLinkWidths,
  InviteLinkDamagedError,
  isInviteLinkDamaged,
  isInviteLinkUnreachable,
  openInviteLinkFromUrl,
  parseInviteLinkFragment,
} from "../contacts/invite-link";
import { GROUP_INVITE_URL_PREFIX, splitGroupInviteLink } from "../contacts/group-invite";
import { avatarFromWasm, avatarToWasm, b64ToBytes, bytesToB64, type AvatarData } from "../profile/avatar-model";
import { channelsActions, channelsState } from "../state/channels-state";
import { deleteInviteKey, listInviteKeys, putInviteKey } from "./api";
import { bytesToHex, channelIdFromHex, type WasmIdentity } from "./composer";
import { newestAdmissionSeed, type ChannelInviteLinkRecord } from "./channel-access-record";
import {
  ChannelError,
  channelAvatarOf,
  channelReadKeyHere,
  channelVisibilityOf,
  repairPrivateChannelAccess,
  serviceErrorCode,
  type ChannelAccessKey,
  type ChannelVisibility,
} from "./channel-access";
import type { Wasm } from "../wasm";

/** Service sentinel: one use (the ledger's rule too). */
const ONE_USE = 0;
/** Service sentinel: no limit. */
const UNLIMITED = 0xff_ff_ff_ff;

function discovery(): DiscoveryClient {
  return new DiscoveryClient(undefined, { sessionToken: () => sessionTokens.peek() });
}

// ---------------------------------------------------------------------------
// The links of a channel
// ---------------------------------------------------------------------------

export type ChannelInviteLink = ChannelLinkView;

/** Uses per invite key, as the service last answered (refreshed by
 *  {@link refreshChannelInviteLinks}). */
const [usesByKey, setUsesByKey] = createSignal<ReadonlyMap<string, number>>(new Map());

function ledgerHex(ledgerIdB64Url: string): string {
  try {
    const std = ledgerIdB64Url.replace(/-/g, "+").replace(/_/g, "/");
    return bytesToHex(b64ToBytes(std.padEnd(std.length + ((4 - (std.length % 4)) % 4), "=")));
  } catch {
    return ledgerIdB64Url;
  }
}

function toView(r: ChannelInviteLinkRecord): ChannelInviteLink {
  return {
    linkIdHex: ledgerHex(r.ledgerIdB64Url),
    url: r.url,
    label: r.label,
    expiryMs: r.expiryMs,
    maxUses: r.maxUses,
    uses: r.invitePkHex.length > 0 ? (usesByKey().get(r.invitePkHex) ?? 0) : 0,
    revoked: r.revoked,
    mine: true,
    mintedAtMs: r.mintedAtMs,
  };
}

/** The invite public key (hex) of a private link's invite seed, read
 *  synchronously when the core is loaded; "" otherwise. */
function invitePkHexOf(inviteSeed: Uint8Array): string {
  if (inviteSeed.length !== 32) return "";
  try {
    const signer = wasmSync().ChannelInviteSigner.fromInviteSeed(inviteSeed);
    try {
      return bytesToHex(signer.verifyingKey);
    } finally {
      signer.free();
    }
  } catch {
    return "";
  }
}

/** The register's links of the channel (every device's), as views. Reactive. */
function registeredChannelLinks(idHex: string): ChannelLinkView[] {
  const records = registeredLinks().filter((r) => r.kind === "channel" && hexOfBytes(r.targetId) === idHex.toLowerCase());
  if (records.length === 0) return [];
  let widths;
  try {
    widths = inviteLinkWidths(wasmSync());
  } catch {
    return [];
  }
  const out: ChannelLinkView[] = [];
  for (const r of records) {
    let url: string;
    try {
      url = composeSealedInviteLink({
        origin: publicOrigin(),
        prefix: GROUP_INVITE_URL_PREFIX,
        blobId: r.ledgerId,
        secret: r.linkSecret,
        widths,
      });
    } catch {
      continue;
    }
    const pk = invitePkHexOf(r.inviteSeed);
    out.push({
      linkIdHex: hexOfBytes(r.ledgerId),
      url,
      label: r.label,
      expiryMs: r.expiryMs,
      maxUses: r.maxUses,
      uses: pk.length > 0 ? (usesByKey().get(pk) ?? 0) : r.uses,
      revoked: r.revoked,
      mine: true,
      mintedAtMs: r.mintedAtMs,
    });
  }
  return out;
}

/** Reactive: the links this device minted for the channel (not the one-use
 *  invitation cards, not publisher links), newest first. */
export function channelInviteLinks(idHex: string): readonly ChannelInviteLink[] {
  const a = channelsState.accessByChannel().get(idHex);
  const local = (a?.inviteLinks ?? []).filter((l) => l.kind === "link").map(toView);
  return mergeChannelLinks(local, registeredChannelLinks(idHex));
}

/** Read the use counts of the channel's invite keys (publisher-signed). */
export async function refreshChannelInviteLinks(identity: WasmIdentity, idHex: string): Promise<void> {
  try {
    const keys = await listInviteKeys(idHex, identity);
    setUsesByKey((m) => {
      const next = new Map(m);
      for (const k of keys) next.set(k.invitePkHex.toLowerCase(), k.usedCount);
      return next;
    });
  } catch {
    // The counts stay as last read.
  }
}

/** The channel's name and description as a link preview carries them. */
function previewFacts(idHex: string): { name: string; description: string } {
  const meta = channelsState.metaByChannel().get(idHex);
  const local = channelsState.channels().find((c) => c.id === idHex);
  return {
    name: (meta?.name ?? local?.localName ?? "").trim(),
    description: (meta?.description ?? local?.localDescription ?? "").trim(),
  };
}

/** Mint one link (or one card), upload it to the ledger, record it. */
async function mint(
  identity: WasmIdentity,
  idHex: string,
  opts: { label: string; expiryMs: number; maxUses: number; kind: "link" | "card" },
): Promise<{ record: ChannelInviteLinkRecord; ledgerId: Uint8Array; linkSecret: Uint8Array; sealed: Uint8Array }> {
  if (!channelsState.keysByChannel().has(idHex)) throw new ChannelError("not-publisher");
  const wasm = await loadWasm();
  const visibility = channelVisibilityOf(idHex);
  const channelId = channelIdFromHex(idHex);
  let access: Wasm["ChannelAccess"]["prototype"] | undefined;
  let inviteSeed: Uint8Array | null = null;
  let invitePkHex = "";
  let admissionVersion = 0;
  if (visibility === "public") {
    const readKey = await channelReadKeyHere(identity, idHex);
    // Only the owner derives the read key; a co-publisher learns it from the
    // owner's records.
    if (readKey === null) throw new ChannelError("needs-owner");
    access = wasm.ChannelAccess.publicChannel(readKey);
  } else {
    // A private channel from before phase B has no admission seed yet; the
    // owner's device mints it now (`repairPrivateChannelAccess`).
    const adm = newestAdmissionSeed(channelsActions.accessOf(idHex)) ?? (await repairPrivateChannelAccess(identity, idHex));
    admissionVersion = adm.version;
    inviteSeed = wasm.generateChannelInviteSeed();
    const signer = wasm.ChannelInviteSigner.fromInviteSeed(inviteSeed);
    try {
      invitePkHex = bytesToHex(signer.verifyingKey);
    } finally {
      signer.free();
    }
    const admSeed = b64ToBytes(adm.seedB64);
    try {
      access = wasm.ChannelAccess.privateChannel(inviteSeed, wasm.channelAdmissionPublicKey(admSeed), adm.version);
    } finally {
      admSeed.fill(0);
    }
  }
  const facts = previewFacts(idHex);
  const avatar = channelAvatarOf(idHex);
  const wasmAvatar =
    avatar === null ? undefined : avatarToWasm<Wasm["ProfileAvatar"]["prototype"]>(wasm, avatar);
  let sealed: Uint8Array;
  let linkSecret: Uint8Array;
  try {
    const m = wasm.composeChannelSubscriberInviteCapability(
      identity.ed25519Seed,
      channelId,
      BigInt(Math.max(0, Math.floor(opts.expiryMs))),
      facts.name,
      facts.description,
      access,
      wasmAvatar,
    );
    sealed = m.sealed;
    linkSecret = m.linkSecret;
    m.free();
  } catch (err) {
    throw new ChannelError("failed", { cause: err });
  } finally {
    access.free();
    wasmAvatar?.free();
  }
  // The invite key first: a link whose key the service does not hold admits
  // nobody, so the link is not handed out before it.
  if (invitePkHex.length > 0) {
    try {
      await putInviteKey(idHex, invitePkHex, opts.expiryMs, opts.maxUses, identity);
    } catch (err) {
      throw new ChannelError(serviceErrorCode(err), { cause: err });
    }
  }
  const revocation = await newRevocationKey();
  let ledgerId: Uint8Array;
  try {
    ledgerId = await discovery().uploadGroupInvite({
      sealed,
      revocationPubkey: revocation.publicKey,
      expiryMs: BigInt(Math.max(0, Math.floor(opts.expiryMs))),
      maxUses: opts.maxUses,
    });
  } catch (err) {
    if (invitePkHex.length > 0) await deleteInviteKey(idHex, invitePkHex, identity).catch(() => undefined);
    throw new ChannelError(discoveryErrorCode(err), { cause: err });
  }
  const url = composeSealedInviteLink({
    origin: publicOrigin(),
    prefix: GROUP_INVITE_URL_PREFIX,
    blobId: ledgerId,
    secret: linkSecret,
    widths: inviteLinkWidths(wasm),
  });
  const record: ChannelInviteLinkRecord = {
    ledgerIdB64Url: base64UrlEncodeNoPad(ledgerId),
    url,
    label: opts.label.trim(),
    expiryMs: opts.expiryMs,
    maxUses: opts.maxUses,
    inviteSeedB64: inviteSeed === null ? "" : bytesToB64(inviteSeed),
    invitePkHex,
    revocationSeedB64: bytesToB64(revocation.seed),
    kind: opts.kind,
    revoked: false,
    mintedAtMs: Date.now(),
  };
  await channelsActions.updateAccessDurable(idHex, (r) => ({ ...r, inviteLinks: [...r.inviteLinks, record] }));
  // ADR-0087 phase B — a named link goes to the account's link register so
  // every device lists and revokes it. A one-use invitation card stays this
  // device's (it went to one person in a chat). Best effort: a register that
  // is not running keeps the link listed here only.
  if (opts.kind === "link") {
    await recordGroupLink({
      kind: "channel",
      linkSecret,
      ledgerId,
      revocationSeed: revocation.seed,
      expiryMs: opts.expiryMs,
      maxUses: opts.maxUses,
      mintedAtMs: record.mintedAtMs,
      label: record.label,
      targetId: channelId,
      ...(inviteSeed === null ? {} : { inviteSeed: inviteSeed.slice(), admissionVersion }),
      capability: sealed,
    });
  }
  inviteSeed?.fill(0);
  return { record, ledgerId, linkSecret, sealed };
}

/** A ledger upload failure: the network only when it was the network. */
function discoveryErrorCode(err: unknown): "offline" | "too-many-today" | "failed" {
  if (err instanceof DiscoveryNetworkError || err instanceof TypeError) return "offline";
  if (err instanceof DiscoveryHttpError && err.status === 429) return "too-many-today";
  return "failed";
}

/** Mint an invite link (UX 14.2): `maxUses` 0 = one use, 0xFFFFFFFF = no
 *  limit; `expiryMs` 0 = never. */
export async function mintChannelInviteLink(
  identity: WasmIdentity,
  idHex: string,
  opts: { label: string; expiryMs: number; maxUses: number },
): Promise<ChannelInviteLink> {
  const { record } = await mint(identity, idHex, { ...opts, kind: "link" });
  return toView(record);
}

/** Revoke a link of the channel (UX 14.2): its invite key on the service
 *  goes, then its ledger row; someone already in stays (section 7). A link
 *  made on another device of the account is revoked from its record in the
 *  link register, whose invite seed and revocation seed it carries. */
export async function revokeChannelInviteLink(identity: WasmIdentity, idHex: string, linkIdHex: string): Promise<void> {
  const id = linkIdHex.toLowerCase();
  const record = channelsActions.accessOf(idHex).inviteLinks.find((l) => ledgerHex(l.ledgerIdB64Url) === id);
  const registered = registeredLinks().find(
    (r) => r.kind === "channel" && hexOfBytes(r.targetId) === idHex.toLowerCase() && hexOfBytes(r.ledgerId) === id,
  );
  if (record === undefined && registered === undefined) throw new ChannelError("failed");
  const invitePkHex =
    record !== undefined && record.invitePkHex.length > 0
      ? record.invitePkHex
      : registered !== undefined
        ? invitePkHexOf(registered.inviteSeed)
        : "";
  // The invite key first: once it is gone the link admits nobody, whatever
  // the ledger answers.
  if (invitePkHex.length > 0) {
    try {
      await deleteInviteKey(idHex, invitePkHex, identity);
    } catch (err) {
      throw new ChannelError(serviceErrorCode(err), { cause: err });
    }
  }
  try {
    if (registered !== undefined && !registered.revoked) {
      // The register revokes it on every device and this device deletes the
      // row with the record's seed (owed when the ledger is unreachable).
      await revokeRegisteredLink(registered);
    } else if (record !== undefined) {
      const std = record.ledgerIdB64Url.replace(/-/g, "+").replace(/_/g, "/");
      const blobId = b64ToBytes(std.padEnd(std.length + ((4 - (std.length % 4)) % 4), "="));
      const seed = b64ToBytes(record.revocationSeedB64);
      try {
        await discovery().revokeGroupInvite(blobId, seed);
      } finally {
        seed.fill(0);
      }
    }
  } catch (err) {
    throw new ChannelError(discoveryErrorCode(err), { cause: err });
  }
  if (record !== undefined) {
    await channelsActions.updateAccessDurable(idHex, (r) => ({
      ...r,
      inviteLinks: r.inviteLinks.map((l) => (l.ledgerIdB64Url === record.ledgerIdB64Url ? { ...l, revoked: true } : l)),
    }));
  }
}

/**
 * One invitation for one contact (UX 14.1): a one-use link (private) or a
 * link carrying the read key (public), as the card bytes and the URL of the
 * same link. Send as `composeChannelInviteEnvelope(url, card)`.
 */
export async function mintChannelInviteCard(
  identity: WasmIdentity,
  idHex: string,
): Promise<{ card: Uint8Array; url: string }> {
  const minted = await mint(identity, idHex, {
    label: "",
    expiryMs: 0,
    maxUses: channelVisibilityOf(idHex) === "public" ? UNLIMITED : ONE_USE,
    kind: "card",
  });
  const wasm = await loadWasm();
  return {
    card: wasm.composeChannelInviteCard(minted.ledgerId, minted.linkSecret, minted.sealed),
    url: minted.record.url,
  };
}

// ---------------------------------------------------------------------------
// Opening a card or a link
// ---------------------------------------------------------------------------

/** What a `/g/` link (kind 3 or kind 2) or an invitation card opens to. */
export interface OpenedChannelInvite {
  readonly channelIdHex: string;
  readonly name: string;
  readonly description: string;
  readonly visibility: ChannelVisibility;
  /** For a publisher invitation (kind 2) there is no reading key: the person
   *  reads as a member of the publisher group. A placeholder public key of
   *  length 0 is carried; never subscribe with it. */
  readonly access: ChannelAccessKey;
  readonly avatar: AvatarData | null;
  /** Kind 2: invites to be a publisher. */
  readonly publisherInvite: boolean;
  /** Kind 2: the inviting owner's display name, when the link carries it. */
  readonly inviterName?: string;
  /** The link this came from (a publisher invitation is accepted through it). */
  readonly link?: string;
}

/** The inviter field of a publisher invitation; empty names are left out. */
function inviterOf(name: string | undefined): { inviterName?: string } {
  const n = (name ?? "").trim();
  return n.length > 0 ? { inviterName: n } : {};
}

function fromSubscriberPreview(
  channelIdHex: string,
  preview: Wasm["GroupInvitePreview"]["prototype"],
  link: string,
): OpenedChannelInvite {
  const sub = preview.subscriber;
  if (sub === undefined) throw new ChannelError("link-invalid");
  try {
    const a = sub.access;
    try {
      let access: ChannelAccessKey;
      if (a.isPrivate) {
        const seed = a.inviteSeed;
        const pk = a.admissionPublicKey;
        const ver = a.admissionVersion;
        if (seed === undefined || pk === undefined || ver === undefined) throw new ChannelError("link-damaged");
        access = { kind: "private", inviteSeed: seed, admissionPublicKey: pk, admissionVersion: ver };
      } else {
        const rk = a.readKey;
        if (rk === undefined) throw new ChannelError("link-damaged");
        access = { kind: "public", readKey: rk };
      }
      return {
        channelIdHex,
        name: preview.groupName ?? "",
        description: sub.description,
        visibility: a.isPrivate ? "private" : "public",
        access,
        avatar: avatarFromWasm(preview.avatar),
        publisherInvite: false,
        link,
      };
    } finally {
      a.free();
    }
  } finally {
    sub.free();
  }
}

/** The URL of a received card's link (MAIN opens a card through it). */
export async function channelInviteLinkFromCard(card: Uint8Array): Promise<string> {
  const wasm = await loadWasm();
  let opened: Wasm["OpenedChannelInviteCard"]["prototype"];
  try {
    opened = wasm.openChannelInviteCard(card);
  } catch {
    throw new ChannelError("link-damaged");
  }
  try {
    return composeSealedInviteLink({
      origin: publicOrigin(),
      prefix: GROUP_INVITE_URL_PREFIX,
      blobId: opened.ledgerId,
      secret: opened.linkSecret,
      widths: inviteLinkWidths(wasm),
    });
  } finally {
    opened.free();
  }
}

/** Open an invitation card. A card whose ledger row is gone (revoked,
 *  expired, used up) is "link-invalid". */
export async function openChannelInviteCardBytes(card: Uint8Array): Promise<OpenedChannelInvite> {
  const opened = await openChannelLink(await channelInviteLinkFromCard(card));
  if (opened === null) throw new ChannelError("link-invalid");
  return opened;
}

/**
 * Open a `/g/` link: a channel link of kind 3 (subscriber invitation) or kind
 * 2 (publisher invitation) opens to an {@link OpenedChannelInvite}; a link
 * that opens fine but targets a group resolves `null` (the caller takes the
 * group flow). ChannelError "link-invalid" (no such row, revoked, expired,
 * not a `/g/` link), "link-damaged", "offline" for a channel link that is bad
 * or a link that cannot be fetched. `url` is the full href with its
 * `#secret` fragment.
 */
export async function openChannelLink(url: string): Promise<OpenedChannelInvite | null> {
  const wasm = await loadWasm();
  const widths = inviteLinkWidths(wasm);
  const split = splitGroupInviteLink(url, widths);
  if (split === null) throw new ChannelError("link-invalid");
  const at = url.indexOf("#");
  const hash = at >= 0 ? url.slice(at) : null;
  const fragment = parseInviteLinkFragment(hash, widths);
  let opened;
  try {
    opened = await openInviteLinkFromUrl({
      slug: split.slug,
      hash,
      widths,
      what: "group-invite",
      fetchPointer: (blobId) => discovery().fetchGroupInvite(blobId),
      open: (sealed, secret) => wasm.GroupInviteCapability.openLink(sealed, secret),
    });
  } catch (err) {
    if (err instanceof InviteLinkDamagedError || isInviteLinkDamaged(err)) throw new ChannelError("link-damaged");
    if (err instanceof ChannelError) throw err;
    // The ledger not answering is "offline"; a malformed slug is a bad link.
    if (isInviteLinkUnreachable(err)) throw new ChannelError("offline", { cause: err });
    throw new ChannelError("link-invalid", { cause: err });
  }
  if (opened.kind === "not-found") throw new ChannelError("link-invalid");
  const cap = opened.capability;
  try {
    if (fragment.kind !== "secret") throw new ChannelError("link-damaged");
    let preview: Wasm["GroupInvitePreview"]["prototype"] | undefined;
    try {
      preview = cap.openPreview(fragment.secret);
    } catch {
      // A preview that does not open: the group flow says what it says.
      return null;
    }
    if (preview === undefined) return null;
    try {
      const channelId = preview.channelId;
      if (channelId === undefined) return null;
      if (!cap.isValidNow(BigInt(Date.now()))) throw new ChannelError("link-invalid");
      const sub = preview.subscriber;
      const isSubscriberInvite = sub !== undefined;
      sub?.free();
      if (isSubscriberInvite) return fromSubscriberPreview(bytesToHex(channelId), preview, url);
      if (cap.knockPublicKey !== undefined) {
        // Kind 2: a publisher invitation.
        return {
          channelIdHex: bytesToHex(channelId),
          name: preview.groupName ?? "",
          description: "",
          visibility: "private",
          access: { kind: "public", readKey: new Uint8Array() },
          avatar: avatarFromWasm(preview.avatar),
          publisherInvite: true,
          ...inviterOf(preview.inviterName),
          link: url,
        };
      }
      throw new ChannelError("link-invalid");
    } finally {
      preview.free();
    }
  } finally {
    cap.free();
  }
}
