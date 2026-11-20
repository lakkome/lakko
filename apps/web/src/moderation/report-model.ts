// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0099 section 2, UX `reporting-and-moderation.md` sections 2.1 to 2.3 -
 * the report sheet's rules, decided once here so the sheet, the select mode
 * and the send cannot disagree. Pure: `vitest` runs without Solid, so every
 * decision lives in this module and the components hold markup.
 */

/** UX 2.2 - up to 20 messages can be picked (chosen, not measured). */
export const REPORT_MAX_MESSAGES = 20;

/** UX 2.2 - the comment is up to 500 characters (chosen). */
export const REPORT_COMMENT_MAX = 500;

/** The report's conversation kind (the core's codes, ADR-0099 section 2). */
export type ConversationKindCode = 1 | 2 | 3 | 4 | 5 | 6;
export const CONVERSATION_ONE_TO_ONE: ConversationKindCode = 1;
export const CONVERSATION_GROUP: ConversationKindCode = 2;
export const CONVERSATION_CHANNEL: ConversationKindCode = 3;
export const CONVERSATION_CONTACT_REQUEST: ConversationKindCode = 4;
export const CONVERSATION_DISCOVER: ConversationKindCode = 5;
export const CONVERSATION_CARD: ConversationKindCode = 6;

/** The report's target kind (the core's codes). */
export const TARGET_ACCOUNT = 1;
export const TARGET_GROUP = 2;
export const TARGET_CHANNEL = 3;

/** Who or what is reported. */
export type ReportTargetSpec =
  | { readonly kind: "person"; readonly accountKeyB64: string; readonly name: string }
  | {
      readonly kind: "group";
      readonly groupIdB64: string;
      /** The group's `@name` without the at sign: a group is reported only
       *  by it (its lookup key is the target). */
      readonly handle: string;
      readonly name: string;
    }
  | { readonly kind: "channel"; readonly channelIdHex: string; readonly name: string };

/** Evidence that is part of the report from the moment it is opened. */
export type FixedEvidence =
  | { readonly kind: "contact-request"; readonly wire: Uint8Array }
  | { readonly kind: "channel-post"; readonly channelIdHex: string; readonly post: Uint8Array }
  | { readonly kind: "listing"; readonly wire: Uint8Array };

/** Where `Add messages` picks from, and whose messages it may pick. */
export type PickSource =
  | {
      readonly kind: "chat";
      /** The conversation (standard base64 group id). */
      readonly groupIdB64: string;
      /** Only this author's messages (a person or a message target), or
       *  `null` for any message except the reporter's own (a group target). */
      readonly authorKeyB64: string | null;
    }
  | {
      /** A channel's posts (a channel target); the picks are post cursors. */
      readonly kind: "channel";
      readonly channelIdHex: string;
    };

/** The checkbox under the comment (UX 2.2), or none. */
export type AlsoKind = "block" | "leave" | "unsubscribe" | null;

/** Everything the sheet holds. */
export interface ReportDraft {
  readonly target: ReportTargetSpec;
  readonly conversationKind: ConversationKindCode;
  readonly pickFrom: PickSource | null;
  /** Picked message ids in `pickFrom.groupIdB64`, in the order picked. */
  readonly picked: readonly number[];
  readonly fixed: readonly FixedEvidence[];
  /** The reason code (1..8), `null` until one is chosen. */
  readonly reason: number | null;
  readonly comment: string;
  readonly also: AlsoKind;
  readonly alsoChecked: boolean;
}

/** What the sheet is doing. */
export type SheetPhase = "editing" | "sending" | "failed" | "limited" | "unavailable";

/** A new draft. `Also block` is on by default; `Leave` / `Unsubscribe` off. */
export function newDraft(input: {
  readonly target: ReportTargetSpec;
  readonly conversationKind: ConversationKindCode;
  readonly pickFrom?: PickSource | null;
  readonly picked?: readonly number[];
  readonly fixed?: readonly FixedEvidence[];
  /** A group or channel shows its checkbox only when there is something to
   *  leave (`true` by default). */
  readonly canLeave?: boolean;
}): ReportDraft {
  const also = alsoKindFor(input.target, input.canLeave ?? true);
  return {
    target: input.target,
    conversationKind: input.conversationKind,
    pickFrom: input.pickFrom ?? null,
    picked: (input.picked ?? []).slice(0, REPORT_MAX_MESSAGES),
    fixed: input.fixed ?? [],
    reason: null,
    comment: "",
    also,
    alsoChecked: also === "block",
  };
}

/** The checkbox a target carries (UX 2.2). */
export function alsoKindFor(target: ReportTargetSpec, canLeave: boolean): AlsoKind {
  if (target.kind === "person") return "block";
  if (!canLeave) return null;
  return target.kind === "group" ? "leave" : "unsubscribe";
}

/** Choose a reason. A code outside 1..8 is ignored. */
export function withReason(draft: ReportDraft, reason: number): ReportDraft {
  if (!Number.isInteger(reason) || reason < 1 || reason > 8) return draft;
  return { ...draft, reason };
}

/** Edit the comment, cut at {@link REPORT_COMMENT_MAX} characters. */
export function withComment(draft: ReportDraft, comment: string): ReportDraft {
  const chars = Array.from(comment);
  return { ...draft, comment: chars.length > REPORT_COMMENT_MAX ? chars.slice(0, REPORT_COMMENT_MAX).join("") : comment };
}

/**
 * Toggle one message in select mode. Removing always works; adding past
 * {@link REPORT_MAX_MESSAGES} is refused (`refused: true`, list unchanged).
 */
export function togglePick(
  picked: readonly number[],
  messageId: number,
): { readonly picked: readonly number[]; readonly refused: boolean } {
  if (picked.includes(messageId)) return { picked: picked.filter((id) => id !== messageId), refused: false };
  if (picked.length >= REPORT_MAX_MESSAGES) return { picked, refused: true };
  return { picked: [...picked, messageId], refused: false };
}

/**
 * Whether a timeline row may be picked: someone else's received message that
 * is still here (not deleted), and, when the source names an author, by
 * that author. The reporter's own messages never.
 */
export function pickable(
  row: { readonly incoming: boolean; readonly deleted: boolean; readonly authorKeyB64: string | undefined },
  source: PickSource,
  ownKeyB64: string | null,
): boolean {
  if (source.kind !== "chat") return false;
  if (!row.incoming || row.deleted) return false;
  if (row.authorKeyB64 === undefined) return false;
  if (ownKeyB64 !== null && row.authorKeyB64 === ownKeyB64) return false;
  return source.authorKeyB64 === null || row.authorKeyB64 === source.authorKeyB64;
}

/** One message or post the "Choose messages" list can show. */
export interface PickCandidate {
  /** The DS message id, or a channel post's cursor. */
  readonly id: number;
  /** The author's account key (a channel post: `undefined`, a publisher's). */
  readonly authorKeyB64: string | undefined;
  readonly text: string;
  /** What its files are, one line ("Photo", "File and 2 more"), empty for
   *  none: a message without text shows as its files. */
  readonly filesLabel: string;
  readonly createdAt: string;
  readonly deleted: boolean;
}

/**
 * The "Choose messages" list (UX 2.2): newest first, only what can be
 * picked. A conversation source keeps its rule ({@link pickable}); a
 * channel's readable posts are all candidates (a subscriber writes none).
 */
export function pickCandidatesFor(
  candidates: readonly PickCandidate[],
  source: PickSource,
  ownKeyB64: string | null,
): PickCandidate[] {
  const kept =
    source.kind === "channel"
      ? candidates.filter((c) => !c.deleted)
      : candidates.filter((c) =>
          pickable({ incoming: true, deleted: c.deleted, authorKeyB64: c.authorKeyB64 }, source, ownKeyB64),
        );
  return [...kept].sort((a, b) => b.id - a.id);
}

/** How many items the report includes, of how many were chosen. */
export function includedCounts(
  draft: ReportDraft,
  availablePicked: number,
): { readonly kept: number; readonly total: number } {
  const total = draft.picked.length + draft.fixed.length;
  const kept = Math.min(availablePicked, draft.picked.length) + draft.fixed.length;
  return { kept, total };
}

/** The "Included" line (UX 2.2, 2.3): a catalog key and its arguments. */
export function includedLine(
  kept: number,
  total: number,
): { readonly key: string; readonly args: Readonly<Record<string, number>> } {
  if (total === 0) return { key: "report.included_none", args: {} };
  if (kept < total) return { key: "report.included_partial", args: { n: kept, total } };
  return { key: "report.included", args: { n: total } };
}

/** Whether the empty-report hint shows: nothing is included. */
export function showsEmptyHint(kept: number): boolean {
  return kept === 0;
}

/** Whether `Send report` can be pressed. */
export function canSend(draft: ReportDraft, phase: SheetPhase): boolean {
  return draft.reason !== null && phase !== "sending";
}

/** UX 2.2 - the line above the button names the person only for a person. */
export function namesTheReported(target: ReportTargetSpec): boolean {
  return target.kind === "person";
}

/** What a send came to. */
export type SendOutcome =
  | { readonly kind: "sent"; readonly receiptId: string }
  | { readonly kind: "failed" }
  | { readonly kind: "limited" }
  | { readonly kind: "unavailable" };

/** The sheet's phase after a send, and whether the checkbox's action runs
 *  now (UX 2.3: with a sent report, and with a refusal that is not the
 *  network's - the daily limit, a server without reporting). */
export function afterSend(outcome: SendOutcome): {
  readonly phase: SheetPhase | "closed";
  readonly runsAlso: boolean;
} {
  switch (outcome.kind) {
    case "sent":
      return { phase: "closed", runsAlso: true };
    case "failed":
      return { phase: "failed", runsAlso: false };
    case "limited":
      return { phase: "limited", runsAlso: true };
    case "unavailable":
      return { phase: "unavailable", runsAlso: true };
  }
}

/** The answer of `POST /moderation/report`, read into an outcome. */
export function postOutcome(status: number, body: unknown): SendOutcome {
  const b = body !== null && typeof body === "object" ? (body as Record<string, unknown>) : {};
  if (status === 201 && typeof b.receipt_id === "string" && /^[A-Za-z0-9_-]{22}$/.test(b.receipt_id)) {
    return { kind: "sent", receiptId: b.receipt_id };
  }
  if (status === 429) return { kind: "limited" };
  if (status === 503) return { kind: "unavailable" };
  return { kind: "failed" };
}

/** A report's status as the reporter sees it. */
export type ReportStatus = "received" | "action" | "none";

/** The answer of `GET /moderation/report/{id}`, or `null` when unknown. */
export function statusOf(status: number, body: unknown): ReportStatus | null {
  if (status !== 200 || body === null || typeof body !== "object") return null;
  const s = (body as Record<string, unknown>).status;
  return s === "received" || s === "action" || s === "none" ? s : null;
}

/** The catalog key of a reason name from `reportReasonName` (`child-safety`
 *  -> `report.reason.child_safety`). */
export function reasonKey(name: string): string {
  return `report.reason.${name.replace(/-/g, "_")}`;
}

/** The catalog key of a status. */
export function statusKey(status: ReportStatus): string {
  return `report.status.${status}`;
}

/**
 * The highest moderator-list version this device has seen (ADR-0099 section
 * 2): a list below it is treated as unavailable, a list at or above it is
 * accepted and becomes the new floor.
 */
export function acceptListVersion(
  floor: bigint | null,
  version: bigint,
): { readonly accept: boolean; readonly floor: bigint } {
  if (floor !== null && version < floor) return { accept: false, floor };
  return { accept: true, floor: floor === null || version > floor ? version : floor };
}
