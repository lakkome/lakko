// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0050 section 2.9 — the state bundle a pairing hands to the new device
 * beside the history bundle: accepted contacts (with their local renames),
 * the per-contact identity pins and rotation markers, and the ids of the
 * groups the old device knows. Everything that lives in this browser's
 * `localStorage` and would otherwise reach a second device only through a
 * backup that runs at signup and recover.
 *
 * Carried as ONE more row on the pairing rendezvous mailbox, tagged by a
 * plaintext magic so the three polls (history, recovery, state) can each skip
 * the rows that are not theirs in whatever FIFO order the scanner published
 * them. Sealed under the DeviceGroup exporter at the post-add epoch, the
 * recovery row's construction: both devices derive the same key, the DS
 * stores ciphertext.
 *
 * Channel subscriptions are NOT here yet: the channel list does not survive a
 * reload on this device either (`docs/PRODUCT-READINESS.md`), so there is
 * nothing durable to carry until that lands.
 */
import { DsClient, DsHttpError, DsNetworkError } from "../ds/client";
import {
  boundContactsAccount,
  loadAccepted,
  readPin,
  setPin,
  upsertAccepted,
  type AcceptedContact,
} from "../contacts/local-store";
import { base64Encode } from "../auth/account-id";

/** Exporter label; the context is the 32-byte rendezvous pseudonym. */
export const STATE_EXPORTER_LABEL = "lakko/state-bundle/v1";
/** Exporter output length — the ChaCha20-Poly1305 key size. */
export const STATE_EXPORTER_KEY_LENGTH = 32;

const STATE_ROW_MAGIC = new TextEncoder().encode("lakko/state-bundle-row/v1\0");
const STATE_POLL_TIMEOUT_MS = 20_000;
const PER_REQUEST_LONG_POLL_SECS = 5;
/** Prefix of the per-account known-group list; the account id follows. */
const KNOWN_GROUPS_PREFIX = "lakko.pairing.known-groups.v2.";
/** The retired device-wide list. Never read; dropped by Forget device. */
const RETIRED_KNOWN_GROUPS_KEY = "lakko.pairing.known-groups.v1";
const MAX_BUNDLE_BYTES = 1 << 20;

/** A pinned account key as carried in the bundle. */
export interface CarriedPin {
  readonly accountIdB64: string;
  readonly pinnedSignaturePubkeyB64: string;
  readonly rotationMarker: number;
}

/** The bundle. `v` is the only version there is. */
export interface StateBundleV1 {
  readonly v: 1;
  readonly contacts: readonly AcceptedContact[];
  readonly pins: readonly CarriedPin[];
  /** base64 group ids the old device holds; the new device counts the ones
   *  it has not joined yet as "still synchronizing". */
  readonly knownGroupIdsB64: readonly string[];
}

/** Read the account's contacts, pins and the given group ids into a bundle.
 *  `ownerAccountIdB64` is the account being paired (the contacts are that
 *  account's, never another account's on this browser). */
export function buildStateBundle(
  knownGroupIdsB64: readonly string[],
  ownerAccountIdB64: string,
): StateBundleV1 {
  const contacts = loadAccepted(ownerAccountIdB64);
  const pins: CarriedPin[] = [];
  for (const c of contacts) {
    const accountIdB64 = accountIdB64OfHex(c.accountIdHex);
    if (accountIdB64 === null) continue;
    const pin = readPin(accountIdB64, ownerAccountIdB64);
    if (pin === undefined) continue;
    pins.push({
      accountIdB64,
      pinnedSignaturePubkeyB64: pin.pinnedSignaturePubkeyB64,
      rotationMarker: pin.rotationMarker,
    });
  }
  return { v: 1, contacts, pins, knownGroupIdsB64: [...knownGroupIdsB64] };
}

export function encodeStateBundle(bundle: StateBundleV1): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(bundle));
}

/** Decode a bundle, refusing anything that is not the shape above. A refused
 *  bundle is `null` — the joiner then lands without contacts, as before. */
export function decodeStateBundle(bytes: Uint8Array): StateBundleV1 | null {
  if (bytes.length === 0 || bytes.length > MAX_BUNDLE_BYTES) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const o = parsed as Record<string, unknown>;
  if (o["v"] !== 1) return null;
  if (!Array.isArray(o["contacts"]) || !Array.isArray(o["pins"])) return null;
  if (!Array.isArray(o["knownGroupIdsB64"])) return null;
  const contacts: AcceptedContact[] = [];
  for (const c of o["contacts"] as unknown[]) {
    if (typeof c !== "object" || c === null) return null;
    const r = c as Record<string, unknown>;
    if (typeof r["signaturePubkeyB64"] !== "string") return null;
    if (typeof r["displayName"] !== "string") return null;
    if (typeof r["acceptedAtMs"] !== "number") return null;
    contacts.push(c as AcceptedContact);
  }
  const pins: CarriedPin[] = [];
  for (const p of o["pins"] as unknown[]) {
    if (typeof p !== "object" || p === null) return null;
    const r = p as Record<string, unknown>;
    if (
      typeof r["accountIdB64"] !== "string" ||
      typeof r["pinnedSignaturePubkeyB64"] !== "string" ||
      typeof r["rotationMarker"] !== "number"
    ) {
      return null;
    }
    pins.push({
      accountIdB64: r["accountIdB64"],
      pinnedSignaturePubkeyB64: r["pinnedSignaturePubkeyB64"],
      rotationMarker: r["rotationMarker"],
    });
  }
  const knownGroupIdsB64: string[] = [];
  for (const g of o["knownGroupIdsB64"] as unknown[]) {
    if (typeof g !== "string") return null;
    knownGroupIdsB64.push(g);
  }
  return { v: 1, contacts, pins, knownGroupIdsB64 };
}

/** Merge a bundle into this browser, into the list of `ownerAccountIdB64`
 *  (the account the new device joined): contacts that are not here yet, pins
 *  through the monotone setter, the known group ids for the chat list. The
 *  new device may run this before its session lands, so the account is
 *  named, not taken from the binding. */
export function applyStateBundle(
  bundle: StateBundleV1,
  ownerAccountIdB64: string,
): { contacts: number; pins: number } {
  const have = new Set(loadAccepted(ownerAccountIdB64).map((c) => c.signaturePubkeyB64));
  let contacts = 0;
  for (const c of bundle.contacts) {
    if (have.has(c.signaturePubkeyB64)) continue;
    upsertAccepted(c, ownerAccountIdB64);
    contacts += 1;
  }
  let pins = 0;
  for (const p of bundle.pins) {
    try {
      if (setPin(p.accountIdB64, p.pinnedSignaturePubkeyB64, p.rotationMarker, ownerAccountIdB64) === "set") {
        pins += 1;
      }
    } catch {
      // A pin the setter refuses (a lower marker than one already held) is
      // not ours to force.
    }
  }
  saveKnownGroupIds(bundle.knownGroupIdsB64, ownerAccountIdB64);
  return { contacts, pins };
}

// ---- known group ids (drives the one "Synchronizing..." label) -------------
//
// The list belongs to the account the bundle was paired into, not to the
// browser: `lakko.pairing.known-groups.v2.<accountIdB64>`. It used to be one
// device-wide row, so a second account signed in on the same browser showed
// "Synchronizing..." for the first account's groups (2026-09-30).
//
// Which account a call means: the trailing `owner` argument, otherwise the
// signed-in account bound through `bindContactsAccount`. With no account a
// read is empty and a write is a no-op. Log out keeps the list; Forget device
// drops it ({@link clearKnownGroupIds}).

function knownGroupsOwner(owner: string | undefined): string | null {
  const account = owner ?? boundContactsAccount();
  return account === null || account.length === 0 ? null : account;
}

export function saveKnownGroupIds(idsB64: readonly string[], owner?: string): void {
  const account = knownGroupsOwner(owner);
  if (account === null) return;
  try {
    if (idsB64.length === 0) localStorage.removeItem(KNOWN_GROUPS_PREFIX + account);
    else localStorage.setItem(KNOWN_GROUPS_PREFIX + account, JSON.stringify(idsB64));
  } catch {
    /* storage unavailable: the label falls back to "no chats yet" */
  }
}

export function loadKnownGroupIds(owner?: string): string[] {
  const account = knownGroupsOwner(owner);
  if (account === null) return [];
  try {
    const raw = localStorage.getItem(KNOWN_GROUPS_PREFIX + account);
    if (raw === null) return [];
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** The known groups this device has not joined yet, for `owner` (default:
 *  the signed-in account). Clears the stored list once every known group is
 *  held, so the label cannot outlive its cause. */
export function pendingKnownGroups(joinedIdsB64: readonly string[], owner?: string): number {
  const account = knownGroupsOwner(owner);
  if (account === null) return 0;
  const known = loadKnownGroupIds(account);
  if (known.length === 0) return 0;
  const joined = new Set(joinedIdsB64);
  const pending = known.filter((id) => !joined.has(id));
  if (pending.length === 0) saveKnownGroupIds([], account);
  return pending.length;
}

/** Forget device: drop `accountIdB64`'s list and the retired device-wide
 *  one. Another account's list on this browser is left alone. */
export function clearKnownGroupIds(accountIdB64: string): void {
  try {
    localStorage.removeItem(KNOWN_GROUPS_PREFIX + accountIdB64);
    localStorage.removeItem(RETIRED_KNOWN_GROUPS_KEY);
  } catch {
    /* storage unavailable: nothing to clear */
  }
}

// ---- the tagged mailbox row ------------------------------------------------

export function wrapStateRow(sealed: Uint8Array): Uint8Array {
  const out = new Uint8Array(STATE_ROW_MAGIC.length + sealed.length);
  out.set(STATE_ROW_MAGIC, 0);
  out.set(sealed, STATE_ROW_MAGIC.length);
  return out;
}

export function isStateRow(row: Uint8Array): boolean {
  if (row.length <= STATE_ROW_MAGIC.length) return false;
  for (let i = 0; i < STATE_ROW_MAGIC.length; i += 1) {
    if (row[i] !== STATE_ROW_MAGIC[i]) return false;
  }
  return true;
}

export function unwrapStateRow(row: Uint8Array): Uint8Array | null {
  return isStateRow(row) ? row.slice(STATE_ROW_MAGIC.length) : null;
}

export async function publishStateBundle(
  rendezvousPseudonym: Uint8Array,
  sealed: Uint8Array,
  options: { ds?: DsClient } = {},
): Promise<void> {
  const ds = options.ds ?? new DsClient();
  await ds.publishRendezvous(rendezvousPseudonym, wrapStateRow(sealed));
}

export type StateBundleOutcome =
  | { readonly kind: "received"; readonly sealed: Uint8Array }
  | { readonly kind: "timeout" }
  | { readonly kind: "cancelled" };

/** Joiner side: the state row, past the Welcome cursor, skipping the history
 *  and recovery rows. Soft timeout; an older scanner never sends one. */
export async function pollForStateBundle(
  rendezvousPseudonym: Uint8Array,
  afterCursor: number,
  options: { ds?: DsClient; signal?: AbortSignal; timeoutMs?: number } = {},
): Promise<StateBundleOutcome> {
  const ds = options.ds ?? new DsClient();
  const signal = options.signal;
  const timeoutMs = options.timeoutMs ?? STATE_POLL_TIMEOUT_MS;
  const deadline = performance.now() + timeoutMs;
  let cursor = afterCursor;
  while (true) {
    if (signal?.aborted) return { kind: "cancelled" };
    const remainingMs = deadline - performance.now();
    if (remainingMs <= 0) return { kind: "timeout" };
    const waitSecs = Math.min(
      PER_REQUEST_LONG_POLL_SECS,
      Math.max(1, Math.floor(remainingMs / 1000)),
    );
    let result;
    try {
      result = await ds.fetchRendezvous(rendezvousPseudonym, { after: cursor, limit: 1, waitSecs });
    } catch (err) {
      if (err instanceof DsHttpError && err.status === 404) continue;
      if (err instanceof DsNetworkError) continue;
      throw err;
    }
    if (result.messages.length > 0) {
      const first = result.messages[0];
      if (first === undefined) continue;
      const sealed = unwrapStateRow(first.payload);
      if (sealed !== null) return { kind: "received", sealed };
      cursor = first.id;
      continue;
    }
    cursor = result.messages.reduce((acc, m) => Math.max(acc, m.id), cursor);
  }
}

function accountIdB64OfHex(hex: string | undefined): string | null {
  if (hex === undefined || hex.length !== 32 || !/^[0-9a-f]+$/i.test(hex)) return null;
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return base64Encode(out);
}
