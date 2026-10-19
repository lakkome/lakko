// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The links this device minted, revoked with their own keys (ADR-0078
 * section 10).
 *
 * A ledger row (`/i/`, `/c/`, `/g/`) names no owner and no issuer; it holds
 * the public half of a revocation key drawn for that one link. This module
 * draws that key at mint ({@link newRevocationKey}) and revokes a kept link
 * with the seed its record holds ({@link revokeMintedLink}), so the
 * directory never sees the account or its identity key on any link.
 *
 * Account deletion does not delete rows by owner any more (there is none):
 * {@link revokeAllMintedLinks} runs first, and a link this device has no
 * record of lives until it expires or is used up.
 */
import { loadWasm } from "../wasm";
import { base64UrlDecodeNoPad } from "../auth/account-id";
import type { DiscoveryClient, RevokeInviteResult } from "../discovery/client";
import { reachSession } from "../reach/reach-service";
import {
  forgetMintedLink,
  listMintedLinks,
  type MintedLinkRow,
} from "./minted-links";

/** A fresh revocation key for one link: the seed the device keeps, and the
 *  public key the upload carries. */
export async function newRevocationKey(): Promise<{
  readonly seed: Uint8Array;
  readonly publicKey: Uint8Array;
}> {
  const wasm = await loadWasm();
  const seed = crypto.getRandomValues(new Uint8Array(32));
  return { seed, publicKey: wasm.linkRevocationPublicKey(seed) };
}

/** How a revoke of one kept link ended. */
export type OwnLinkRevokeOutcome =
  /** The ledger deleted the row, or had no row any more (gone, expired,
   *  revoked earlier); the local record and the reach key are gone too. */
  | "done"
  /** The ledger could not be reached; the record is kept for another try. */
  | "unreachable"
  /** The record holds no revocation seed (kept before ADR-0078): only the
   *  reach key and the record are dropped, the row lives until it expires. */
  | "no-key";

/** Revoke one link this device minted, with its own revocation seed, then
 *  drop its reach key and its record. */
export async function revokeMintedLink(
  discovery: DiscoveryClient,
  accountIdB64: string,
  link: MintedLinkRow,
): Promise<OwnLinkRevokeOutcome> {
  let outcome: OwnLinkRevokeOutcome = "done";
  if (link.revocationSeed === null) {
    outcome = "no-key";
  } else {
    let blobId: Uint8Array;
    try {
      blobId = base64UrlDecodeNoPad(link.blobIdSlug);
    } catch {
      blobId = new Uint8Array();
    }
    if (blobId.length > 0) {
      try {
        const revoke = (): Promise<RevokeInviteResult> => {
          const seed = link.revocationSeed as Uint8Array;
          switch (link.kind) {
            case "contact":
              return discovery.revokeInvite(blobId, seed);
            case "call":
              return discovery.revokeCallInvite(blobId, seed);
            case "group":
              return discovery.revokeGroupInvite(blobId, seed);
          }
        };
        await revoke();
      } catch (err) {
        if (err instanceof Error && err.name === "DiscoveryNetworkError") return "unreachable";
        throw err;
      }
    }
  }
  if (link.linkId.length > 0) {
    await reachSession()?.revokeLink(link.linkId).catch(() => undefined);
  }
  await forgetMintedLink(accountIdB64, link.kind, link.blobIdSlug);
  return outcome;
}

/** What {@link deleteLedgerRowOfRecord} needs from a link record. */
export interface LedgerRowRef {
  readonly kind: "contact" | "call";
  /** The 6-byte ledger id. */
  readonly ledgerId: Uint8Array;
  /** The 32-byte seed the row was created under. */
  readonly revocationSeed: Uint8Array;
}

/**
 * ADR-0079 (reworked 2026-10-17) - delete a link's ledger row with the
 * revocation seed its synced record carries. Any device of the account holds
 * the seed, so the device that revokes deletes the row itself, at once.
 * `"done"` also when the ledger has no row any more. Never throws for the
 * ledger's answer.
 */
export async function deleteLedgerRowOfRecord(
  discovery: Pick<DiscoveryClient, "revokeInvite" | "revokeCallInvite">,
  record: LedgerRowRef,
): Promise<OwnLinkRevokeOutcome> {
  if (record.revocationSeed.length === 0) return "no-key";
  try {
    if (record.kind === "contact") await discovery.revokeInvite(record.ledgerId, record.revocationSeed);
    else await discovery.revokeCallInvite(record.ledgerId, record.revocationSeed);
  } catch (err) {
    if (err instanceof Error && err.name === "DiscoveryNetworkError") return "unreachable";
    throw err;
  }
  return "done";
}

const OWED_PREFIX = "lakko.link-row-deletions-owed.v1.";

/** The links (hex ids) whose ledger row this device revoked but could not
 *  delete yet; retried at every change of the register and at start. */
export function owedRowDeletions(accountIdB64: string): Set<string> {
  try {
    const raw = localStorage.getItem(OWED_PREFIX + accountIdB64);
    const parsed: unknown = raw === null ? [] : JSON.parse(raw);
    return new Set(Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

/** Record (or clear, `owed = false`) an owed row deletion. Best-effort. */
export function setOwedRowDeletion(accountIdB64: string, linkIdHex: string, owed: boolean): void {
  const set = owedRowDeletions(accountIdB64);
  if (owed === set.has(linkIdHex)) return;
  if (owed) set.add(linkIdHex);
  else set.delete(linkIdHex);
  try {
    if (set.size === 0) localStorage.removeItem(OWED_PREFIX + accountIdB64);
    else localStorage.setItem(OWED_PREFIX + accountIdB64, JSON.stringify([...set]));
  } catch {
    // The minting device's own cleanup still deletes the row.
  }
}

/** Forget device: drop the owed list. */
export function clearOwedRowDeletions(accountIdB64: string): void {
  try {
    localStorage.removeItem(OWED_PREFIX + accountIdB64);
  } catch {
    // best-effort
  }
}

/**
 * ADR-0079 section 4 — a link another device of the account revoked: the
 * minting device still holds a local record of it (`minted-links.ts`).
 * The device that revoked it has already deleted the ledger row with the
 * seed the synced record carries; this drops the local record (and deletes
 * the row again, which the ledger answers as gone, or deletes it when the
 * revoking device could not reach the ledger). Returns how many could not
 * be reached (tried again at the next change). Never throws.
 */
export async function revokeLinksRevokedElsewhere(
  discovery: DiscoveryClient,
  accountIdB64: string,
  revokedLinkIdsHex: ReadonlySet<string>,
): Promise<number> {
  if (revokedLinkIdsHex.size === 0) return 0;
  let unreachable = 0;
  for (const link of await listMintedLinks(accountIdB64)) {
    let hex = "";
    for (const b of link.linkId) hex += b.toString(16).padStart(2, "0");
    if (!revokedLinkIdsHex.has(hex)) continue;
    try {
      if ((await revokeMintedLink(discovery, accountIdB64, link)) === "unreachable") unreachable += 1;
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("own-links: a link revoked elsewhere could not be revoked", err instanceof Error ? err.name : "");
    }
  }
  return unreachable;
}

/** Drop this device's local record of a link (ADR-0078 phase 8) once its
 *  ledger row was dealt with through the synced record. Never throws. */
export async function forgetMintedLinkRecord(accountIdB64: string, link: MintedLinkRow): Promise<void> {
  try {
    await forgetMintedLink(accountIdB64, link.kind, link.blobIdSlug);
  } catch {
    // best-effort: a record left behind is revoked again at account deletion,
    // which the ledger answers as gone
  }
}

/** Revoke every live link this device keeps a record of (account deletion,
 *  ADR-0078 section 10). Returns how many could not be reached; never throws
 *  for one link's refusal. */
export async function revokeAllMintedLinks(
  discovery: DiscoveryClient,
  accountIdB64: string,
): Promise<number> {
  let unreachable = 0;
  for (const link of await listMintedLinks(accountIdB64)) {
    if (link.facts !== null && link.facts.expiryMs !== 0 && link.facts.expiryMs <= Date.now()) {
      await forgetMintedLink(accountIdB64, link.kind, link.blobIdSlug);
      continue;
    }
    try {
      if ((await revokeMintedLink(discovery, accountIdB64, link)) === "unreachable") {
        unreachable += 1;
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("own-links: a link could not be revoked", err instanceof Error ? err.name : "");
    }
  }
  return unreachable;
}
