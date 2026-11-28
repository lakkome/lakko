// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Publishing and reading the account's own seal announcement (ADR-0101
 * section 10), behind ports so the rules can be tested without a network.
 *
 * - {@link readOwnSeals}: at every session landing on a device with the
 *   keys. Opens what the directory serves under the account's own key,
 *   records what it lists for the page (`own-seals.ts`) and keeps the bytes
 *   (they are what a later announcement carries entries over from).
 * - {@link publishSeals}: after anything that alters the announcement on a
 *   device with the keys (sign-up, a password sign-in or change that ran
 *   here, a passkey added or removed, a phrase saved, a rotation, a reset).
 *
 * What is kept per account in plain storage is public: the last
 * announcement this device opened under the account's key or signed itself
 * (standard base64). It is the `seq` floor too: this device never signs at
 * or below a `seq` it has seen. No seal secret is ever kept.
 */
import type { Wasm } from "../../wasm";
import type { Identity as WasmIdentity } from "../../../wasm/lakko_ffi_wasm";
import type { MarkerStorage } from "../phrase-gate";
import { adoptOwnSealFacts } from "./own-seals";
import {
  nextSealEntries,
  nextSealSeq,
  readSeals,
  sameSealEntries,
  sealFactsOf,
  signSeals,
  type OpenedSeals,
  type SealChanges,
} from "./seal-announcement";

/** The account's own row at the directory, as far as the seals go. */
export interface OwnBundleSeals {
  /** The key the directory publishes for the account. */
  readonly signaturePubkey: Uint8Array;
  readonly keyGeneration: number;
  readonly resetSeals: Uint8Array | null;
  readonly previousResetSeals: Uint8Array | null;
}

export interface SealPorts {
  /** `GET /discovery/identity/{own id}`; `null` when the account has no
   *  row. Throws when the directory could not be reached. */
  readonly fetchOwn: (accountId: Uint8Array) => Promise<OwnBundleSeals | null>;
  /** A same-key identity upload carrying `resetSeals`. Throws with
   *  `status` / `errorCode` on a refusal. */
  readonly upload: (accountId: Uint8Array, identity: WasmIdentity, resetSeals: Uint8Array) => Promise<void>;
  readonly storage: MarkerStorage | undefined;
  readonly nowMs: () => number;
}

function keptKey(accountIdB64: string): string {
  return `lakko:seal-announcement:${accountIdB64}`;
}

function b64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

function unb64(text: string): Uint8Array | null {
  try {
    const raw = atob(text);
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i += 1) out[i] = raw.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/** The last announcement this device opened or signed for the account. */
export function keptAnnouncement(storage: MarkerStorage | undefined, accountIdB64: string): Uint8Array | null {
  try {
    const raw = storage?.getItem(keptKey(accountIdB64)) ?? null;
    return raw === null ? null : unb64(raw);
  } catch {
    return null;
  }
}

function keep(storage: MarkerStorage | undefined, accountIdB64: string, announcement: Uint8Array): void {
  try {
    storage?.setItem(keptKey(accountIdB64), b64(announcement));
  } catch {
    // Storage blocked: the floor is re-read from the directory next time.
  }
}

/** Forget what was kept for the account (Forget device). */
export function clearSealFacts(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(keptKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}

/** The `seq` of a kept announcement, read without a key (it is only ever a
 *  floor, never a source of entries). */
function keptSeq(wasm: Wasm, kept: Uint8Array | null): bigint | null {
  if (kept === null) return null;
  try {
    const decoded = wasm.decodeSealAnnouncement(kept);
    try {
      return decoded.seq;
    } finally {
      decoded.free();
    }
  } catch {
    return null;
  }
}

/** The `seq` of the announcement this device kept for the account, or
 *  `null` (none kept, or not decodable). A floor only. */
export function keptAnnouncementSeq(
  wasm: Wasm,
  storage: MarkerStorage | undefined,
  accountIdB64: string,
): bigint | null {
  return keptSeq(wasm, keptAnnouncement(storage, accountIdB64));
}

function seqOfUnverified(wasm: Wasm, bytes: Uint8Array | null): bigint | null {
  return keptSeq(wasm, bytes);
}

interface OwnAccount {
  readonly accountIdB64: string;
  readonly accountId: Uint8Array;
  /** The account identity of the CURRENT key. */
  readonly identity: WasmIdentity;
  /** The generation of this device's session. */
  readonly keyGeneration: bigint;
}

/**
 * Read the account's own announcement and record what it lists. Resolves
 * what was opened, or `null` when the directory serves none that opens under
 * this account's key (then the page shows nothing: unknown is not "password
 * only"). Never throws.
 */
export async function readOwnSeals(
  wasm: Wasm,
  ports: SealPorts,
  account: OwnAccount,
): Promise<OpenedSeals | null> {
  let bundle: OwnBundleSeals | null;
  try {
    bundle = await ports.fetchOwn(account.accountId);
  } catch {
    return null;
  }
  if (bundle === null || bundle.resetSeals === null) {
    adoptOwnSealFacts(account.accountIdB64, null);
    return null;
  }
  const opened = readSeals(wasm, bundle.resetSeals, account.identity.signaturePublicKey, account.accountId);
  if (opened === null) {
    adoptOwnSealFacts(account.accountIdB64, null);
    return null;
  }
  const floor = keptSeq(wasm, keptAnnouncement(ports.storage, account.accountIdB64));
  if (floor === null || opened.seq >= floor) keep(ports.storage, account.accountIdB64, bundle.resetSeals);
  adoptOwnSealFacts(account.accountIdB64, sealFactsOf(opened));
  return opened;
}

export type PublishOutcome =
  /** The directory took the announcement. */
  | "published"
  /** What is published already says exactly this: nothing was sent. */
  | "unchanged"
  /** Nothing could be listed for the password; nothing was sent. */
  | "no-password"
  /** The account already lists eight passkeys that keep the chats. */
  | "passkeys-full"
  /** The directory could not be reached or refused: try again later. */
  | "retry";

/**
 * Publish the account's announcement with `changes` applied.
 *
 * The entries are carried over from the directory's current announcement
 * when it opens under this account's key; when it does not (a rotation or a
 * reset just changed the key) and `carryFromKept` is set, from the
 * announcement this device kept, opened under `keptUnderKey` (the account's
 * key before the change). Nothing is ever carried from bytes that did not
 * open. A `409` (another device published meanwhile) is read again and
 * retried once. Never throws.
 */
export async function publishSeals(
  wasm: Wasm,
  ports: SealPorts,
  account: OwnAccount,
  changes: SealChanges,
  options: { readonly carryFromKept?: { readonly keptUnderKey: Uint8Array } } = {},
): Promise<PublishOutcome> {
  for (let attempt = 0; attempt < 2; attempt += 1) {
    let bundle: OwnBundleSeals | null;
    try {
      bundle = await ports.fetchOwn(account.accountId);
    } catch {
      return "retry";
    }
    const ownKey = account.identity.signaturePublicKey;
    const kept = keptAnnouncement(ports.storage, account.accountIdB64);
    const current =
      bundle?.resetSeals != null ? readSeals(wasm, bundle.resetSeals, ownKey, account.accountId) : null;
    let base = current?.entries ?? null;
    if (base === null && options.carryFromKept !== undefined && kept !== null) {
      base =
        readSeals(wasm, kept, options.carryFromKept.keptUnderKey, account.accountId)?.entries ?? null;
    }
    const next = nextSealEntries(wasm, base, changes);
    if (next.kind !== "entries") return next.kind;
    if (
      current !== null &&
      current.keyGeneration === account.keyGeneration &&
      sameSealEntries(current.entries, next.entries)
    ) {
      adoptOwnSealFacts(account.accountIdB64, sealFactsOf(current));
      return "unchanged";
    }
    const seq = nextSealSeq([
      seqOfUnverified(wasm, bundle?.resetSeals ?? null),
      seqOfUnverified(wasm, bundle?.previousResetSeals ?? null),
      keptSeq(wasm, kept),
    ]);
    let announcement: Uint8Array;
    try {
      announcement = signSeals(wasm, {
        identity: account.identity,
        accountId: account.accountId,
        keyGeneration: account.keyGeneration,
        seq,
        nowMs: ports.nowMs(),
        entries: next.entries,
      });
    } catch {
      return "retry";
    }
    try {
      await ports.upload(account.accountId, account.identity, announcement);
    } catch (err) {
      const status = (err as { status?: unknown }).status;
      // Another device of the account published first: read again, once.
      if (status === 409 && attempt === 0) continue;
      return "retry";
    }
    keep(ports.storage, account.accountIdB64, announcement);
    adoptOwnSealFacts(
      account.accountIdB64,
      sealFactsOf({ keyGeneration: account.keyGeneration, seq, entries: next.entries }),
    );
    return "published";
  }
  return "retry";
}
