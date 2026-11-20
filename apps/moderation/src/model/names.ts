// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The names the panel API speaks (docs: panel-api, "Names on the wire") and
 * the English the panel shows for them. One table per vocabulary, so a code
 * and its label cannot drift apart in a screen.
 */

/** A report reason as the API names it. */
export type ReasonName =
  | "spam"
  | "harassment"
  | "hate"
  | "violence"
  | "sexual"
  | "child-safety"
  | "impersonation"
  | "other";

interface ReasonInfo {
  readonly name: ReasonName;
  readonly code: number;
  /** The label in filters and pickers (the reporter's sheet wording). */
  readonly label: string;
  /** The short label in queue rows ("Spam 14 · Harassment 2"). */
  readonly short: string;
  /** The words after "for" in a confirmation sentence. */
  readonly phrase: string;
}

/** The reasons in sheet order (UX section 2.2), with their wire codes. */
export const REASONS: readonly ReasonInfo[] = [
  { name: "spam", short: "Spam", code: 1, label: "Spam or scam", phrase: "spam or a scam" },
  { name: "harassment", short: "Harassment", code: 2, label: "Harassment or threats", phrase: "harassment or threats" },
  { name: "hate", short: "Hate", code: 3, label: "Hate", phrase: "hate" },
  { name: "violence", short: "Violence", code: 4, label: "Violence or graphic content", phrase: "violence or graphic content" },
  { name: "sexual", short: "Sexual content", code: 5, label: "Sexual content", phrase: "sexual content" },
  { name: "child-safety", short: "Child safety", code: 6, label: "A child's safety", phrase: "a child's safety" },
  { name: "impersonation", short: "Impersonation", code: 7, label: "Pretending to be someone else", phrase: "pretending to be someone else" },
  { name: "other", short: "Other", code: 8, label: "Something else", phrase: "another reason" },
];

export function reasonByName(name: string): ReasonInfo | undefined {
  return REASONS.find((r) => r.name === name);
}

export function reasonByCode(code: number): ReasonInfo | undefined {
  return REASONS.find((r) => r.code === code);
}

/** The label for a reason name, or the name itself for one the panel does not know. */
export function reasonLabel(name: string): string {
  return reasonByName(name)?.label ?? name;
}

/** The words after "for" for a reason name ("spam or a scam"). */
export function reasonPhrase(name: string): string {
  return reasonByName(name)?.phrase ?? name;
}

/** The short label for a reason name. */
export function reasonShort(name: string): string {
  return reasonByName(name)?.short ?? name;
}

/** A target kind as the API names it. */
export type TargetKindName = "account" | "group-handle" | "channel" | "name" | "blob";

interface TargetKindInfo {
  readonly name: TargetKindName;
  readonly byte: number;
  readonly length: number;
  readonly label: string;
  /** "this account", used in confirmation sentences. */
  readonly thisOne: string;
}

export const TARGET_KINDS: readonly TargetKindInfo[] = [
  { name: "account", byte: 1, length: 32, label: "Account", thisOne: "this account" },
  { name: "group-handle", byte: 2, length: 64, label: "Group", thisOne: "this group" },
  { name: "channel", byte: 3, length: 16, label: "Channel", thisOne: "this channel" },
  { name: "name", byte: 4, length: 64, label: "@username", thisOne: "this @username" },
  { name: "blob", byte: 5, length: 32, label: "File", thisOne: "this file" },
];

export function targetKind(name: string): TargetKindInfo | undefined {
  return TARGET_KINDS.find((k) => k.name === name);
}

export function targetKindByByte(byte: number): TargetKindInfo | undefined {
  return TARGET_KINDS.find((k) => k.byte === byte);
}

export function targetKindLabel(name: string): string {
  return targetKind(name)?.label ?? name;
}

/** The kinds a report can name (never a file), for the queue filter. */
export const REPORTABLE_KINDS: readonly TargetKindName[] = ["account", "group-handle", "channel", "name"];

/** A statement action as the API names it, with its wire byte and target kind. */
export type ActionName =
  | "suspend"
  | "lift"
  | "take-back-name"
  | "release-name"
  | "hide-from-search"
  | "show-in-search"
  | "channel-delist"
  | "channel-block-handle"
  | "channel-remove"
  | "channel-restore"
  | "group-delist"
  | "group-block-handle"
  | "group-restore"
  | "blob-delete"
  | "keep-suspension";

interface ActionInfo {
  readonly name: ActionName;
  readonly byte: number;
  readonly target: TargetKindName;
  /** How the log and the history name it. */
  readonly label: string;
}

export const ACTIONS: readonly ActionInfo[] = [
  { name: "suspend", byte: 1, target: "account", label: "Suspended" },
  { name: "lift", byte: 2, target: "account", label: "Suspension lifted" },
  { name: "take-back-name", byte: 3, target: "name", label: "Username taken back" },
  { name: "release-name", byte: 4, target: "name", label: "Username released" },
  { name: "hide-from-search", byte: 5, target: "account", label: "Hidden from search" },
  { name: "show-in-search", byte: 6, target: "account", label: "Shown in search again" },
  { name: "channel-delist", byte: 7, target: "channel", label: "Removed from Discover" },
  { name: "channel-block-handle", byte: 8, target: "channel", label: "Channel @name blocked" },
  { name: "channel-remove", byte: 9, target: "channel", label: "Channel removed" },
  { name: "channel-restore", byte: 10, target: "channel", label: "Channel restored" },
  { name: "group-delist", byte: 11, target: "group-handle", label: "Group removed from search" },
  { name: "group-block-handle", byte: 12, target: "group-handle", label: "Group @name blocked" },
  { name: "group-restore", byte: 13, target: "group-handle", label: "Group restored" },
  { name: "blob-delete", byte: 14, target: "blob", label: "File deleted" },
  { name: "keep-suspension", byte: 15, target: "account", label: "Suspension kept, appeal declined" },
];

export function action(name: string): ActionInfo | undefined {
  return ACTIONS.find((a) => a.name === name);
}

export function actionLabel(name: string): string {
  if (name === "no-action") return "No action";
  return action(name)?.label ?? name;
}

export const CASE_STATUSES = ["new", "in-review", "decided"] as const;
export type CaseStatus = (typeof CASE_STATUSES)[number];

export function caseStatusLabel(status: string): string {
  switch (status) {
    case "new":
      return "New";
    case "in-review":
      return "In review";
    case "decided":
      return "Decided";
    default:
      return status;
  }
}

export function reportStatusLabel(status: string): string {
  switch (status) {
    case "received":
      return "Open";
    case "action":
      return "Action taken";
    case "none":
      return "No action";
    default:
      return status;
  }
}

export function conversationKindLabel(kind: string): string {
  switch (kind) {
    case "one-to-one":
      return "One-to-one chat";
    case "group":
      return "Group";
    case "channel":
      return "Channel";
    case "contact-request":
      return "Contact request";
    case "discover":
      return "Discover";
    case "card":
      return "Profile card";
    default:
      return "Unknown";
  }
}

/** The account-age band auth reports, as UX section 5.3 words it. */
export function ageBandLabel(band: string | null): string {
  switch (band) {
    case "day":
      return "Under a day old";
    case "week":
      return "Under a week old";
    case "month":
      return "Under a month old";
    case "older":
      return "Older than a month";
    default:
      return "Age unknown";
  }
}
