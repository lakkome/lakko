// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What `GET /recovery-email` reports about the signed-in account and what
 * Settings > Account shows for it (ADR-0075 as amended 2026-11-26,
 * `docs/ux/identity-recovery.md` 19.1 and 19.1c). Pure; the stores at the end
 * hold two per-device facts in plain storage (neither a secret, neither the
 * address).
 *
 * There is one kind of account since the email vault was removed: a mailed
 * code gives a new password and confirms a key reset, never the keys.
 * Whether a recovery phrase is saved is no server flag any more: it is what
 * the account's published protection lists (`seal/own-seals.ts`).
 */
import type { MarkerStorage } from "../phrase-gate";
import type { ChatsKeptBy } from "../seal/own-seals";

/** What `GET /recovery-email` says about the session's account. */
export interface RecoveryEmailStatus {
  /** This server can send a recovery code. */
  readonly available: boolean;
  /** The account has a recovery email. Never the address. */
  readonly enabled: boolean;
  /** The account has a confirmed authenticator: its code is asked for as
   *  well as the mailed one, and it is one of the account's second ways in.
   *  `false` when the server did not say. */
  readonly totp: boolean;
}

/**
 * The catalog key of the Settings > Account status line (UX 19.1c): what
 * keeps the account's chats through a key reset. `null` while that is not
 * known: no line is drawn rather than a wrong one.
 */
export function chatsKeptStatusKey(keptBy: ChatsKeptBy | null): string | null {
  switch (keptBy) {
    case "phrase":
      return "protect.status.phrase";
    case "passkey":
      return "protect.status.passkey_seal";
    case "password":
      return "protect.status.password_only";
    case null:
      return null;
  }
}

/** Read a `GET /recovery-email` body leniently; `null` when it is not one. */
export function parseRecoveryEmailStatus(body: unknown): RecoveryEmailStatus | null {
  if (typeof body !== "object" || body === null) return null;
  const b = body as Record<string, unknown>;
  if (typeof b["available"] !== "boolean" || typeof b["enabled"] !== "boolean") return null;
  return { available: b["available"], enabled: b["enabled"], totp: b["totp"] === true };
}

// ---------------------------------------------------------------------------
// Settings > Account: the two rows
// ---------------------------------------------------------------------------

/** What the `Recovery email` row offers. */
export type EmailRowAction = "add" | "replace" | "remove" | "choose-username";

export interface ProtectionRows {
  /** The `Recovery email` row. Absent when mail is not set up on this
   *  server and the account has no email (UX 19.2, "Mail is not set up"). */
  readonly email: {
    readonly on: boolean;
    readonly actions: readonly EmailRowAction[];
  } | null;
  /** The `Recovery phrase` row: `show` once the phrase was saved, `save`
   *  for every other account. */
  readonly phrase: "show" | "save";
}

/**
 * The rows for a status. `hasUsername` is `false` for a guest account, which
 * must choose a username before it can add an email (the stored value is
 * made from the username). `phraseSaved` is what the account's published
 * protection lists.
 */
export function protectionRows(
  status: RecoveryEmailStatus,
  hasUsername: boolean,
  phraseSaved: boolean,
): ProtectionRows {
  const phrase = phraseSaved ? "show" : "save";
  if (!status.available && !status.enabled) return { email: null, phrase };
  if (!hasUsername) return { email: { on: false, actions: ["choose-username"] }, phrase };
  if (!status.enabled) {
    return { email: { on: false, actions: status.available ? ["add"] : [] }, phrase };
  }
  const actions: EmailRowAction[] = [];
  if (status.available) actions.push("replace");
  actions.push("remove");
  return { email: { on: true, actions }, phrase };
}

/**
 * Whether a "three words from your phrase" surface is shown (ADR-0075
 * section 11): only on a device of a phrase-protected account. The other
 * conditions (the device holds the phrase and the keys) are
 * `recover/three-words.ts`'s.
 */
export function threeWordsShown(phraseProtectedOnDevice: boolean): boolean {
  return phraseProtectedOnDevice;
}

/**
 * Whether NEW recovery words are shown after the account's keys changed on
 * this device (a rotation that removed a device, a key reset, a recall):
 * only to a phrase-protected account. The owner of any other account was
 * never shown words, so none are shown now: the change completes silently as
 * far as the phrase goes. "Phrase-protected" is what the account's published
 * protection listed BEFORE the change (the per-device flag, which is not
 * cleared while new words are owed). A guest account (no username) is never
 * shown any.
 */
export function newWordsShown(phraseProtectedOnDevice: boolean, hasUsername: boolean): boolean {
  return phraseProtectedOnDevice && hasUsername;
}

/**
 * What the email form starts with. Adding (the account has no recovery
 * email at the server, as after a recall) pre-fills the address this device
 * holds; replacing starts empty, since a new address is being typed.
 */
export function emailFormPrefill(action: EmailRowAction, heldAddress: string | null): string {
  return action === "add" ? (heldAddress ?? "") : "";
}

/** Whether `Show phrase` exists in Settings: a phrase-protected account
 *  whose phrase this device holds. */
export function showPhraseOffered(phraseProtectedOnDevice: boolean, phraseOnDevice: boolean): boolean {
  return phraseProtectedOnDevice && phraseOnDevice;
}

// ---------------------------------------------------------------------------
// Per-device facts
// ---------------------------------------------------------------------------

function flagKey(accountIdB64: string): string {
  return `lakko:phrase-protected:${accountIdB64}`;
}

function stepKey(accountIdB64: string): string {
  return `lakko:protect-step-owed:${accountIdB64}`;
}

/** Record whether the account's recovery phrase is saved, as this device
 *  last learned it: the phrase was saved here, or the account's published
 *  protection said so at a landing (`protection-state.ts`). Kept in plain
 *  storage so the three-word surfaces work while signed out. */
export function setPhraseProtected(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  value: boolean,
): void {
  try {
    if (value) storage?.setItem(flagKey(accountIdB64), "1");
    else storage?.removeItem(flagKey(accountIdB64));
  } catch {
    // Storage blocked: the fact is asked again at the next landing.
  }
}

export function isPhraseProtected(storage: MarkerStorage | undefined, accountIdB64: string): boolean {
  try {
    return storage?.getItem(flagKey(accountIdB64)) === "1";
  } catch {
    return false;
  }
}

/** The sign-up's "Protect your account" step is owed (UX 19.1a): shown
 *  before any route until the person chose, and again after a reload. */
export function markProtectStepOwed(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.setItem(stepKey(accountIdB64), "1");
  } catch {
    // Storage blocked: the in-memory flag still holds for this page.
  }
}

export function isProtectStepOwed(storage: MarkerStorage | undefined, accountIdB64: string): boolean {
  try {
    return storage?.getItem(stepKey(accountIdB64)) === "1";
  } catch {
    return false;
  }
}

export function clearProtectStepOwed(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(stepKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}

function sealsLookedKey(accountIdB64: string): string {
  return `lakko:protection-looked:${accountIdB64}`;
}

/** This device read the account's published protection at least once
 *  (`notices.ts`, `phraseSavedElsewhere`: a device that never looked is
 *  new and is told nothing about a phrase saved before it existed). */
export function markProtectionLooked(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.setItem(sealsLookedKey(accountIdB64), "1");
  } catch {
    // Storage blocked: the device stays "new" and is told nothing.
  }
}

export function protectionLookedBefore(storage: MarkerStorage | undefined, accountIdB64: string): boolean {
  try {
    return storage?.getItem(sealsLookedKey(accountIdB64)) === "1";
  } catch {
    return false;
  }
}

function passwordOnlyDialogKey(accountIdB64: string): string {
  return `lakko:password-only-dialog-seen:${accountIdB64}`;
}

/** The "Your chats depend on your password" dialog (UX 19.1d, place 3) was
 *  shown on this device, or is not to be shown (the sign-up step or a key
 *  reset on this device already said it): it never opens here again. */
export function markPasswordOnlyDialogSeen(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.setItem(passwordOnlyDialogKey(accountIdB64), "1");
  } catch {
    // Storage blocked: the dialog may be shown once more after a reload.
  }
}

export function passwordOnlyDialogSeen(storage: MarkerStorage | undefined, accountIdB64: string): boolean {
  try {
    return storage?.getItem(passwordOnlyDialogKey(accountIdB64)) === "1";
  } catch {
    // Storage blocked: never nag on every load.
    return true;
  }
}

/** Forget every per-device fact of the account (sign-out wipe). */
export function clearProtectionFacts(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(flagKey(accountIdB64));
    storage?.removeItem(stepKey(accountIdB64));
    storage?.removeItem(passwordOnlyDialogKey(accountIdB64));
    storage?.removeItem(sealsLookedKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}
