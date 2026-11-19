// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0092 / `docs/ux/attachments.md` — the shapes the attachment screens and
 * bubbles exchange with the conversation that hosts them. The screens hold no
 * keys and call no server: they return a draft, and the conversation signs,
 * composes and sends it; the bubbles get a view already folded and report a
 * person's action back.
 */
import type {
  Article,
  ChecklistItemRef,
  SharedContactCard,
  SharedLocation,
} from "../../wasm/lakko_ffi_wasm";

export type {
  Article,
  ArticleBlock,
  ArticleRun,
  ChecklistBody,
  ChecklistItemRef,
  MessageContent,
  PollBody,
  SharedContactCard,
  SharedLocation,
  SignedChecklist,
  SignedPoll,
} from "../../wasm/lakko_ffi_wasm";

/** The nine paperclip items, in the core's names (`attachmentMenu`). */
export type AttachItem =
  | "photo-or-video"
  | "camera"
  | "file"
  | "audio"
  | "poll"
  | "checklist"
  | "location"
  | "contact"
  | "article";

/** Why a row is shown disabled (`attachmentMenu`'s `disabled`). */
export type AttachDisabled = "no-camera" | "camera-in-call" | "no-location";

/** One row of the paperclip menu, ready to draw. */
export interface AttachMenuEntry {
  readonly item: AttachItem;
  readonly section: number;
  readonly disabled: AttachDisabled | undefined;
}

/** What the poll editor shows about anonymity (`pollAnonymity`). */
export type PollAnonymityMode = "not-offered" | "switch-default-on" | "always-anonymous";

/** The poll editor's result. Bounds are the editor's (UX 7.1); the core
 *  re-checks every one when it signs. */
export interface PollDraft {
  readonly question: string;
  readonly options: readonly string[];
  readonly multiple: boolean;
  /** Quiz mode: the correct option's index and an optional explanation. */
  readonly quiz?: { readonly correct: number; readonly explanation: string };
  /** Close automatically after this many milliseconds from creation. */
  readonly closeAfterMs?: number;
  readonly anonymous: boolean;
}

/** One checklist item in an editor: `id` is set for an item that already
 *  exists (an edit keeps ticks with their ids), absent for a new one. */
export interface ChecklistDraftItem {
  readonly id?: number;
  readonly text: string;
}

export interface ChecklistDraft {
  readonly title: string;
  readonly othersAdd: boolean;
  readonly othersMark: boolean;
  readonly items: readonly ChecklistDraftItem[];
}

/** A contact the picker offers. */
export interface ShareableContact {
  readonly name: string;
  /** Canonical username, or absent (shown disabled: "No username to share"). */
  readonly username?: string;
  /** Standard base64 of the 32-byte account key. */
  readonly accountKeyB64: string;
  /** ADR-0092 section 7 — `false` when the newest profile held from the
   *  contact says their card may not be shared (shown disabled). Absent:
   *  shareable. */
  readonly cardShareable?: boolean;
}

/** The article editor's result: `article` whose image blocks index into
 *  `images` (same order); the conversation re-encodes and uploads them. */
export interface ArticleDraftOut {
  readonly article: Article;
  readonly images: readonly Blob[];
}

/** One picked item on the send preview. */
export interface PreviewItem {
  readonly file: File;
}

// ----- bubbles ---------------------------------------------------------

/** One option of a poll card, folded. */
export interface PollOptionView {
  readonly text: string;
  readonly count: number;
  /** 0..=100, rounded. */
  readonly percent: number;
  /** The person's own choice. */
  readonly mine: boolean;
  /** Quiz only, shown after voting or closing. */
  readonly correct?: boolean;
}

export interface PollView {
  readonly question: string;
  readonly anonymous: boolean;
  readonly quiz: boolean;
  readonly multiple: boolean;
  readonly options: readonly PollOptionView[];
  /** Distinct voters counted. */
  readonly voters: number;
  /** The person has a counted (or sending) vote. */
  readonly voted: boolean;
  readonly closed: boolean;
  /** Automatic close time, when open and set. */
  readonly closesAtMs?: number;
  /** Quiz explanation, shown after voting. */
  readonly explanation?: string;
  /** An anonymous vote waits in the queue. */
  readonly sending: boolean;
  /** The last vote could not be sent. */
  readonly failed: boolean;
  /** The person may vote now. */
  readonly canVote: boolean;
  /** Fail-closed: the poll's voter list did not verify (no totals, no voting). */
  readonly unverifiable: boolean;
  /** `View results` is offered (non-anonymous, voted or closed). */
  readonly canViewResults: boolean;
}

export interface ChecklistItemView {
  readonly ref: ChecklistItemRef;
  readonly text: string;
  readonly done: boolean;
  /** First name of who ticked it, when shown (UX 8.2). */
  readonly doneByName?: string;
  readonly canToggle: boolean;
}

export interface ChecklistView {
  readonly title: string;
  readonly items: readonly ChecklistItemView[];
  readonly doneCount: number;
  readonly canAdd: boolean;
  readonly closed: boolean;
}

/** How a shared contact card relates to this device. */
export type ContactCardRelation = "contact" | "self" | "other";

/** The person's share of a poll result screen (non-anonymous polls). */
export interface PollResultSection {
  readonly text: string;
  readonly count: number;
  readonly voters: readonly { readonly keyB64: string; readonly name: string }[];
}

export type { SharedContactCard as ContactCard, SharedLocation as Location };
