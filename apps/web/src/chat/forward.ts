// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 decision 2 / `docs/ux/message-actions.md` sections 5 and 5.1 —
 * what a forward SENDS, what a forwarded bubble SAYS, and how the attribution
 * is stored.
 *
 * **What is sent** (`forwardPayloadOf`): the original's text verbatim — the
 * edited text if it was edited, because that is what the person is looking
 * at — its attachments (the caller re-encrypts them; this module only says
 * which), and the attribution. The attribution is the ORIGINAL author's
 * display name as the forwarder's timeline showed it plus the origin
 * reference, and for a message that is itself a forward it is the name and
 * origin that message already carried, never the forwarder's — a chain does
 * not rewrite its own history. A reply's quote is not carried (a forward is a
 * new message, not an answer), and reactions are not carried.
 *
 * **What the bubble says** (`forwardLineLabel`): "Forwarded from <name>", or
 * "Forwarded" when no name was carried. The name is a label and nothing more
 * (section 5): the caller renders it as plain text with no link styling and
 * no verification mark.
 *
 * Pure: no wasm, no Solid, no network. The core truncates an over-long name
 * (255 bytes) on a character boundary, so counting bytes here would be a
 * second rule for the same thing.
 */
import { t } from "../l10n";
import type { AttachmentRefBytes, ForwardOrigin, ForwardRef } from "./attachments";
import type { PersistedForward } from "./message-log-store";

/** The message being forwarded, as the timeline holds it. */
export interface ForwardSource<A = AttachmentRefBytes> {
  /** The original text as sent. */
  readonly text: string;
  /** The edited text, when the message was edited (`msgOps`). */
  readonly editedText?: string | undefined;
  readonly attachments: readonly A[];
  /** The source's DS `messageId` — the origin reference for a first forward. */
  readonly messageId: number;
  /** Set when the source is itself a forward: its attribution is kept. */
  readonly forward?: ForwardRef | undefined;
}

/** What the forward composes and sends. */
export interface ForwardPayload<A = AttachmentRefBytes> {
  readonly text: string;
  /** The SOURCE refs. The sender re-encrypts each under a fresh key before
   *  composing (decision 2, OQ1): the two conversations never share a blob. */
  readonly attachments: readonly A[];
  readonly forward: ForwardRef;
}

/**
 * Section 5.1 "What is sent".
 *
 * `authorLabel` is the source's author as a RECIPIENT should read it: the
 * display resolver's name, or this account's own profile name / username for
 * an own message - never "You", which is the viewer's word and a lie on the
 * receiving screen; `undefined` when nobody can be named, so the bubble reads
 * "Forwarded". `originGroupIdB64` is the
 * conversation the source sits in — the forwarder is always a member of it,
 * so the origin can always be carried on a FIRST forward. For a forward of a
 * forward both are ignored in favour of what the source already carries,
 * including when the source carries no name or no origin: inventing either
 * would rewrite the chain.
 *
 * ADR-0053 section 6 / contract point 10: `sourceIsOneToOne` — a forward out
 * of a ONE-TO-ONE carries no origin at all, the chain's included. The only
 * other holder of a one-to-one is its other participant, so the origin could
 * never serve a jump for anyone the forward reaches, and it would carry the
 * conversation's id to people who were never in it. The name still travels.
 */
export function forwardPayloadOf<A>(
  source: ForwardSource<A>,
  authorLabel: string | undefined,
  originGroupIdB64: string,
  sourceIsOneToOne: boolean,
): ForwardPayload<A> {
  const text = source.editedText ?? source.text;
  const origin: ForwardOrigin | undefined = sourceIsOneToOne
    ? undefined
    : source.forward !== undefined
      ? source.forward.origin
      : { groupIdB64: originGroupIdB64, messageId: source.messageId };
  const author =
    source.forward !== undefined
      ? source.forward.author
      : authorLabel === undefined || authorLabel.trim() === ""
        ? undefined
        : authorLabel;
  const forward: ForwardRef = {
    ...(author === undefined ? {} : { author }),
    ...(origin === undefined ? {} : { origin }),
  };
  return { text, attachments: source.attachments, forward };
}

/** Section 5.1 "The bubble" — the one line, which is also the button's
 *  accessible name. */
export function forwardLineLabel(forward: Pick<ForwardRef, "author">): string {
  const name = forward.author ?? "";
  return name.length > 0 ? t("chat.forward.from", { name }) : t("chat.forward.label");
}

/**
 * Section 5.1 "Tap to jump" — where a tap on the line goes: the origin, when
 * the forward carries one AND this device holds that conversation; else
 * nowhere, silently (an explanation would confirm the conversation exists).
 * Whether the MESSAGE is held is the destination view's question, answered by
 * its own jump doing nothing when the row is absent.
 */
export function forwardJumpTarget(
  forward: Pick<ForwardRef, "origin">,
  holdsConversation: (groupIdB64: string) => boolean,
): ForwardOrigin | null {
  const origin = forward.origin;
  if (origin === undefined) return null;
  return holdsConversation(origin.groupIdB64) ? origin : null;
}

/**
 * ADR-0073 R.5 — the origin of a forward names a GROUP and a message id; the
 * conversation that group belongs to may since have continued in a successor
 * group. The jump then lands in an OLDER segment of the conversation, whose
 * rows the timeline draws under `rowId(messageId, distance)`
 * (`lineage.ts` `historyRowId`). This resolves the id of the row to jump to:
 *
 * | the origin group is ...                              | row to jump to                |
 * |------------------------------------------------------|-------------------------------|
 * | the conversation's current group                     | the message id                |
 * | an older segment this device holds (`d` before head) | `rowId(messageId, d)`         |
 * | anything else (a segment not held, beyond the head)  | `null`: nothing, silently     |
 *
 * `heldHistory` is the older segments this device holds, oldest first, as
 * the view lists them; `d` counts within THAT list, which is what the
 * timeline numbers its history rows by.
 */
export function forwardJumpRow(input: {
  readonly originGroupIdB64: string;
  readonly messageId: number;
  readonly headGroupIdB64: string;
  readonly heldHistory: readonly string[];
  readonly rowId: (messageId: number, distanceFromHead: number) => number;
}): number | null {
  if (input.originGroupIdB64 === input.headGroupIdB64) return input.messageId;
  const index = input.heldHistory.indexOf(input.originGroupIdB64);
  if (index === -1) return null;
  return input.rowId(input.messageId, input.heldHistory.length - index);
}

// ----- persistence -------------------------------------------------------

/** The on-disk shape of a forward attribution (`message-log-store.ts`). The
 *  origin message id is stringified like every DS id the log stores. */
export function persistedFromForwardRef(forward: ForwardRef): PersistedForward {
  return {
    ...(forward.author === undefined ? {} : { author: forward.author }),
    ...(forward.origin === undefined
      ? {}
      : {
          origin: {
            groupIdB64: forward.origin.groupIdB64,
            messageId: String(forward.origin.messageId),
          },
        }),
  };
}

/** Inverse of [`persistedFromForwardRef`]. An origin whose id does not parse
 *  (never written by this client, but a store is not a wire) reads as no
 *  origin rather than as message 0 / NaN; a row with neither half reads as
 *  a forward that carried nothing, which is what "Forwarded" is for. */
export function forwardRefFromPersisted(
  forward: PersistedForward | undefined,
): ForwardRef | undefined {
  if (forward === undefined) return undefined;
  const author = typeof forward.author === "string" ? forward.author : undefined;
  let origin: ForwardOrigin | undefined;
  const stored = forward.origin;
  if (stored !== undefined && typeof stored.groupIdB64 === "string") {
    const messageId = Number(stored.messageId);
    if (Number.isFinite(messageId)) origin = { groupIdB64: stored.groupIdB64, messageId };
  }
  return {
    ...(author === undefined ? {} : { author }),
    ...(origin === undefined ? {} : { origin }),
  };
}
