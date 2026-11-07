// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0079 section 4 — the account's own invite links as the synced state
 * holds them: one register per link, carried between the account's devices
 * over the DeviceGroup, last writer wins per link, a revocation final.
 *
 * This module is the read side for the screens (the projection of the fold
 * into plain values) and the two pure rules the screens need on top of the
 * fold: which links the "Invite links" list shows, and how a link a person
 * opened is told apart from a link they made. The fold itself, and the one
 * rule that decides a request's link answer, are in Rust (`lakko-sync`
 * `LinkRecordEntry::verdict`), so the three clients cannot disagree.
 *
 * The link secret and the revocation seed are SECRET: they live in the
 * synced state (sealed under the device key on disk, end-to-end encrypted
 * between the account's devices) and are never logged. The revocation seed
 * travels with the record (ADR-0079 as reworked 2026-10-17) so that any
 * device that revokes a link deletes its ledger row at once.
 */
import type { SyncedState as WasmSyncedState } from "../../../wasm/lakko_ffi_wasm";

/** Which ledger a link lives in, and for a `/g/` link what it invites to
 *  (ADR-0087 phase B: group, channel subscriber, channel publisher). */
export type LinkRecordKind = "contact" | "call" | "group" | "channel" | "publisher";

/** The `/g/` kinds: their register key is `groupLinkRecordId(ledgerId)`. */
export function isGroupLinkKind(kind: LinkRecordKind): kind is "group" | "channel" | "publisher" {
  return kind === "group" || kind === "channel" || kind === "publisher";
}

/** The wasm `LinkRecordKind` numbers (frozen in `lakko-sync`). */
export interface LinkRecordKindNumbers {
  readonly Contact: number;
  readonly Call: number;
  readonly Group: number;
  readonly Channel: number;
  readonly Publisher: number;
}

/** The model kind of a wasm kind number; `null` for one this client does not
 *  know. Pure. */
export function kindOfNumber(n: number, kinds: LinkRecordKindNumbers): LinkRecordKind | null {
  if (n === kinds.Contact) return "contact";
  if (n === kinds.Call) return "call";
  if (n === kinds.Group) return "group";
  if (n === kinds.Channel) return "channel";
  if (n === kinds.Publisher) return "publisher";
  return null;
}

/** The wasm kind number of a model kind. Pure. */
export function numberOfKind(kind: LinkRecordKind, kinds: LinkRecordKindNumbers): number {
  switch (kind) {
    case "contact":
      return kinds.Contact;
    case "call":
      return kinds.Call;
    case "group":
      return kinds.Group;
    case "channel":
      return kinds.Channel;
    case "publisher":
      return kinds.Publisher;
  }
}

/** One invite link of the account, as the screens read it. */
export interface LinkRecordModel {
  /** The link's 16-byte id, lower-case hex: the register key. */
  readonly linkIdHex: string;
  readonly linkId: Uint8Array;
  /** The link's 12-byte secret (the URL fragment). SECRET. */
  readonly linkSecret: Uint8Array;
  readonly kind: LinkRecordKind;
  /** The 6-byte ledger id. */
  readonly ledgerId: Uint8Array;
  /** The 32-byte seed that deletes the ledger row (ADR-0078 section 10),
   *  empty when there is no row. SECRET. */
  readonly revocationSeed: Uint8Array;
  /** UNIX millis after which the link no longer works; 0 = never. */
  readonly expiryMs: number;
  /** The use limit as the capability carries it (0 = single use,
   *  0xffffffff = no limit). */
  readonly maxUses: number;
  /** Requests accepted through the link so far. */
  readonly uses: number;
  readonly revoked: boolean;
  /** Copied on one of the account's devices (final, 2026-10-26). */
  readonly shared: boolean;
  /** The link's terms may still be changed: not revoked, never copied,
   *  never answered. The core's one rule (`LinkRecordEntry::editable`). */
  readonly editable: boolean;
  readonly mintedAtMs: number;
  readonly label: string;
  /** `/g/` links: the 32-byte group id or the 16-byte channel id; empty
   *  otherwise. */
  readonly targetId: Uint8Array;
  /** A private channel subscriber link's 32-byte invite seed, else empty.
   *  SECRET. */
  readonly inviteSeed: Uint8Array;
  /** The admission-key version `inviteSeed` names (0 when none). */
  readonly admissionVersion: number;
  /** The sealed `/g/` blob the ledger holds, else empty. */
  readonly capability: Uint8Array;
}

/** What a device does with a contact request carrying a link answer. */
export type LinkAnswerVerdict = "accept" | "answered" | "dead" | "unknown";

/** A link just minted, as the register first records it. */
export interface NewLinkRecord {
  readonly linkId: Uint8Array;
  readonly linkSecret: Uint8Array;
  readonly kind: LinkRecordKind;
  readonly ledgerId: Uint8Array;
  /** The 32-byte seed the ledger row was created under. */
  readonly revocationSeed: Uint8Array;
  readonly expiryMs: number;
  readonly maxUses: number;
  readonly mintedAtMs: number;
  readonly label: string;
  /** `/g/` links only (see {@link LinkRecordModel}). */
  readonly targetId?: Uint8Array;
  readonly inviteSeed?: Uint8Array;
  readonly admissionVersion?: number;
  readonly capability?: Uint8Array;
}

export function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** The fold's link records as plain values; an unreadable fold reads as
 *  none. */
export function readLinkRecords(synced: WasmSyncedState, kinds: LinkRecordKindNumbers): LinkRecordModel[] {
  let records;
  try {
    records = synced.allLinkRecords();
  } catch {
    return [];
  }
  const out: LinkRecordModel[] = [];
  for (const r of records) {
    try {
      const kind = kindOfNumber(r.kind, kinds);
      if (kind === null) continue;
      const linkId = r.linkId.slice();
      out.push({
        linkIdHex: hexOf(linkId),
        linkId,
        linkSecret: r.linkSecret.slice(),
        kind,
        ledgerId: r.ledgerId.slice(),
        revocationSeed: r.revocationSeed.slice(),
        expiryMs: Number(r.expiryMs),
        maxUses: r.maxUses,
        uses: r.uses,
        revoked: r.revoked,
        shared: r.shared,
        editable: r.editable,
        mintedAtMs: Number(r.mintedAtMs),
        label: r.label,
        targetId: r.targetId.slice(),
        inviteSeed: r.inviteSeed.slice(),
        admissionVersion: r.admissionVersion,
        capability: r.capability.slice(),
      });
    } finally {
      r.free();
    }
  }
  return out;
}

/** The use limit as a number of requests; `null` for no limit. Mirrors
 *  `LinkRecordEntry::effective_limit`. Pure. */
export function effectiveLimit(maxUses: number): number | null {
  if (maxUses === 0) return 1;
  if (maxUses === 0xffff_ffff) return null;
  return maxUses;
}

/** Whether a link still works at `nowMs`: not revoked, not expired, uses
 *  left. Mirrors `LinkRecordEntry::is_live`. Pure. */
export function isLinkLive(r: LinkRecordModel, nowMs: number): boolean {
  if (r.revoked) return false;
  if (r.expiryMs !== 0 && nowMs >= r.expiryMs) return false;
  const limit = effectiveLimit(r.maxUses);
  return limit === null || r.uses < limit;
}

/** The links the "Invite links" list shows (UX section 5: a single-use link
 *  that was used disappears; a multi-use one counts down): live contact
 *  links, newest first. Pure. */
export function listedContactLinks(records: readonly LinkRecordModel[], nowMs: number): LinkRecordModel[] {
  return records
    .filter((r) => r.kind === "contact" && isLinkLive(r, nowMs))
    .sort((a, b) => b.mintedAtMs - a.mintedAtMs);
}

/** The live `/g/` links of one target (a group or a channel) of one kind,
 *  every device's, newest first. Pure. */
export function listedTargetLinks(
  records: readonly LinkRecordModel[],
  kind: "group" | "channel" | "publisher",
  targetIdHex: string,
  nowMs: number,
): LinkRecordModel[] {
  const target = targetIdHex.toLowerCase();
  return records
    .filter((r) => r.kind === kind && hexOf(r.targetId) === target && isLinkLive(r, nowMs))
    .sort((a, b) => b.mintedAtMs - a.mintedAtMs);
}
