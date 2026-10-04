// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 — a conversation that is CLOSED on this device without
 * this device's membership having ended (`docs/ux/identity-recovery.md`
 * 16.4 and 16.5).
 *
 * Two kinds, one persisted record each, per `(account, group)`:
 *
 *   * **`one-to-one-keys-reset`** (16.4) — the one-to-one with a contact's
 *     OLD keys. A one-to-one cannot lose a person (ADR-0062), so when the
 *     contact's keys are reset the chat ends rather than being repaired.
 *     Written by the device the moment it notices the reset, BEFORE any
 *     eviction anywhere, because a message sent here would reach the old
 *     devices — which is exactly what a reset exists to end. Nothing is
 *     sent into it again; a late row is dropped before it is stored
 *     (`keys-reset.ts` `dropsLateMessage`); the journal is left to the
 *     server's retention. The record survives the contact's verification:
 *     the new keys get a NEW chat, this one stays closed.
 *   * **`group-closed`** (16.5) — a group an admin closed after recreating
 *     it, set by a received group-closed envelope (`group-closed-store.ts`,
 *     where the gate and the successor id live). Carried here only as the
 *     one predicate the composer and the list row read, so the two shapes
 *     cannot drift.
 *
 * Unlike `membership-ended.ts`, the state is NOT derivable from the core
 * (`isActive` stays `true`: this device is still a member), so the record
 * is load-bearing rather than a narrowing. It is written to `localStorage`
 * like the leave-requested stamp, under the account, and discarded with
 * the chat (`delete-chat.ts`) or the account (`wipe-account.ts`).
 */
import { t } from "../l10n";
import { composerState, type ComposerState, type MembershipState } from "./membership-ended";

/** Why a conversation is closed on this device. */
export type ClosedReason = "one-to-one-keys-reset";

/** The persisted record. */
export interface ClosedRecord {
  readonly reason: ClosedReason;
  /** base64 of the contact's account_id (the person whose keys were reset),
   *  so the row and the terminal row can name them through the account id
   *  after the row has been re-keyed to their new key. */
  readonly contactAccountIdB64: string;
  /** base64 of the contact's OLD account key, the one this chat holds. */
  readonly previousKeyB64: string;
  readonly closedAtMs: number;
}

const CLOSED_STORE_PREFIX = "lakko.conversation-closed.v1";

function closedStoreKey(accountIdB64: string, groupIdB64: string): string {
  return `${CLOSED_STORE_PREFIX}.${accountIdB64}.${groupIdB64}`;
}

function isClosedRecord(v: unknown): v is ClosedRecord {
  return (
    typeof v === "object" &&
    v !== null &&
    (v as ClosedRecord).reason === "one-to-one-keys-reset" &&
    typeof (v as ClosedRecord).contactAccountIdB64 === "string" &&
    typeof (v as ClosedRecord).previousKeyB64 === "string" &&
    typeof (v as ClosedRecord).closedAtMs === "number" &&
    Number.isFinite((v as ClosedRecord).closedAtMs)
  );
}

/**
 * Record the close. The FIRST record wins: a re-observation of the same
 * reset (the next identity check, the next open) is the same fact. Never
 * throws — a quota failure costs this device the closed shape, and the
 * worst that follows is the status quo (a send into the old journal that
 * the old devices may read), which is why the caller also refuses to send
 * on the in-memory verdict. Returns whether a record is now present.
 */
export function recordConversationClosed(
  accountIdB64: string,
  groupIdB64: string,
  record: ClosedRecord,
): boolean {
  try {
    const key = closedStoreKey(accountIdB64, groupIdB64);
    const store = globalThis.localStorage;
    if (store === undefined || store === null) return false;
    if (store.getItem(key) !== null) return true;
    store.setItem(key, JSON.stringify(record));
    notify();
    return true;
  } catch {
    return false;
  }
}

/** The record, or `undefined` when the conversation is not closed (or the
 *  row is unreadable, which reads as open: a corrupt row must not close a
 *  chat the person can still use). */
export function loadConversationClosed(
  accountIdB64: string,
  groupIdB64: string,
): ClosedRecord | undefined {
  try {
    const raw = globalThis.localStorage?.getItem(closedStoreKey(accountIdB64, groupIdB64));
    if (raw === null || raw === undefined) return undefined;
    const parsed: unknown = JSON.parse(raw);
    return isClosedRecord(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

/** Discard the record for one group — the `Delete this chat` path. */
export function clearConversationClosed(accountIdB64: string, groupIdB64: string): void {
  try {
    globalThis.localStorage?.removeItem(closedStoreKey(accountIdB64, groupIdB64));
    notify();
  } catch {
    /* best-effort */
  }
}

/** Drop every record for one account — `forgetDevice` hygiene. */
export function clearConversationClosedForAccount(accountIdB64: string): void {
  try {
    const store = globalThis.localStorage;
    if (store === undefined || store === null) return;
    const victims: string[] = [];
    for (let i = 0; i < store.length; i++) {
      const k = store.key(i);
      if (k !== null && k.startsWith(`${CLOSED_STORE_PREFIX}.${accountIdB64}.`)) victims.push(k);
    }
    for (const k of victims) store.removeItem(k);
    notify();
  } catch {
    /* best-effort */
  }
}

const listeners = new Set<() => void>();

/** Subscribe to any write of a closed record on this page. */
export function subscribeConversationClosed(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function notify(): void {
  for (const l of listeners) {
    try {
      l();
    } catch {
      /* a failing listener must not break the write */
    }
  }
}

// ---------------------------------------------------------------------------
// The decision: close a one-to-one? (UX 16.4)
// ---------------------------------------------------------------------------

/**
 * Whether THIS one-to-one is the chat with a flagged contact's old keys.
 *
 * True when the conversation is a one-to-one (ADR-0062, the kind the group
 * carries — never the people count) and the OTHER person's key is a
 * flagged contact's `previousKey`. A torn roster (`undefined`) decides
 * nothing: closing a chat is a claim, and a device that cannot read the
 * roster has no basis for it.
 */
export function oneToOneClosesForReset(input: {
  readonly kind: "chat" | "group";
  /** The distinct person keys of the roster, base64; `undefined` = torn. */
  readonly personKeysB64: readonly string[] | undefined;
  readonly ownPersonKeyB64: string | null;
  /** `previousKeyB64` → the flagged contact's account id, every flagged
   *  contact. */
  readonly flaggedPreviousKeys: ReadonlyMap<string, string>;
}): { readonly previousKeyB64: string; readonly contactAccountIdB64: string } | null {
  if (input.kind !== "chat") return null;
  if (input.personKeysB64 === undefined) return null;
  for (const k of input.personKeysB64) {
    if (k === input.ownPersonKeyB64) continue;
    const account = input.flaggedPreviousKeys.get(k);
    if (account !== undefined) return { previousKeyB64: k, contactAccountIdB64: account };
  }
  return null;
}

// ---------------------------------------------------------------------------
// The closed shape — composed over the ended shape, never beside it
// ---------------------------------------------------------------------------

/** Why the composer slot is not the composer. */
export type ConversationCloseKind =
  | { readonly kind: "open" }
  /** `membership-ended.ts`: this device is no longer in the group. */
  | { readonly kind: "ended"; readonly state: MembershipState }
  /** UX 16.4: the one-to-one with the old keys. */
  | { readonly kind: "one-to-one-keys-reset"; readonly record: ClosedRecord }
  /** UX 16.5: an admin closed the group after recreating it. */
  | {
      readonly kind: "group-closed";
      /** base64 of the successor group, or `null` when none was named. */
      readonly successorGroupIdB64: string | null;
      readonly closedAtMs: bigint;
    };

/**
 * Decide the one close state of a conversation. The membership ending
 * wins over everything (its reads are frozen and nothing can be sent),
 * then the one-to-one close, then the group close.
 */
export function conversationCloseKind(input: {
  readonly membership: MembershipState;
  readonly closedRecord: ClosedRecord | undefined;
  readonly groupClosed: { successorGroupIdB64: string | null; closedAtMs: bigint } | undefined;
}): ConversationCloseKind {
  if (input.membership.kind === "ended") return { kind: "ended", state: input.membership };
  if (input.closedRecord !== undefined) {
    return { kind: "one-to-one-keys-reset", record: input.closedRecord };
  }
  if (input.groupClosed !== undefined) {
    return {
      kind: "group-closed",
      successorGroupIdB64: input.groupClosed.successorGroupIdB64,
      closedAtMs: input.groupClosed.closedAtMs,
    };
  }
  return { kind: "open" };
}

/** Whether anything may be sent into the conversation. */
export function conversationClosed(close: ConversationCloseKind): boolean {
  return close.kind !== "open";
}

/** The two lines of the terminal row (UX 16.4 / 16.5), or `null`. */
export interface ClosedTerminalRow {
  readonly headline: string;
  readonly consequence: string;
}

/**
 * The terminal row's copy for the two reset closes. `contactName` names the
 * reset contact (resolved through the account id); `closerName` names the
 * admin who posted the group-closed notice. The ended case is
 * `membership-ended.ts`'s own `terminalRow`.
 */
export function closedTerminalRow(
  close: ConversationCloseKind,
  names: { readonly contactName: string; readonly closerName: string },
): ClosedTerminalRow | null {
  switch (close.kind) {
    case "one-to-one-keys-reset":
      return {
        headline: t("chat.keys_reset.one_to_one_closed", { name: names.contactName }),
        consequence: t("chat.ended.consequence"),
      };
    case "group-closed":
      return {
        headline: t("group.closed.system", { name: names.closerName }),
        consequence: t("chat.ended.consequence"),
      };
    default:
      return null;
  }
}

/** What occupies the composer slot of a closed conversation. */
export type ClosedComposerState =
  | ComposerState
  | {
      readonly kind: "closed";
      /** The static bar. */
      readonly notice: string;
      /** The one button, or `null` for no button at all. */
      readonly action:
        | {
            readonly label: string;
            readonly enabled: boolean;
            /** The helper under a disabled button (`Verify ... first.`),
             *  or `null`. */
            readonly helper: string | null;
          }
        | null;
      readonly draft: string | null;
      readonly draftLabel: string | null;
    };

/**
 * The composer slot (UX 16.4 / 16.5 tables), composed over
 * `membership-ended.ts`'s `composerState` so the draft rule is one rule.
 *
 *   * one-to-one: `You can no longer send messages in this chat.` and
 *     `Start a new chat` — DISABLED with `Verify {name}'s new safety number
 *     first.` while the contact is `reset-unverified`, enabled once verified;
 *   * group: `This group was closed.` and `Open the new group`, shown only
 *     once this device has JOINED the successor (the id is the admin's
 *     claim); until then, and for a device that was not invited, the bar
 *     alone.
 */
export function closedComposerState(
  close: ConversationCloseKind,
  draft: string,
  context: {
    /** The reset contact is still `reset-unverified` on this device. */
    readonly contactUnverified: boolean;
    readonly contactName: string | null;
    /** This device holds the successor group. */
    readonly joinedSuccessor: boolean;
  },
): ClosedComposerState {
  if (close.kind === "open") return { kind: "composer" };
  if (close.kind === "ended") return composerState(close.state, draft);
  const worthKeeping = draft.trim().length > 0;
  const draftPart = {
    draft: worthKeeping ? draft : null,
    draftLabel: worthKeeping ? t("chat.ended.unsent_draft") : null,
  };
  if (close.kind === "one-to-one-keys-reset") {
    return {
      kind: "closed",
      notice: t("chat.keys_reset.one_to_one_composer"),
      action: {
        label: t("chat.keys_reset.start_new_chat"),
        enabled: !context.contactUnverified,
        helper: context.contactUnverified
          ? context.contactName === null
            ? t("contacts.keys_reset.verify_first_short")
            : t("contacts.keys_reset.verify_first", { name: context.contactName })
          : null,
      },
      ...draftPart,
    };
  }
  return {
    kind: "closed",
    notice: t("group.closed.composer"),
    action:
      close.successorGroupIdB64 !== null && context.joinedSuccessor
        ? { label: t("group.closed.open_new"), enabled: true, helper: null }
        : null,
    ...draftPart,
  };
}

/** The chat-list row's secondary line for a reset close, or `null`. */
export function closedChatListLine(
  close: ConversationCloseKind,
  contactName: string,
): string | null {
  switch (close.kind) {
    case "one-to-one-keys-reset":
      return t("chat.keys_reset.one_to_one_row", { name: contactName });
    case "group-closed":
      return t("group.closed.row");
    default:
      return null;
  }
}
