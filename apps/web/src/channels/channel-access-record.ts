// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 phase B — what this device holds about a channel's access, beside
 * the channel log's other sections: whether it is public or private, its
 * `@name` and the certificate that binds it, topic and language, its photo,
 * the read key a public reader came by, whether a private reader is still
 * waiting to be let in, and for a publisher the admission seeds, the invite
 * links this device minted and the publishers it knows.
 *
 * Pure: the shape, a strict per-field decode and the small rules the loops
 * and the subscriber list apply, so `vitest` can check them. Every secret
 * here is written only into the device-key-encrypted channel log, never
 * logged.
 */

import { avatarFromJson, type AvatarData } from "../profile/avatar-model";

export type ChannelVisibility = "public" | "private";

/** One link (or one-use invitation card) this device minted. */
export interface ChannelInviteLinkRecord {
  /** base64url (no padding) of the 6-byte ledger id. */
  readonly ledgerIdB64Url: string;
  readonly url: string;
  readonly label: string;
  readonly expiryMs: number;
  readonly maxUses: number;
  /** 32-byte invite seed (SECRET), standard base64. Empty on a public link. */
  readonly inviteSeedB64: string;
  /** Hex of the invite public key registered on the service, or "". */
  readonly invitePkHex: string;
  /** The link's own ledger revocation seed (SECRET), standard base64. */
  readonly revocationSeedB64: string;
  /** `card`: a one-use contact invitation; `publisher`: a co-publisher link. */
  readonly kind: "link" | "card" | "publisher";
  readonly revoked: boolean;
  readonly mintedAtMs: number;
  /** A publisher link: its plaintext capability wire (standard base64), so a
   *  sealed knock carrying it is recognised as named by this device. */
  readonly capabilityB64?: string;
  /** A publisher link was used by one request (one use). */
  readonly used?: boolean;
}

/** An owed change of the channel's type (UX 4.1), retried until it lands. */
export interface OwedChannelType {
  readonly visibility: ChannelVisibility;
  readonly handle: string;
  readonly topic: number;
  readonly language: string;
  /** The visibility before the change, for the private step (reseal, re-key). */
  readonly from: ChannelVisibility | null;
}

export interface ChannelAccessRecord {
  readonly visibility: ChannelVisibility | null;
  readonly handle: string | null;
  /** The discovery handle certificate (169 bytes), standard base64. */
  readonly handleCertB64: string | null;
  /** The signed `ConversationHandle` envelope the meta record v3 carries
   *  (`composeConversationHandleEnvelope`), standard base64. */
  readonly handleEnvelopeB64: string | null;
  readonly topic: number | null;
  readonly language: string | null;
  readonly avatar: AvatarData | null;
  /** A public reader's read key (32 bytes), standard base64. */
  readonly readKeyB64: string | null;
  /** A private reader registered and not yet let in. */
  readonly waiting: boolean;
  /** Admission seeds by version (owner and publishers), standard base64. */
  readonly admissionSeeds: readonly { readonly version: number; readonly seedB64: string }[];
  readonly inviteLinks: readonly ChannelInviteLinkRecord[];
  /** Account keys (standard base64) the owner invited to publish. */
  readonly namedPublishers: readonly string[];
  /** Account keys (standard base64) this device knows publish here. */
  readonly publishers: readonly string[];
  readonly owedType: OwedChannelType | null;
  /**
   * The highest subscriber row (service `seq`) this device let in. Every row
   * at or below it holds a seed envelope per key: a private channel's rows
   * whose ticket checked out (an unchecked row is removed), and the rows of
   * the time a channel was private. A rotation reseals per key to exactly
   * these; a public channel's later rows read the public seed instead.
   */
  readonly admittedThroughSeq: number;
  /** Publisher account key -> its appointed co-publisher key (standard
   *  base64), for revoking the appointment. */
  readonly publisherKeys: readonly PublisherEntry[];
}

/** One publisher the owner approved here. */
export interface PublisherEntry {
  readonly accountKeyB64: string;
  /** The appointed co-publisher key (standard base64), "" when its binding
   *  was not read yet. */
  readonly copublisherKeyB64: string;
  /** Lower-case hex of the 16-byte account id the request named, or "". */
  readonly accountIdHex: string;
  /** The admission-seed versions already sent to this publisher. */
  readonly sentVersions: readonly number[];
  /** ADR-0087 phase B: the channel's read key was sent to this publisher
   *  (it never changes, so once is enough). */
  readonly readKeySent?: boolean;
}

/** The admission-seed versions a publisher still needs, ascending (ADR-0087
 *  section 7: every version, so tickets of older links still open). */
export function versionsToSend(record: ChannelAccessRecord, p: PublisherEntry): number[] {
  return record.admissionSeeds
    .map((s) => s.version)
    .filter((v) => !p.sentVersions.includes(v))
    .sort((a, b) => a - b);
}

export function emptyAccessRecord(): ChannelAccessRecord {
  return {
    visibility: null,
    handle: null,
    handleCertB64: null,
    handleEnvelopeB64: null,
    topic: null,
    language: null,
    avatar: null,
    readKeyB64: null,
    waiting: false,
    admissionSeeds: [],
    inviteLinks: [],
    namedPublishers: [],
    publishers: [],
    owedType: null,
    admittedThroughSeq: 0,
    publisherKeys: [],
  };
}

function isB64(v: unknown): v is string {
  return typeof v === "string" && /^[A-Za-z0-9+/]*={0,2}$/.test(v);
}

const KEY_B64_LEN = 44;

function decodeLink(raw: unknown): ChannelInviteLinkRecord | null {
  if (typeof raw !== "object" || raw === null) return null;
  const l = raw as Record<string, unknown>;
  if (typeof l.ledgerIdB64Url !== "string" || !/^[A-Za-z0-9_-]+$/.test(l.ledgerIdB64Url)) return null;
  if (typeof l.url !== "string" || typeof l.label !== "string") return null;
  if (typeof l.expiryMs !== "number" || typeof l.maxUses !== "number") return null;
  if (!isB64(l.inviteSeedB64) || !isB64(l.revocationSeedB64)) return null;
  if (typeof l.invitePkHex !== "string" || !/^([0-9a-f]{64})?$/.test(l.invitePkHex)) return null;
  if (l.kind !== "link" && l.kind !== "card" && l.kind !== "publisher") return null;
  return {
    ledgerIdB64Url: l.ledgerIdB64Url,
    url: l.url,
    label: l.label,
    expiryMs: l.expiryMs,
    maxUses: l.maxUses,
    inviteSeedB64: l.inviteSeedB64,
    invitePkHex: l.invitePkHex,
    revocationSeedB64: l.revocationSeedB64,
    kind: l.kind,
    revoked: l.revoked === true,
    mintedAtMs: typeof l.mintedAtMs === "number" ? l.mintedAtMs : 0,
    ...(isB64(l.capabilityB64) && l.capabilityB64.length > 0 ? { capabilityB64: l.capabilityB64 } : {}),
    ...(l.used === true ? { used: true } : {}),
  };
}

/** Strict per field: a malformed field reads as absent, never as a guess.
 *  `null` only when the value is not an object at all. */
export function decodeAccessRecord(raw: unknown): ChannelAccessRecord | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  const visibility = r.visibility === "public" || r.visibility === "private" ? r.visibility : null;
  const handle = typeof r.handle === "string" && /^[a-z0-9_]{3,32}$/.test(r.handle) ? r.handle : null;
  const topic =
    typeof r.topic === "number" && Number.isInteger(r.topic) && r.topic >= 0 && r.topic <= 255 ? r.topic : null;
  const language = typeof r.language === "string" && /^([a-z]{2,3})?$/.test(r.language) ? r.language : null;
  const seeds: { version: number; seedB64: string }[] = [];
  for (const s of Array.isArray(r.admissionSeeds) ? r.admissionSeeds : []) {
    if (typeof s !== "object" || s === null) continue;
    const e = s as Record<string, unknown>;
    if (typeof e.version !== "number" || !Number.isInteger(e.version) || e.version < 0) continue;
    if (!isB64(e.seedB64) || e.seedB64.length !== KEY_B64_LEN) continue;
    if (seeds.some((x) => x.version === e.version)) continue;
    seeds.push({ version: e.version, seedB64: e.seedB64 });
  }
  const links: ChannelInviteLinkRecord[] = [];
  for (const l of Array.isArray(r.inviteLinks) ? r.inviteLinks : []) {
    const d = decodeLink(l);
    if (d !== null && !links.some((x) => x.ledgerIdB64Url === d.ledgerIdB64Url)) links.push(d);
  }
  const keys = (v: unknown): string[] => {
    const out: string[] = [];
    for (const k of Array.isArray(v) ? v : []) {
      if (isB64(k) && k.length === KEY_B64_LEN && !out.includes(k)) out.push(k);
    }
    return out;
  };
  let owedType: OwedChannelType | null = null;
  if (typeof r.owedType === "object" && r.owedType !== null) {
    const o = r.owedType as Record<string, unknown>;
    if (
      (o.visibility === "public" || o.visibility === "private") &&
      typeof o.handle === "string" &&
      typeof o.topic === "number" &&
      typeof o.language === "string"
    ) {
      owedType = {
        visibility: o.visibility,
        handle: o.handle,
        topic: o.topic,
        language: o.language,
        from: o.from === "public" || o.from === "private" ? o.from : null,
      };
    }
  }
  return {
    visibility,
    handle,
    handleCertB64: isB64(r.handleCertB64) && r.handleCertB64.length > 0 ? r.handleCertB64 : null,
    handleEnvelopeB64: isB64(r.handleEnvelopeB64) && r.handleEnvelopeB64.length > 0 ? r.handleEnvelopeB64 : null,
    topic,
    language,
    avatar: avatarFromJson(r.avatar),
    readKeyB64: isB64(r.readKeyB64) && r.readKeyB64.length === KEY_B64_LEN ? r.readKeyB64 : null,
    waiting: r.waiting === true,
    admissionSeeds: seeds,
    inviteLinks: links,
    namedPublishers: keys(r.namedPublishers),
    publishers: keys(r.publishers),
    owedType,
    admittedThroughSeq:
      typeof r.admittedThroughSeq === "number" && Number.isInteger(r.admittedThroughSeq) && r.admittedThroughSeq >= 0
        ? r.admittedThroughSeq
        : 0,
    publisherKeys: (Array.isArray(r.publisherKeys) ? r.publisherKeys : []).flatMap((e: unknown) => {
      if (typeof e !== "object" || e === null) return [];
      const p = e as Record<string, unknown>;
      if (!(isB64(p.accountKeyB64) && p.accountKeyB64.length === KEY_B64_LEN && isB64(p.copublisherKeyB64))) return [];
      const sent = (Array.isArray(p.sentVersions) ? p.sentVersions : []).filter(
        (v: unknown): v is number => typeof v === "number" && Number.isInteger(v) && v >= 0,
      );
      return [
        {
          accountKeyB64: p.accountKeyB64,
          copublisherKeyB64: p.copublisherKeyB64,
          accountIdHex: typeof p.accountIdHex === "string" && /^([0-9a-f]{32})?$/.test(p.accountIdHex) ? p.accountIdHex : "",
          sentVersions: [...new Set(sent)],
          ...(p.readKeySent === true ? { readKeySent: true } : {}),
        },
      ];
    }),
  };
}

/**
 * Which rows of one page a private channel's admission pass handles
 * (ADR-0087 section 7): rows above `admittedThroughSeq`, in order. Each is
 * admitted when its ticket names a live key, else removed. Returns the new
 * high-water once every row of the page was settled.
 */
export function rowsToAdmit<T extends { readonly seq: number }>(rows: readonly T[], admittedThroughSeq: number): T[] {
  return rows.filter((r) => r.seq > admittedThroughSeq).sort((a, b) => a.seq - b.seq);
}

/** The newest admission seed (highest version), or `null`. */
export function newestAdmissionSeed(
  r: ChannelAccessRecord,
): { readonly version: number; readonly seedB64: string } | null {
  let best: { version: number; seedB64: string } | null = null;
  for (const s of r.admissionSeeds) if (best === null || s.version > best.version) best = s;
  return best;
}

/**
 * How a subscriber row came in, for the subscriber list (UX 15.1): the
 * invite key its ticket names, matched against this device's links. A card
 * (one-use contact invitation) reads "From an invitation"; a link its label;
 * a key no link here minted (another publisher's), or no ticket, nothing.
 */
export function subscriberVia(
  invitePkHex: string | null,
  links: readonly ChannelInviteLinkRecord[],
): { readonly kind: "link"; readonly label: string } | { readonly kind: "invitation" } | null {
  if (invitePkHex === null) return null;
  const link = links.find((l) => l.invitePkHex === invitePkHex.toLowerCase());
  if (link === undefined) return null;
  return link.kind === "card" ? { kind: "invitation" } : { kind: "link", label: link.label };
}

/**
 * Whether a ticket's invite key admits a row (ADR-0087 section 7, "At the
 * publisher"): the key must be one of this channel's live keys on the
 * service. A revoked key is gone from that list, so it admits nobody new.
 */
export function ticketKeyAdmits(invitePkHex: string | null, liveKeys: ReadonlySet<string>): boolean {
  return invitePkHex !== null && liveKeys.has(invitePkHex.toLowerCase());
}

/**
 * The visibility this device shows (ADR-0087 section 16): the owner's own
 * setting wins; a reader goes by the newest metadata record's flag, else by
 * what it came by.
 */
export function foldVisibility(args: {
  readonly ownerSetting: ChannelVisibility | null;
  readonly metaListed: boolean | undefined;
  readonly cameBy: ChannelVisibility | null;
}): ChannelVisibility | null {
  if (args.ownerSetting !== null) return args.ownerSetting;
  if (args.metaListed !== undefined) return args.metaListed ? "public" : "private";
  return args.cameBy;
}

/**
 * The rotation a subscriber removal needs, in order (ADR-0087 section 7):
 * the removal on the service first, so the rotation's reseal never includes
 * the removed key; then the rotation. A removal that did not land rotates
 * nothing (the caller retries the whole step).
 */
export type RemovalStep = "remove" | "rotate";
export function removalSteps(alreadyRemoved: boolean): readonly RemovalStep[] {
  return alreadyRemoved ? ["rotate"] : ["remove", "rotate"];
}
