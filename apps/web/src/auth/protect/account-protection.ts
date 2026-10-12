// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * How an account is protected against a forgotten password or a lost device
 * (ADR-0067 section 1, `docs/ux/identity-recovery.md` 19.1), decided from
 * the three facts `GET /recovery-email` reports, and what Settings > Account
 * shows for it. Pure; the stores at the end hold three per-device facts in
 * plain storage (none of them a secret, none of them the address).
 */
import type { MarkerStorage } from "../phrase-gate";

/** What `GET /recovery-email` says about the session's account. */
export interface RecoveryEmailStatus {
  /** This server can send a recovery code. */
  readonly available: boolean;
  /** The account has a recovery email. Never the address. */
  readonly enabled: boolean;
  /** The account has an email vault: a mailed code brings its keys back. */
  readonly vault: boolean;
  /** The owner saved a recovery phrase. */
  readonly phraseSaved: boolean;
  /** The account has an authenticator: its code is asked for as well as the
   *  mailed one. `false` when the server did not say. */
  readonly totp: boolean;
}

/** The three kinds of account of ADR-0067 section 1. */
export type AccountProtection = "email" | "phrase" | "none";

/**
 * The account's mode. A saved phrase wins: from then on the mailed code
 * changes the password only. Without one, the email protects the account
 * only while its vault exists: after a key reset or a recall the server
 * drops the vault, and the account is unprotected until the email is added
 * again (which seals the new phrase).
 */
export function accountProtection(
  enabled: boolean,
  vault: boolean,
  phraseSaved: boolean,
): AccountProtection {
  if (phraseSaved) return "phrase";
  return enabled && vault ? "email" : "none";
}

/** The mode of a status answer. */
export function protectionOf(status: RecoveryEmailStatus): AccountProtection {
  return accountProtection(status.enabled, status.vault, status.phraseSaved);
}

/** The catalog key of the Settings > Account status line. */
export function protectionStatusKey(mode: AccountProtection): string {
  switch (mode) {
    case "email":
      return "protect.status.email";
    case "phrase":
      return "protect.status.phrase";
    case "none":
      return "protect.status.none";
  }
}

/** Read a `GET /recovery-email` body leniently; `null` when it is not one. */
export function parseRecoveryEmailStatus(body: unknown): RecoveryEmailStatus | null {
  if (typeof body !== "object" || body === null) return null;
  const b = body as Record<string, unknown>;
  if (
    typeof b["available"] !== "boolean" ||
    typeof b["enabled"] !== "boolean" ||
    typeof b["vault"] !== "boolean" ||
    typeof b["phrase_saved"] !== "boolean"
  ) {
    return null;
  }
  return {
    available: b["available"],
    enabled: b["enabled"],
    vault: b["vault"],
    phraseSaved: b["phrase_saved"],
    totp: b["totp"] === true,
  };
}

// ---------------------------------------------------------------------------
// Settings > Account: the two rows and the banner
// ---------------------------------------------------------------------------

/** What the `Recovery email` row offers. */
export type EmailRowAction = "add" | "addAgain" | "replace" | "remove" | "choose-username";

export interface ProtectionRows {
  /** The `Recovery email` row. Absent when mail is not set up on this
   *  server and the account has no email (UX 19.2, "Mail is not set up"). */
  readonly email: {
    readonly on: boolean;
    readonly actions: readonly EmailRowAction[];
  } | null;
  /** The `Recovery phrase` row: `show` for a phrase-protected account,
   *  `save` for every other one. */
  readonly phrase: "show" | "save";
}

/**
 * The rows for a status. `hasUsername` is `false` for a guest account, which
 * must choose a username before it can add an email (the stored value is
 * made from the username).
 */
export function protectionRows(status: RecoveryEmailStatus, hasUsername: boolean): ProtectionRows {
  const phrase = status.phraseSaved ? "show" : "save";
  if (!status.available && !status.enabled) return { email: null, phrase };
  if (!hasUsername) return { email: { on: false, actions: ["choose-username"] }, phrase };
  if (!status.enabled) {
    return { email: { on: false, actions: status.available ? ["add"] : [] }, phrase };
  }
  const actions: EmailRowAction[] = [];
  if (status.available) {
    // The email is on but its vault is gone (a key reset or a recall, with
    // no phrase saved): adding it again seals the new phrase.
    if (!status.vault && !status.phraseSaved) actions.push("addAgain");
    else actions.push("replace");
  }
  actions.push("remove");
  return { email: { on: true, actions }, phrase };
}

/** The banner returns this long after it was dismissed (UX 19.1a). */
export const PROTECT_BANNER_RETURN_MS = 30 * 24 * 60 * 60 * 1000;

/**
 * Whether "This account has no way back in." is shown: an unprotected
 * account that has no passkey, never dismissed or dismissed at least 30 days
 * ago. A passkey is a way back in, so an account with at least one is never
 * shown it (nor its 30-day return). `hasPasskey` is `null` while the passkey
 * list is not known, which keeps the banner away rather than flashing it. A
 * clock that went backwards keeps it hidden.
 */
export function protectBannerShown(
  mode: AccountProtection,
  dismissedAtMs: number | null,
  nowMs: number,
  hasPasskey: boolean | null,
): boolean {
  if (mode !== "none") return false;
  if (hasPasskey !== false) return false;
  if (dismissedAtMs === null) return true;
  return nowMs - dismissedAtMs >= PROTECT_BANNER_RETURN_MS;
}

/**
 * The text of the publish-owed / retry screen after a key reset or a
 * take-back: the existing one for a phrase-protected account (it speaks of
 * the new words), and one with no mention of words for any other.
 */
export function publishOwedTextKey(wordsShown: boolean): string {
  return wordsShown ? "recovery_phrase.fresh.publish_owed" : "account.keys_changed.finishing";
}

/**
 * Whether a "three words from your phrase" surface is shown (ADR-0067
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
 * far as the phrase goes, and the account reads "Not protected" until the
 * email is added again. A guest account (no username) is never shown any.
 */
export function newWordsShown(phraseProtectedOnDevice: boolean, hasUsername: boolean): boolean {
  return phraseProtectedOnDevice && hasUsername;
}

/** The email is on but its vault is gone and no phrase is saved: the row
 *  and the banner ask for the address again. */
export function emailNeedsAddingAgain(status: RecoveryEmailStatus): boolean {
  return status.enabled && !status.vault && !status.phraseSaved;
}

/** The banner's two lines: the add-again state, or the ordinary one (which
 *  has no second line). */
export function protectBannerKeys(status: RecoveryEmailStatus): {
  readonly titleKey: string;
  readonly bodyKey: string | null;
} {
  return emailNeedsAddingAgain(status)
    ? { titleKey: "recovery_email.add_again.title", bodyKey: "recovery_email.add_again.body" }
    : { titleKey: "protect.banner.none", bodyKey: null };
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

function bannerKey(accountIdB64: string): string {
  return `lakko:protect-banner-dismissed:${accountIdB64}`;
}

function stepKey(accountIdB64: string): string {
  return `lakko:protect-step-owed:${accountIdB64}`;
}

/** Record whether the account is phrase-protected, as this device last
 *  learned it (the phrase was saved here, or `GET /recovery-email` said so
 *  at a landing). */
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

/** When the no-protection banner was dismissed, unix ms, or `null`. */
export function bannerDismissedAt(storage: MarkerStorage | undefined, accountIdB64: string): number | null {
  try {
    const raw = storage?.getItem(bannerKey(accountIdB64)) ?? null;
    if (raw === null) return null;
    const ms = Number(raw);
    return Number.isFinite(ms) ? ms : null;
  } catch {
    return null;
  }
}

export function dismissBanner(storage: MarkerStorage | undefined, accountIdB64: string, nowMs: number): void {
  try {
    storage?.setItem(bannerKey(accountIdB64), String(nowMs));
  } catch {
    // Storage blocked: the banner is back after a reload.
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

/** Forget every per-device fact of the account (sign-out wipe). */
export function clearProtectionFacts(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(flagKey(accountIdB64));
    storage?.removeItem(bannerKey(accountIdB64));
    storage?.removeItem(stepKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}
