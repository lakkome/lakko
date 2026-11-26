// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 3 - the pure rules behind channel member cards, kept free
 * of the wasm loader and the network so they can be tested in bare node.
 *
 * A member card is the member's profile (v3) sealed under the channel's
 * current epoch seed and uploaded under the subscriber key it registered.
 * What the rules here decide:
 *
 * - what the OWN card says: the name always; the `@username` and the
 *   `channel` reach grant only when the "who can find me" setting lets this
 *   channel's members find the owner (UX section 6.2: the username goes with
 *   the permission, because a username is a handle for finding someone);
 * - when to upload it again: no card yet, a newer epoch (a newcomer holds
 *   only current seeds), or different content at the current epoch;
 * - what to do with a grant READ from someone else's card: keep it, withdraw
 *   a grant the person no longer hands out, or leave the store alone;
 * - whether a stored subscriber key predates the Ed25519 flag day.
 */

import { t } from "../l10n";

/** What the own card carries. */
export interface CardContent {
  readonly displayName: string | null;
  /** Only with the grant (UX 6.2). */
  readonly username: string | null;
  /** Whether the `channel` reach grant rides in the card. */
  readonly includeGrant: boolean;
}

/** Decide the own card's fields from the profile and the setting. */
export function cardContent(input: {
  readonly displayName: string | null;
  readonly username: string | null;
  /** `allowsScope(setting, Channel(id))`; `false` with no setting loaded. */
  readonly channelAllowed: boolean;
  /** A reach session is running (it holds the seed grants derive from). */
  readonly reachAvailable: boolean;
}): CardContent {
  const includeGrant = input.channelAllowed && input.reachAvailable;
  const name = input.displayName === null ? null : input.displayName.trim();
  return {
    displayName: name === null || name.length === 0 ? null : name,
    username: includeGrant ? input.username : null,
    includeGrant,
  };
}

/** A card this device uploaded, as far as re-upload decisions need. */
export interface UploadedCard {
  readonly epoch: number;
  readonly fingerprint: string;
}

/** A stable description of the card's content: two cards with the same
 *  fingerprint carry the same profile and the same grant version. */
export function cardFingerprint(
  content: CardContent,
  updatedAtMs: number,
  grantVersion: number | null,
): string {
  return JSON.stringify([
    content.displayName,
    content.username,
    content.includeGrant ? grantVersion : null,
    updatedAtMs,
  ]);
}

/**
 * Whether the own card must be uploaded (again) for `epoch`.
 *
 * - nothing uploaded yet: yes;
 * - a newer epoch: yes (a member who joined after the rotation holds only
 *   the current seed and cannot open an older card);
 * - the same epoch with different content (a renamed profile, a changed
 *   setting): yes;
 * - an older epoch than the one already uploaded: never - the server keeps
 *   the newest and refuses a stale one.
 */
export function cardUploadDue(
  last: UploadedCard | undefined,
  epoch: number,
  fingerprint: string,
): boolean {
  if (last === undefined) return true;
  if (epoch > last.epoch) return true;
  if (epoch < last.epoch) return false;
  return fingerprint !== last.fingerprint;
}

/** What to do with the grant state of one card read from the member list. */
export type CardGrantDecision =
  /** File the card's `channel` grant under the sender's account key. */
  | "file"
  /** The card carries no `channel` grant: withdraw one held from before. */
  | "withdraw"
  /** Leave the store alone. */
  | "skip";

/**
 * Decide the grant outcome of one card. The card's author is the account
 * whose key discovery returns for the account id the card CLAIMS; a claim
 * alone is not proof, so a grant is filed only after the reach check showed
 * it is registered on that account's mailbox (a grant another member copied
 * or made up would otherwise be filed under someone else's key, and a made-up
 * high version would shadow the real one).
 */
export function cardGrantDecision(input: {
  /** The card is this device's own upload. */
  readonly isOwnCard: boolean;
  /** The author's account key, `null` when it could not be resolved. */
  readonly senderAccountKeyHex: string | null;
  readonly ownAccountKeyHex: string | null;
  readonly carriesChannelGrant: boolean;
  /** The reach check accepted the grant on the author's mailbox; `null`
   *  when it did not run or could not answer (offline). */
  readonly grantVerified: boolean | null;
}): CardGrantDecision {
  if (input.isOwnCard) return "skip";
  const sender = input.senderAccountKeyHex;
  if (sender === null) return "skip";
  if (input.ownAccountKeyHex !== null && sender === input.ownAccountKeyHex) return "skip";
  if (!input.carriesChannelGrant) return "withdraw";
  // A refused grant is not the person narrowing their setting (that shows as
  // a card without a grant), and an unanswered check proves nothing: in both
  // cases what is held stays as it is.
  return input.grantVerified === true ? "file" : "skip";
}

/** The label a member row shows. */
export function memberRowName(displayName: string | null | undefined, isSelf: boolean): string {
  if (isSelf) return t("channels.members.you");
  const name = displayName?.trim() ?? "";
  return name.length > 0 ? name : t("channels.members.unnamed");
}

/** Whether a stored subscriber key predates the Ed25519 flag day: its
 *  stored public half is not the Ed25519 public key of its secret. */
export function isStaleSubscriberKey(storedPublic: Uint8Array, derivedPublic: Uint8Array): boolean {
  if (storedPublic.length !== derivedPublic.length) return true;
  for (let i = 0; i < storedPublic.length; i++) {
    if (storedPublic[i] !== derivedPublic[i]) return true;
  }
  return false;
}

/**
 * Seal one envelope per registered subscriber, skipping a key the core
 * refuses (a pre-flag-day X25519 key that is not a valid Ed25519 point, or a
 * small-order one). Such a subscriber cannot open anything sealed to it, and
 * one stale registration must not abort every rotation of the channel.
 */
export function sealEachSkippingInvalid<T>(
  subscribersHex: readonly string[],
  seal: (subscriberHex: string) => T,
): { readonly sealed: Array<{ readonly subscriberHex: string; readonly value: T }>; readonly skipped: string[] } {
  const sealed: Array<{ subscriberHex: string; value: T }> = [];
  const skipped: string[] = [];
  for (const hex of subscribersHex) {
    try {
      sealed.push({ subscriberHex: hex, value: seal(hex) });
    } catch {
      skipped.push(hex);
    }
  }
  return { sealed, skipped };
}

/**
 * ADR-0101 section 8 - the profile a member card carries counts only when it
 * is signed by the account the card's binding names (`verifyProfileEnvelope`
 * against the card's `identityPk`). A card whose profile is not is still a
 * member (the binding verified), drawn as an unnamed row: its name, username,
 * photo and grants are refused. Pure over the verdict.
 */
export function unnamedCardRow(input: {
  readonly identityPkHex: string;
  readonly isOwnCard: boolean;
  readonly ownAccountKeyHex: string | null;
}): { key: string; name: string; username: null; isSelf: boolean; named: boolean } {
  const isSelf =
    input.isOwnCard || (input.ownAccountKeyHex !== null && input.identityPkHex === input.ownAccountKeyHex);
  return {
    key: isSelf ? "self" : input.identityPkHex,
    name: memberRowName(null, isSelf),
    username: null,
    isSelf,
    named: isSelf,
  };
}

/** Where the own card's subscriber key comes from on this device. */
export type OwnCardKeySource =
  /** The key this device reads the channel with (a subscriber, or a reader
   *  device of the owner or a publisher: the account's register key). */
  | "reading-key"
  /** The account's register key, which the publishing device registered at
   *  the channels service as a reader (`account-reader-key.ts`). */
  | "register-key";

/** Whether and how this device uploads its own member card. */
export interface OwnCardPlan {
  readonly keySource: OwnCardKeySource;
  /** A card the service refuses as `unknown-subscriber` registers the key
   *  once and tries again: only for a subscriber's own key, whose
   *  registration is an ordinary subscribe. The owner's and the publishers'
   *  key is registered by the publishing device, never from here (a private
   *  channel wants an invite proof for it). */
  readonly reRegister: boolean;
}

/**
 * ADR-0060 section 3, ADR-0101 section 7 (walk 2026-11-26, D1): every member
 * uploads a member card, the owner and the publishers too. A channel comes
 * back to an owner who reset only through the owner's card in the member
 * list, so an owner that never uploads one never gets the channel back.
 *
 * | this device                              | reading | key held              | plan                       |
 * |------------------------------------------|---------|-----------------------|----------------------------|
 * | subscriber                               | yes     | its subscriber key    | reading-key, re-register   |
 * | reader device of the owner / a publisher | yes     | the register key      | reading-key, no re-register|
 * | publishing device (owner or publisher)   | -       | register key, and the | register-key, no re-register|
 * |                                          |         | service holds it      |                            |
 * | anything else                            |         |                       | none                       |
 *
 * The card's content follows the same "who can find me" rule for every role
 * (`cardContent`, `ownCardShareable`). Pure.
 */
export function ownCardPlan(input: {
  /** This device holds the channel's publishing state. */
  readonly publishing: boolean;
  /** The account owns or publishes in the channel, but this device only
   *  reads it. */
  readonly readerDevice: boolean;
  /** This device reads the channel (it is let in). */
  readonly reading: boolean;
  /** Hex of the key this device reads with, or `null`. */
  readonly readingKeyHex: string | null;
  /** Hex of the account's register key for the channel, or `null`. */
  readonly registerKeyHex: string | null;
  /** Hex of the reader key the publishing device registered, or `null`. */
  readonly registeredReaderKeyHex: string | null;
}): OwnCardPlan | null {
  if (input.publishing) {
    if (input.registerKeyHex === null || input.registeredReaderKeyHex !== input.registerKeyHex) return null;
    return { keySource: "register-key", reRegister: false };
  }
  if (!input.reading || input.readingKeyHex === null) return null;
  return { keySource: "reading-key", reRegister: !input.readerDevice };
}
