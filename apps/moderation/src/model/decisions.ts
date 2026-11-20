// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Decisions (UX section 5.3): which ones a case offers, the sentence the
 * moderator confirms before touching the key, and the exact inputs of
 * `buildModerationStatement`. Everything here is pure, so what will be
 * signed is decided in one tested place and the screen only shows it.
 */

import type { DiscoveryRead } from "../api/types";
import { b64ToBytes, bytesToB64, bytesToHex, hexToBytes } from "./bytes";
import { formatTime } from "./format";
import { action, reasonByCode, targetKind, type ActionName, type TargetKindName } from "./names";

export type SuspendLength = "1d" | "7d" | "30d" | "none";

const DAY_MS = 86_400_000;

const SUSPEND_DAYS: Readonly<Record<SuspendLength, number>> = { "1d": 1, "7d": 7, "30d": 30, none: 0 };

/** The suspension's end for `length` from `nowMs`; `0` means no end. */
export function suspendUntilMs(length: SuspendLength, nowMs: number): number {
  const days = SUSPEND_DAYS[length];
  return days === 0 ? 0 : nowMs + days * DAY_MS;
}

/** One decision a case offers. */
export interface DecisionOption {
  readonly id: string;
  /** The button / picker label. */
  readonly label: string;
  /** `no-action` is not a statement: nothing is signed or published. */
  readonly action: ActionName | "no-action";
  /** What the statement names (for `no-action`, the case itself). */
  readonly targetKind: TargetKindName;
  readonly targetHex: string;
  readonly suspend?: SuspendLength;
  /**
   * The case this decision closes when it is not the statement's own target
   * (a username taken back from an account case).
   */
  readonly closesCase?: { readonly kind: string; readonly targetHex: string };
}

/**
 * The decisions a case offers, in the order the panel lists them.
 * `discovery` is the account's discovery read: `Take back @username` is
 * offered only when it names the username's lookup key.
 */
export function decisionOptions(
  caseKind: string,
  targetHex: string,
  discovery: DiscoveryRead | null,
): DecisionOption[] {
  const out: DecisionOption[] = [];
  const kind = targetKind(caseKind);
  if (kind === undefined) return out;
  out.push({ id: "no-action", label: "No action", action: "no-action", targetKind: kind.name, targetHex });
  switch (kind.name) {
    case "account": {
      const lengths: readonly [SuspendLength, string][] = [
        ["1d", "Suspend for 1 day"],
        ["7d", "Suspend for 7 days"],
        ["30d", "Suspend for 30 days"],
        ["none", "Suspend with no end date"],
      ];
      for (const [len, label] of lengths) {
        out.push({ id: `suspend-${len}`, label, action: "suspend", targetKind: "account", targetHex, suspend: len });
      }
      const keys = discovery?.name_lookup_keys_b64 ?? [];
      keys.forEach((k, i) => {
        out.push({
          id: `take-back-${i}`,
          label: keys.length === 1 ? "Take back @username" : `Take back @username (${i + 1} of ${keys.length})`,
          action: "take-back-name",
          targetKind: "name",
          targetHex: bytesToHex(b64ToBytes(k)),
          closesCase: { kind: "account", targetHex },
        });
      });
      break;
    }
    case "name":
      out.push({ id: "take-back", label: "Take back @username", action: "take-back-name", targetKind: "name", targetHex });
      break;
    case "channel":
      out.push(
        { id: "channel-delist", label: "Remove from Discover", action: "channel-delist", targetKind: "channel", targetHex },
        { id: "channel-block-handle", label: "Block the @name", action: "channel-block-handle", targetKind: "channel", targetHex },
        { id: "channel-remove", label: "Remove posts", action: "channel-remove", targetKind: "channel", targetHex },
      );
      break;
    case "group-handle":
      out.push(
        { id: "group-delist", label: "Remove from search", action: "group-delist", targetKind: "group-handle", targetHex },
        { id: "group-block-handle", label: "Block the @name", action: "group-block-handle", targetKind: "group-handle", targetHex },
      );
      break;
    case "blob":
      break;
  }
  return out;
}

/** The decision that lifts an account's suspension (appeals). */
export function liftOption(accountTargetHex: string): DecisionOption {
  return { id: "lift", label: "Lift the suspension", action: "lift", targetKind: "account", targetHex: accountTargetHex };
}

/**
 * The decision that keeps an account's suspension and declines its pending
 * appeal (appeals). It is a signed statement like any other and goes into
 * the moderation log; it changes no standing.
 */
export function keepSuspensionOption(accountTargetHex: string): DecisionOption {
  return {
    id: "keep-suspension",
    label: "Keep the suspension",
    action: "keep-suspension",
    targetKind: "account",
    targetHex: accountTargetHex,
  };
}

/**
 * Decisions that close no case: the ones that undo an earlier decision,
 * keeping a suspension on appeal, and deleting a file. The service closes, for every other decision, only the
 * case of the statement's own target (a username taken back closes the
 * account case it was taken from), so the panel never says otherwise.
 */
const CLOSES_NO_CASE: ReadonlySet<string> = new Set([
  "lift",
  "release-name",
  "show-in-search",
  "channel-restore",
  "group-restore",
  "blob-delete",
  "keep-suspension",
]);

/** `true` when deciding `action` closes no case and no report. */
export function closesNoCase(action: string): boolean {
  return CLOSES_NO_CASE.has(action);
}

const SUSPEND_WORDS: Readonly<Record<SuspendLength, string>> = {
  "1d": "for 1 day",
  "7d": "for 7 days",
  "30d": "for 30 days",
  none: "with no end date",
};

/**
 * The sentence the moderator confirms. It names exactly the action, its
 * target, its length and its reason, and says where it goes:
 * "Suspend this account for 7 days for spam or a scam. This will be
 * published in the moderation log."
 */
export function confirmationSentence(option: DecisionOption, reasonCode: number): string {
  const reason = reasonByCode(reasonCode);
  const why = reason === undefined ? "" : ` for ${reason.phrase}`;
  const published = " This will be published in the moderation log.";
  switch (option.action) {
    case "no-action":
      return `Close this case with no action${why}. Every open report on it will show No action. Nothing is published.`;
    case "suspend":
      return `Suspend this account ${SUSPEND_WORDS[option.suspend ?? "none"]}${why}.${published}`;
    case "lift":
      return `Lift this account's suspension${why}. The account can use Lakko again at once.${published}`;
    case "keep-suspension":
      return `Keep this account's suspension${why} and decline the appeal. The person sees that their appeal was declined.${published}`;
    case "take-back-name":
      return option.closesCase === undefined
        ? `Take back this @username${why}. Nobody can claim it afterwards.${published}`
        : `Take back this account's @username${why}. Nobody can claim it afterwards.${published}`;
    case "channel-delist":
      return `Remove this channel from Discover${why}.${published}`;
    case "channel-block-handle":
      return `Block this channel's @name${why}. Nobody can use the name afterwards.${published}`;
    case "channel-remove":
      return `Remove this channel and its posts${why}. Subscribers can no longer read or post in it.${published}`;
    case "group-delist":
      return `Remove this group from search${why}.${published}`;
    case "group-block-handle":
      return `Block this group's @name${why}. Nobody can use the name afterwards.${published}`;
    default: {
      const info = action(option.action);
      return `${info?.label ?? option.action}${why}.${published}`;
    }
  }
}

/** The arguments of `buildModerationStatement`, in its order. */
export interface StatementInputs {
  readonly statementId: Uint8Array;
  readonly action: number;
  readonly targetKind: number;
  readonly target: Uint8Array;
  readonly reason: number;
  readonly reasonSalt: Uint8Array;
  readonly untilMs: bigint;
  readonly issuedAtMs: bigint;
  readonly moderatorId: Uint8Array;
  readonly listVersion: bigint;
}

/**
 * The statement inputs for `option`. Throws, with a readable sentence, on
 * anything the statement could not carry: a `no-action` (not signed), an
 * action whose target kind does not match, a target of the wrong length, an
 * unknown reason, or ids that are not 16 bytes.
 */
export function statementInputs(args: {
  readonly option: DecisionOption;
  readonly reasonCode: number;
  readonly nowMs: number;
  readonly moderatorId: Uint8Array;
  readonly listVersion: number;
  readonly statementId: Uint8Array;
  readonly reasonSalt: Uint8Array;
}): StatementInputs {
  const { option } = args;
  if (option.action === "no-action") throw new Error("No action isn't signed.");
  const info = action(option.action);
  if (info === undefined) throw new Error("This decision isn't known to the panel.");
  if (info.target !== option.targetKind) throw new Error("This decision doesn't apply to this kind of target.");
  const kind = targetKind(option.targetKind);
  const target = hexToBytes(option.targetHex);
  if (kind === undefined || target === null || target.length !== kind.length) {
    throw new Error("The target isn't valid for its kind.");
  }
  if (reasonByCode(args.reasonCode) === undefined) throw new Error("Choose a reason.");
  if (args.statementId.length !== 16 || args.reasonSalt.length !== 16 || args.moderatorId.length !== 16) {
    throw new Error("The decision couldn't be prepared. Reload the page and try again.");
  }
  const until = option.action === "suspend" ? suspendUntilMs(option.suspend ?? "none", args.nowMs) : 0;
  return {
    statementId: args.statementId,
    action: info.byte,
    targetKind: kind.byte,
    target,
    reason: args.reasonCode,
    reasonSalt: args.reasonSalt,
    untilMs: BigInt(until),
    issuedAtMs: BigInt(args.nowMs),
    moderatorId: args.moderatorId,
    listVersion: BigInt(args.listVersion),
  };
}

/** The body of `POST /panel/api/decide`. */
export function decideBody(args: {
  readonly signedStatement: Uint8Array;
  readonly reasonCode: number;
  readonly reasonSalt: Uint8Array;
  readonly note: string;
  readonly option: DecisionOption;
}): Record<string, unknown> {
  const body: Record<string, unknown> = {
    signed_statement_b64: bytesToB64(args.signedStatement),
    reason: args.reasonCode,
    reason_salt_b64: bytesToB64(args.reasonSalt),
  };
  const note = args.note.trim();
  if (note !== "") body.note = note;
  if (args.option.closesCase !== undefined) {
    body.case = { kind: args.option.closesCase.kind, target_hex: args.option.closesCase.targetHex };
  }
  return body;
}

/** The internal note's bound (the service refuses more). */
export const NOTE_MAX = 2000;

/** What the moderator sees after a decide answer. */
export type DecideOutcome =
  | { readonly kind: "applied"; readonly closed: number }
  | { readonly kind: "logged-only"; readonly closed: number }
  | {
      readonly kind: "no-change";
      readonly closed: number;
      readonly superseded: boolean;
      /** A kept suspension whose appeal was no longer waiting. */
      readonly noPendingAppeal: boolean;
    }
  | { readonly kind: "not-applied"; readonly closed: number; readonly detail: string | null; readonly failed: boolean };

/** Classify a decide answer by its status (200 / 422 / 502). */
export function decideOutcome(
  status: number,
  answer: {
    readonly reports_closed: number;
    readonly forwarded?: boolean;
    readonly applied?: boolean;
    readonly superseded?: boolean;
    readonly already_removed?: boolean;
    readonly no_pending_appeal?: boolean;
    readonly detail?: string | null;
  },
): DecideOutcome {
  if (status === 422) return { kind: "not-applied", closed: answer.reports_closed, detail: answer.detail ?? null, failed: false };
  if (status === 502) return { kind: "not-applied", closed: answer.reports_closed, detail: null, failed: true };
  if (answer.forwarded === true && answer.applied === true) return { kind: "applied", closed: answer.reports_closed };
  if (answer.forwarded === true) {
    return {
      kind: "no-change",
      closed: answer.reports_closed,
      superseded: answer.superseded === true,
      noPendingAppeal: answer.no_pending_appeal === true,
    };
  }
  return { kind: "logged-only", closed: answer.reports_closed };
}

/**
 * How long a signed decision can be sent again: every service refuses a
 * statement issued more than 24 hours earlier (`stale-statement`).
 */
export const RESEND_WINDOW_MS = 24 * 60 * 60 * 1000;

/** The last moment a decision issued at `issuedAtMs` can be sent again. */
export function resendDeadlineMs(issuedAtMs: number): number {
  return issuedAtMs + RESEND_WINDOW_MS;
}

/** `true` while a decision issued at `issuedAtMs` can still be sent again. */
export function canResend(issuedAtMs: number, nowMs: number): boolean {
  return nowMs <= resendDeadlineMs(issuedAtMs);
}

/** The sentence for a service's refusal code inside a 422 answer. */
export function applyDetailPhrase(detail: string): string {
  switch (detail) {
    case "stale-statement":
      return "it was signed more than 24 hours ago";
    case "future-statement":
      return "its time is ahead of the service's clock";
    case "stale-list":
      return "a newer moderator list is in force";
    case "bad-list":
      return "the service doesn't hold the moderator list it names";
    case "not-authorised":
      return "the service doesn't accept your key for it";
    case "unknown-account":
      return "the service has no account with this key";
    case "reason-mismatch":
      return "the reason didn't match the signed decision";
    case "wrong-service":
      return "it went to a service that doesn't apply this kind of decision";
    default:
      return detail;
  }
}

/** What the outcome sentence needs to know about the decision itself. */
export interface OutcomeContext {
  /** The decision's action; decides whether reports were closed and how a kept suspension reads. */
  readonly action?: string | undefined;
  /** When the decision was signed, for the Send again deadline. */
  readonly issuedAtMs?: number | undefined;
}

/** The sentence for an outcome. */
export function outcomeSentence(o: DecideOutcome, ctx: OutcomeContext = {}): string {
  const noCase = ctx.action !== undefined && closesNoCase(ctx.action);
  const closed = noCase ? "" : o.closed === 1 ? " 1 report closed." : ` ${o.closed} reports closed.`;
  const keep = ctx.action === "keep-suspension";
  const until =
    ctx.issuedAtMs === undefined
      ? " A decision can be sent again for 24 hours after it was signed; after that, sign a new one."
      : ` You can send it again until ${formatTime(resendDeadlineMs(ctx.issuedAtMs))}; after that, sign a new decision.`;
  switch (o.kind) {
    case "applied":
      return keep
        ? `Published in the moderation log. The suspension stays, and the person sees that their appeal was declined.${closed}`
        : `Published in the moderation log and applied.${closed}`;
    case "no-change":
      if (o.noPendingAppeal) {
        return `Published in the moderation log, but no appeal was waiting on this suspension, so nothing changed.${closed}`;
      }
      return o.superseded
        ? `Published in the moderation log. A later decision on this target already applies, so nothing changed.${closed}`
        : `Published in the moderation log. It was already in effect, so nothing changed.${closed}`;
    case "logged-only":
      return `Published in the moderation log. No service is set up to apply it on this server.${closed}`;
    case "not-applied":
      return o.failed
        ? `Published in the moderation log, but not applied yet: the service didn't answer.${closed}${until}`
        : `Published in the moderation log, but the service refused to apply it${o.detail === null ? "" : ` (${applyDetailPhrase(o.detail)})`}.${closed}${until}`;
  }
}
