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
  /** Admission seeds (owner and publishers), standard base64. ADR-0090
   *  open question 4: two seeds of ONE version may coexist (two owner devices
   *  minted the same version at once); a ticket of that version is tried with
   *  every one of them. */
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
  /** This device set the mark itself (it made the channel private, created
   *  it private, or minted its first admission seed). A device that did not
   *  (a co-publisher, another device of the owner) cannot tell a reader from
   *  before the channel went private from a row with no ticket, so its
   *  let-in pass removes nobody ({@link letInVerdict}). */
  readonly admissionMarkSet?: true;
  /** Publisher account key -> its appointed co-publisher key (standard
   *  base64), for revoking the appointment. */
  readonly publisherKeys: readonly PublisherEntry[];
  /** A PublisherGroup commit of this device moved the channel to a new
   *  epoch and the channel record has not been posted again under it yet
   *  (ADR-0051 section 4: every epoch holds a record a late reader can
   *  open). Set with the commit's own record, cleared once the record
   *  landed; the publisher pass retries it until then. */
  readonly recordOwed?: boolean;
  /** A co-publisher removed a subscriber and its rotate request to the
   *  owner's device has not gone out yet (ADR-0087 section 15, amended
   *  2026-11-08). Written before the publish, cleared once it went out; the
   *  publisher pass retries it until then, so the removed device does not
   *  keep reading until the owner's next scheduled rotation. */
  readonly rotateRequestOwed?: boolean;
  /**
   * 2026-11-13 (follow-up decision 1): on the publishing device, the
   * account's one subscriber key (the channel-subscription register's) that
   * this device registered at the channels service, lower-case hex. Its
   * every rotation reseals the new seed to it, whatever the admission mark
   * says, so the account's reader devices keep reading a private channel.
   */
  readonly readerKeyRegisteredHex?: string;
  /** ADR-0090 section 6: what this owner device handed its OWN account's
   *  devices (the epoch seeds and admission seeds), per device like the
   *  publishers' records; cleared when an admission adds an own device. */
  readonly ownHandover?: PublisherEntry | null;
  /** ADR-0090: the channel epoch minus the PublisherGroup epoch, when they
   *  differ (a group founded again at a live channel's epoch, decision A;
   *  learned from the channels service when a device joined). Absent: 0. */
  readonly epochOffset?: number;
  /** ADR-0090: on a co-publisher's device, the channel owner's account key
   *  (standard base64) it verified as the `/g/` capability's issuer when it
   *  joined; written into the account's channel-subscription register. */
  readonly ownerAccountKeyB64?: string;
  /** ADR-0090 section 6 (security pass): the highest admission mark the
   *  channel OWNER's devices sent; counted only up to
   *  {@link rowsReadThroughSeq}. */
  readonly markPendingSeq?: number;
  /** The newest subscriber row (service `seq`) this device has itself read
   *  from the channels service. */
  readonly rowsReadThroughSeq?: number;
  /** ADR-0090 section 2.5 (security pass): the DS suggests the journal was
   *  founded again elsewhere (refused reads, another epoch). This device
   *  stopped publishing and rotating and waits as a reader; the group state
   *  is KEPT until an owner-signed Welcome for the same group replaces it. */
  readonly resetSuspected?: true;
}

/**
 * ADR-0090 section 6 (security pass), pure: the admission mark this device
 * counts: the owner's pending mark, but never past the rows this device has
 * itself read; never below what it held.
 */
export function effectiveAdmissionMark(held: number, pendingSeq: number, rowsReadThroughSeq: number): number {
  return Math.max(held, Math.min(pendingSeq, rowsReadThroughSeq));
}

/** The record after this device read subscriber rows up to `seq`: the read
 *  position moves, and a pending owner mark counts that much further. */
export function withRowsRead(record: ChannelAccessRecord, seq: number): ChannelAccessRecord {
  const read = Math.max(record.rowsReadThroughSeq ?? 0, seq);
  const admitted = effectiveAdmissionMark(record.admittedThroughSeq, record.markPendingSeq ?? 0, read);
  if (read === (record.rowsReadThroughSeq ?? 0) && admitted === record.admittedThroughSeq) return record;
  return { ...record, rowsReadThroughSeq: read, admittedThroughSeq: admitted };
}

/** The channel epoch a PublisherGroup epoch serves (ADR-0090). */
export function channelEpochOfGroup(record: ChannelAccessRecord, groupEpoch: bigint | number): number {
  return Number(groupEpoch) + (record.epochOffset ?? 0);
}

/** One publisher the owner approved here. */
export interface PublisherEntry {
  readonly accountKeyB64: string;
  /** The appointed co-publisher key (standard base64), "" when its binding
   *  was not read yet. */
  readonly copublisherKeyB64: string;
  /** Lower-case hex of the 16-byte account id the request named, or "". */
  readonly accountIdHex: string;
  /** The admission-seed versions already sent to this publisher (older
   *  entries; {@link sentSeedIds} decides since ADR-0090). */
  readonly sentVersions: readonly number[];
  /** ADR-0090: the admission seeds already sent, by {@link admissionSeedId}
   *  (every seed of a version is sent, not one per version). */
  readonly sentSeedIds?: readonly string[];
  /** ADR-0087 phase B: the channel's read key was sent to this publisher
   *  (it never changes, so once is enough). */
  readonly readKeySent?: boolean;
  /** ADR-0087 section 7 (amended 2026-11-08): the channel epochs whose seeds
   *  were handed to this publisher. A removed publisher's entry goes, so one
   *  added again gets a fresh hand-over. */
  readonly sentEpochs?: readonly number[];
}

/** The admission-seed versions a publisher still needs, ascending (ADR-0087
 *  section 7: every version, so tickets of older links still open). */
export function versionsToSend(record: ChannelAccessRecord, p: PublisherEntry): number[] {
  return [...new Set(seedsToSend(record, p).map((s) => s.version))].sort((a, b) => a - b);
}

/** A short id of an admission seed for the hand-over records (a prefix of
 *  its encoding; the records live in the same encrypted log as the seeds). */
export function admissionSeedId(seedB64: string): string {
  return seedB64.slice(0, 16);
}

/** ADR-0090 open question 4 — every admission seed a publisher still needs:
 *  every seed of every version not yet sent, ascending by version. A seed
 *  sent under the older per-version record counts as sent only when it is
 *  the one seed of its version. */
export function seedsToSend(
  record: ChannelAccessRecord,
  p: PublisherEntry,
): { readonly version: number; readonly seedB64: string }[] {
  const sentIds = new Set(p.sentSeedIds ?? []);
  return record.admissionSeeds
    .filter((s) => {
      if (sentIds.has(admissionSeedId(s.seedB64))) return false;
      const sameVersion = record.admissionSeeds.filter((x) => x.version === s.version).length;
      return !(sameVersion === 1 && p.sentSeedIds === undefined && p.sentVersions.includes(s.version));
    })
    .sort((a, b) => a.version - b.version);
}

/** The seeds with `add` folded in once each (by seed, not by version). */
export function withAdmissionSeeds(
  held: readonly { readonly version: number; readonly seedB64: string }[],
  add: readonly { readonly version: number; readonly seedB64: string }[],
): { readonly version: number; readonly seedB64: string }[] {
  const out = [...held];
  for (const a of add) if (!out.some((x) => x.seedB64 === a.seedB64)) out.push(a);
  return out;
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

function decodePublisherEntry(e: unknown): PublisherEntry | null {
  if (typeof e !== "object" || e === null) return null;
  const p = e as Record<string, unknown>;
  if (!(isB64(p.accountKeyB64) && p.accountKeyB64.length === KEY_B64_LEN && isB64(p.copublisherKeyB64))) return null;
  const sent = (Array.isArray(p.sentVersions) ? p.sentVersions : []).filter(
    (v: unknown): v is number => typeof v === "number" && Number.isInteger(v) && v >= 0,
  );
  return {
    accountKeyB64: p.accountKeyB64,
    copublisherKeyB64: p.copublisherKeyB64,
    accountIdHex: typeof p.accountIdHex === "string" && /^([0-9a-f]{32})?$/.test(p.accountIdHex) ? p.accountIdHex : "",
    sentVersions: [...new Set(sent)],
    ...(Array.isArray(p.sentSeedIds)
      ? {
          sentSeedIds: [
            ...new Set((p.sentSeedIds as unknown[]).filter((v: unknown): v is string => typeof v === "string" && v.length <= 24)),
          ],
        }
      : {}),
    ...(p.readKeySent === true ? { readKeySent: true } : {}),
    ...(Array.isArray(p.sentEpochs)
      ? {
          sentEpochs: [
            ...new Set(
              (p.sentEpochs as unknown[]).filter(
                (v: unknown): v is number => typeof v === "number" && Number.isInteger(v) && v >= 0,
              ),
            ),
          ],
        }
      : {}),
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
    if (seeds.some((x) => x.seedB64 === e.seedB64)) continue;
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
    ...(r.admissionMarkSet === true ? { admissionMarkSet: true as const } : {}),
    ...(r.recordOwed === true ? { recordOwed: true } : {}),
    ...(typeof r.markPendingSeq === "number" && Number.isInteger(r.markPendingSeq) && r.markPendingSeq > 0
      ? { markPendingSeq: r.markPendingSeq }
      : {}),
    ...(typeof r.rowsReadThroughSeq === "number" && Number.isInteger(r.rowsReadThroughSeq) && r.rowsReadThroughSeq > 0
      ? { rowsReadThroughSeq: r.rowsReadThroughSeq }
      : {}),
    ...(r.resetSuspected === true ? { resetSuspected: true as const } : {}),
    ...(isB64(r.ownerAccountKeyB64) && r.ownerAccountKeyB64.length === KEY_B64_LEN
      ? { ownerAccountKeyB64: r.ownerAccountKeyB64 }
      : {}),
    ...(typeof r.epochOffset === "number" && Number.isInteger(r.epochOffset) && r.epochOffset > 0
      ? { epochOffset: r.epochOffset }
      : {}),
    ...(r.rotateRequestOwed === true ? { rotateRequestOwed: true } : {}),
    ...(typeof r.readerKeyRegisteredHex === "string" && /^[0-9a-f]{64}$/.test(r.readerKeyRegisteredHex)
      ? { readerKeyRegisteredHex: r.readerKeyRegisteredHex }
      : {}),
    publisherKeys: (Array.isArray(r.publisherKeys) ? r.publisherKeys : []).flatMap((e: unknown) => {
      const d = decodePublisherEntry(e);
      return d === null ? [] : [d];
    }),
    ...(r.ownHandover !== undefined && r.ownHandover !== null
      ? ((): { ownHandover?: PublisherEntry } => {
          const d = decodePublisherEntry(r.ownHandover);
          return d === null ? {} : { ownHandover: d };
        })()
      : {}),
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

/** What the let-in pass does with one row above the mark (ADR-0087
 *  section 7). */
export type LetIn = "admit" | "remove" | "leave";

/**
 * The let-in verdict of a row above the mark (the rows at or below it are
 * readers: they came in before the channel went private, or an earlier pass
 * judged them; UX 4.1 "Everyone subscribed stays"):
 * - a ticket naming a live invite key: let in;
 * - this device's own key: left alone (a publisher's own card key);
 * - any other row, on a device that set the mark itself: removed;
 * - on a device that did not, left alone: no ticket alone never lets a row
 *   in, and the device holding the mark removes it. Pure; the same rule as
 *   iOS `SubscriberAdmission.letInVerdict` and Android
 *   `ChannelSubscriberRows.letInVerdict`.
 */
export function letInVerdict(ticketLive: boolean, markSet: boolean, isOwnKey: boolean): LetIn {
  if (ticketLive) return "admit";
  if (isOwnKey) return "leave";
  return markSet ? "remove" : "leave";
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
  links: readonly Pick<ChannelInviteLinkRecord, "invitePkHex" | "label" | "kind">[],
): { readonly kind: "link"; readonly label: string } | { readonly kind: "invitation" } | null {
  if (invitePkHex === null) return null;
  const link = links.find((l) => l.invitePkHex.toLowerCase() === invitePkHex.toLowerCase());
  if (link === undefined) return null;
  // UX 15.1: a contact invitation, or a link the owner gave no label, reads
  // `From an invitation` (the same rule on every client).
  const label = link.label.trim();
  return link.kind === "card" || label.length === 0 ? { kind: "invitation" } : { kind: "link", label };
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
