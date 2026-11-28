// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The account's seal announcement (ADR-0101 section 10): what this device
 * reads from one and how it builds the next one. Pure over the wasm core;
 * the network and the stores are `seal-runtime.ts`'s.
 *
 * An announcement lists the seal keys that may sign a reset of the account:
 * exactly one from the password, at most one from the recovery phrase, and
 * one per passkey that keeps the chats (at most eight). A device can derive
 * a seal key only from a secret it holds at that moment (the export key of a
 * password sign-in, a passkey's `prf` output, the phrase), so the next
 * announcement is the current one with the entries this device can speak
 * for replaced, and every other entry carried over by its public key.
 *
 * Carrying an entry over is safe only from an announcement that was opened
 * under the account's own key: `readSeals` does that, and nothing here
 * accepts an entry from bytes that were not opened.
 */
import type { Wasm } from "../../wasm";
import type { Identity as WasmIdentity, ResetSealKey } from "../../../wasm/lakko_ffi_wasm";
import type { OwnSealFacts } from "./own-seals";

export type SealKindName = "password" | "passkey" | "phrase";

/** One entry of an announcement. */
export interface SealEntry {
  readonly kind: SealKindName;
  /** Lower-case hex of the 16-byte tag (all zero for password and phrase). */
  readonly tagHex: string;
  /** The 32-byte seal public key. */
  readonly sealPk: Uint8Array;
}

/** An announcement opened under the account key. */
export interface OpenedSeals {
  readonly keyGeneration: bigint;
  readonly seq: bigint;
  readonly entries: readonly SealEntry[];
}

export const MAX_PASSKEY_SEALS = 8;
const ZERO_TAG_HEX = "00".repeat(16);

export function hex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function unhex(text: string): Uint8Array {
  const out = new Uint8Array(text.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(text.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

/**
 * Open `announcement` under `accountKey` for the account `accountId`.
 * `null` when it does not open (a signature by another key, a damaged blob)
 * or names another account: such bytes are never read further.
 */
export function readSeals(
  wasm: Wasm,
  announcement: Uint8Array,
  accountKey: Uint8Array,
  accountId: Uint8Array,
): OpenedSeals | null {
  let opened;
  try {
    opened = wasm.openSealAnnouncement(announcement, accountKey);
  } catch {
    return null;
  }
  try {
    if (!sameBytes(opened.accountRef, wasm.keyResetAccountRef(accountId))) return null;
    const kinds = opened.kinds as SealKindName[];
    const tags = opened.tags;
    const pks = opened.sealPks;
    const entries: SealEntry[] = kinds.map((kind, i) => ({
      kind,
      tagHex: hex(tags.slice(i * 16, (i + 1) * 16)),
      sealPk: pks.slice(i * 32, (i + 1) * 32),
    }));
    return { keyGeneration: opened.keyGeneration, seq: opened.seq, entries };
  } finally {
    opened.free();
  }
}

/** What the page shows for an opened announcement. */
export function sealFactsOf(seals: OpenedSeals): OwnSealFacts {
  return {
    password: seals.entries.some((e) => e.kind === "password"),
    phrase: seals.entries.some((e) => e.kind === "phrase"),
    passkeyTagsHex: seals.entries.filter((e) => e.kind === "passkey").map((e) => e.tagHex),
    seq: seals.seq.toString(),
  };
}

/** What this device can speak for right now. Every byte string is a secret
 *  the caller holds only for the call. */
export interface SealChanges {
  /** The 64-byte export key of a password registration or sign-in that ran
   *  just now on this device: the password entry becomes this password's. */
  readonly passwordExportKey?: Uint8Array;
  /** The recovery phrase this device holds: a phrase entry is listed. */
  readonly savePhrase?: string;
  /** The phrase this device holds, to REPLACE a listed phrase entry (after
   *  a rotation gave the account a new phrase). Adds none. */
  readonly rekeyPhrase?: string;
  /** A passkey that gave a `prf` output at the seal salt. */
  readonly addPasskey?: { readonly credentialId: Uint8Array; readonly prfOutput: Uint8Array };
  /** Lower-case hex of the tag of a passkey that was deleted. */
  readonly removePasskeyTagHex?: string;
  /** After a key reset: the passkeys were deleted and the new phrase is not
   *  saved, so only the password entry is listed. */
  readonly afterReset?: boolean;
}

export type NextEntries =
  | { readonly kind: "entries"; readonly entries: readonly SealEntry[] }
  /** No password entry can be listed: this device holds no export key and
   *  the current announcement gave none to carry. Nothing is published. */
  | { readonly kind: "no-password" }
  /** The account already lists eight passkeys that keep the chats. */
  | { readonly kind: "passkeys-full" };

/**
 * The entries of the next announcement: `base` (the entries of an
 * announcement opened under the account's own key, or `null`) with
 * `changes` applied.
 */
export function nextSealEntries(
  wasm: Wasm,
  base: readonly SealEntry[] | null,
  changes: SealChanges,
): NextEntries {
  const carried = changes.afterReset === true ? [] : [...(base ?? [])];
  let password = carried.find((e) => e.kind === "password") ?? null;
  let phrase = carried.find((e) => e.kind === "phrase") ?? null;
  let passkeys = carried.filter((e) => e.kind === "passkey");

  if (changes.passwordExportKey !== undefined) {
    const key = wasm.ResetSealKey.fromPasswordExportKey(changes.passwordExportKey);
    try {
      password = { kind: "password", tagHex: ZERO_TAG_HEX, sealPk: key.publicKey };
    } finally {
      key.free();
    }
  }
  const phraseText =
    changes.savePhrase ?? (phrase !== null && changes.rekeyPhrase !== undefined ? changes.rekeyPhrase : undefined);
  if (phraseText !== undefined) {
    const mnemonic = wasm.RecoveryMnemonic.fromPhrase(phraseText);
    try {
      const key = wasm.ResetSealKey.fromMnemonic(mnemonic);
      try {
        phrase = { kind: "phrase", tagHex: ZERO_TAG_HEX, sealPk: key.publicKey };
      } finally {
        key.free();
      }
    } finally {
      mnemonic.free();
    }
  }
  if (changes.removePasskeyTagHex !== undefined) {
    passkeys = passkeys.filter((e) => e.tagHex !== changes.removePasskeyTagHex);
  }
  if (changes.addPasskey !== undefined) {
    const tagHex = hex(wasm.resetSealPasskeyTag(changes.addPasskey.credentialId));
    const key = wasm.ResetSealKey.fromPasskeyPrf(changes.addPasskey.prfOutput);
    try {
      passkeys = passkeys.filter((e) => e.tagHex !== tagHex);
      if (passkeys.length >= MAX_PASSKEY_SEALS) return { kind: "passkeys-full" };
      passkeys.push({ kind: "passkey", tagHex, sealPk: key.publicKey });
    } finally {
      key.free();
    }
  }
  if (password === null) return { kind: "no-password" };
  const entries: SealEntry[] = [password, ...passkeys];
  if (phrase !== null) entries.push(phrase);
  return { kind: "entries", entries };
}

/** Whether two entry lists announce the same thing (order aside). */
export function sameSealEntries(a: readonly SealEntry[], b: readonly SealEntry[]): boolean {
  if (a.length !== b.length) return false;
  const key = (e: SealEntry): string => `${e.kind}:${e.tagHex}:${hex(e.sealPk)}`;
  const left = a.map(key).sort();
  const right = b.map(key).sort();
  return left.every((k, i) => k === right[i]);
}

/** The next `seq`: above every one this device knows of (the current and
 *  the previous generation's announcement, and what it kept itself). */
export function nextSealSeq(known: readonly (bigint | null)[]): bigint {
  let top = 0n;
  for (const s of known) if (s !== null && s > top) top = s;
  return top + 1n;
}

/** Sign the announcement of `entries` with the account identity. The core
 *  orders the entries and refuses a malformed list (it throws). */
export function signSeals(
  wasm: Wasm,
  args: {
    readonly identity: WasmIdentity;
    readonly accountId: Uint8Array;
    readonly keyGeneration: bigint;
    readonly seq: bigint;
    readonly nowMs: number;
    readonly entries: readonly SealEntry[];
  },
): Uint8Array {
  // The core's canonical order: by kind (password, passkey, phrase), then tag.
  const rank: Record<SealKindName, number> = { password: 1, passkey: 2, phrase: 3 };
  const ordered = [...args.entries].sort((a, b) =>
    rank[a.kind] !== rank[b.kind] ? rank[a.kind] - rank[b.kind] : a.tagHex < b.tagHex ? -1 : a.tagHex > b.tagHex ? 1 : 0,
  );
  const tags = new Uint8Array(ordered.length * 16);
  const pks = new Uint8Array(ordered.length * 32);
  ordered.forEach((e, i) => {
    tags.set(unhex(e.tagHex), i * 16);
    pks.set(e.sealPk, i * 32);
  });
  return wasm.signSealAnnouncement(
    args.identity,
    args.accountId,
    args.keyGeneration,
    args.seq,
    BigInt(Math.max(0, Math.floor(args.nowMs))),
    ordered.map((e) => e.kind),
    tags,
    pks,
  );
}

// ---------------------------------------------------------------------------
// The reset seal
// ---------------------------------------------------------------------------

/** A secret the resetting device holds that may seal its reset. */
export type SealSecret =
  | { readonly kind: "password"; readonly exportKey: Uint8Array }
  | { readonly kind: "passkey"; readonly prfOutput: Uint8Array; readonly credentialId: Uint8Array }
  | { readonly kind: "phrase"; readonly phrase: string };

function withSealKey<T>(wasm: Wasm, secret: SealSecret, run: (key: ResetSealKey) => T): T {
  if (secret.kind === "phrase") {
    const mnemonic = wasm.RecoveryMnemonic.fromPhrase(secret.phrase);
    try {
      const key = wasm.ResetSealKey.fromMnemonic(mnemonic);
      try {
        return run(key);
      } finally {
        key.free();
      }
    } finally {
      mnemonic.free();
    }
  }
  const key =
    secret.kind === "password"
      ? wasm.ResetSealKey.fromPasswordExportKey(secret.exportKey)
      : wasm.ResetSealKey.fromPasskeyPrf(secret.prfOutput);
  try {
    return run(key);
  } finally {
    key.free();
  }
}

/** Whether `secret`'s seal key is listed in `seals` under its kind (and, for
 *  a passkey, under that passkey's tag). */
export function sealSecretListed(wasm: Wasm, seals: OpenedSeals, secret: SealSecret): boolean {
  return withSealKey(wasm, secret, (key) => {
    const pk = key.publicKey;
    const tagHex =
      secret.kind === "passkey" ? hex(wasm.resetSealPasskeyTag(secret.credentialId)) : ZERO_TAG_HEX;
    return seals.entries.some((e) => e.kind === secret.kind && e.tagHex === tagHex && sameBytes(e.sealPk, pk));
  });
}

/**
 * Which of the secrets this device holds seals a reset of the account whose
 * current announcement is `seals` (UX 15.1c): the first one listed, the
 * password before a passkey before the phrase. `null`: the reset is
 * unsealed.
 */
export function chooseSealSecret(
  wasm: Wasm,
  seals: OpenedSeals | null,
  held: readonly SealSecret[],
): SealSecret | null {
  if (seals === null) return null;
  const order: Record<SealSecret["kind"], number> = { password: 0, passkey: 1, phrase: 2 };
  for (const secret of [...held].sort((a, b) => order[a.kind] - order[b.kind])) {
    try {
      if (sealSecretListed(wasm, seals, secret)) return secret;
    } catch {
      // A malformed secret seals nothing.
    }
  }
  return null;
}

/** What the reset screen says before the word is typed (UX 15.1c). */
export interface ResetOutlook {
  /** The reset will be sealed by a secret this device holds. */
  readonly sealed: boolean;
  /** Unsealed, and the account lists a passkey that keeps the chats which
   *  was not the confirmation used: `Use passkey` is offered. */
  readonly passkeyCouldSeal: boolean;
}

export function resetOutlook(
  wasm: Wasm,
  seals: OpenedSeals | null,
  held: readonly SealSecret[],
): ResetOutlook {
  const sealed = chooseSealSecret(wasm, seals, held) !== null;
  const passkeyListed = seals !== null && seals.entries.some((e) => e.kind === "passkey");
  const passkeyTried = held.some((s) => s.kind === "passkey");
  return { sealed, passkeyCouldSeal: !sealed && passkeyListed && !passkeyTried };
}

/**
 * Sign the reset seal over the next generation and the new key
 * (`from = keyGeneration`, `to = keyGeneration + 1`).
 */
export function signResetSeal(
  wasm: Wasm,
  args: {
    readonly secret: SealSecret;
    readonly accountId: Uint8Array;
    readonly announcementSeq: bigint;
    readonly fromGeneration: bigint;
    readonly newSignaturePubkey: Uint8Array;
  },
): Uint8Array {
  return withSealKey(wasm, args.secret, (key) =>
    key.signResetSeal(args.accountId, args.announcementSeq, args.fromGeneration, args.newSignaturePubkey),
  );
}
