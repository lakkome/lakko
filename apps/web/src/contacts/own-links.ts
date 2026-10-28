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

/** The links (hex ids) whose ledger row this device revoked and has not
 *  deleted yet; retried at every change of the register and at start. */
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

/** The row deletions running on this device, by account and link id. */
const rowDeletionsInFlight = new Map<string, Promise<OwnLinkRevokeOutcome>>();

/**
 * Delete the ledger row of a link this device revoked, exactly once
 * (ADR-0079 section 4). The ledger holds ONE row per link, and only the
 * device that revokes the link deletes it: the caller marks the deletion
 * owed ({@link setOwedRowDeletion}) before it revokes the link in the
 * register, so a crash in between leaves it owed. This runs the deletion
 * while it is owed, clears it once the ledger answered (gone included), and
 * keeps it owed when the ledger could not be reached. A call while the same
 * deletion is running joins it; a call once it is no longer owed sends
 * nothing and answers `"done"`. Throws what the ledger refusal throws (the
 * deletion then stays owed).
 */
export function deleteRevokedLinkRow(
  discovery: Pick<DiscoveryClient, "revokeInvite" | "revokeCallInvite">,
  accountIdB64: string,
  record: LedgerRowRef & { readonly linkIdHex: string },
): Promise<OwnLinkRevokeOutcome> {
  const key = `${accountIdB64}|${record.linkIdHex}`;
  const running = rowDeletionsInFlight.get(key);
  if (running !== undefined) return running;
  if (!owedRowDeletions(accountIdB64).has(record.linkIdHex)) return Promise.resolve("done");
  const run = (async (): Promise<OwnLinkRevokeOutcome> => {
    const outcome = await deleteLedgerRowOfRecord(discovery, record);
    if (outcome !== "unreachable") setOwedRowDeletion(accountIdB64, record.linkIdHex, false);
    return outcome;
  })().finally(() => rowDeletionsInFlight.delete(key));
  rowDeletionsInFlight.set(key, run);
  return run;
}

/**
 * ADR-0079 section 4 — links another device of the account revoked, of
 * which this device still holds a local record (`minted-links.ts`, the
 * device that minted them). The revoking device deletes the ledger row
 * itself, and keeps the deletion owed until the ledger answered; so this
 * sends the ledger nothing and only drops what is local: the record and the
 * link's reach key. The one exception is a synced record with no revocation
 * seed, whose row no other device can delete: this device deletes it with
 * the seed of its own record. Returns how many could not be reached (tried
 * again at the next change). Never throws.
 */
export async function forgetLinksRevokedElsewhere(
  discovery: DiscoveryClient,
  accountIdB64: string,
  revoked: readonly { readonly linkIdHex: string; readonly revocationSeed: Uint8Array }[],
): Promise<number> {
  if (revoked.length === 0) return 0;
  const seedless = new Set(revoked.filter((r) => r.revocationSeed.length === 0).map((r) => r.linkIdHex));
  const all = new Set(revoked.map((r) => r.linkIdHex));
  let unreachable = 0;
  for (const link of await listMintedLinks(accountIdB64)) {
    let hex = "";
    for (const b of link.linkId) hex += b.toString(16).padStart(2, "0");
    if (!all.has(hex)) continue;
    try {
      if (seedless.has(hex) && link.revocationSeed !== null) {
        if ((await revokeMintedLink(discovery, accountIdB64, link)) === "unreachable") unreachable += 1;
        continue;
      }
      await reachSession()?.revokeLink(link.linkId).catch(() => undefined);
      await forgetMintedLink(accountIdB64, link.kind, link.blobIdSlug);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("own-links: a link revoked elsewhere could not be forgotten", err instanceof Error ? err.name : "");
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
