// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One timeline row (docs/ux/client-redesign.md section 5.2): a bubble on the
 * sunken surface to the left for an incoming message, on the accent to the
 * right for the reader's own (including one sent from another device of the
 * same account), the time in its lower corner and, on own messages, a
 * delivery glyph (one check sent, two read); the sender's name and avatar
 * once per run in a group; system rows centred and small.
 *
 * Lifted out of `routes/Chat.tsx`. Every behaviour — the op overlay (edited
 * text, deleted tombstone, reaction chips), the action menu, the reaction
 * pickers, the media bubbles and the file chips — is the ADR-0023 / ADR-0024
 * / ADR-0047 one; only the chrome is here. The date / run computation the
 * caller feeds in is `timeline-layout.ts`.
 *
 * The one behaviour that did change is HOW the actions are reached. They used
 * to be a strip revealed on `:hover` and positioned inside the bubble, which
 * put them over the bubble's own controls — on a voice message, over the play
 * button, so pressing play paused it. They are now `MessageMenu.tsx`, opened
 * on a deliberate gesture and rendered outside the bubble.
 */
import { createSignal, For, Index, onCleanup, Show, type JSX } from "solid-js";

import type { MemberIdentity } from "./author-identity";
import {
  formatAttachmentSize,
  type AttachmentRefBytes,
  type ForwardOrigin,
  type ForwardRef,
  type ReplyRef,
} from "./attachments";
import { useChats } from "./ChatContext";
import { copyMessageText } from "./copy-message";
import { OWN_REACTION_SENTINEL, type MessageOpState } from "./message-ops";
import {
  LONG_PRESS_MS,
  LONG_PRESS_SLOP_PX,
  MessageMenu,
  type MenuAnchor,
} from "./MessageMenu";
import { forwardLineLabel } from "./forward";
import { ownPersonKeyB64 } from "./person-key";
import { replyQuoteLabel, replyQuoteLines } from "./reply-snapshot";
import { canSelectRow, selectionMarkName } from "./selection";
import { NotSent } from "../media/MediaBubbles";
import { shortTime } from "./timeline-layout";
import type { TimelineRow } from "./timeline-row";
import { EmojiPicker } from "../media/EmojiPicker";
import { MediaAttachment, type MediaUpload } from "../media/MediaBubbles";
import {
  bubbleMetaSlot,
  soleMediaBubble,
  type BubbleMetaSlot,
  type MediaBubble,
} from "../media/inline-media";
import { allowlist, mediaKey, type MediaHub } from "../media/media-hub";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { currentLanguage, joinList, t } from "../l10n";

/** The callbacks and reactive reads a bubble needs from the chat view. */
export interface BubbleHost {
  onDownload: (ref: AttachmentRefBytes) => void | Promise<void>;
  onPreview: (ref: AttachmentRefBytes) => void | Promise<void>;
  onHidePreview: (ref: AttachmentRefBytes) => void;
  getCachedUrl: (ref: AttachmentRefBytes) => string | null;
  /** ADR-0023 — our own signature key (for the own-reaction
   *  highlight). Not our leaf: a leaf we compare against a stored
   *  reactor would start matching a stranger after a renumber. */
  ownIdentity: MemberIdentity | undefined;
  /** Reactive overlay accessor for a given target messageId. */
  opState: (messageId: number) => MessageOpState | undefined;
  /** ADR-0024 — reactive read-state accessor: has this messageId
   *  been read by EVERY other member? Drives the own-message
   *  Sent -> Read indicator. */
  readByOthers: (messageId: number) => boolean;
  /** ADR-0048 — the members whose read watermark covers this message, for the
   *  `Read by ...` label. */
  readersOf: (messageId: number) => MemberIdentity[];
  editingId: number | null;
  editText: string;
  setEditText: (v: string) => void;
  onStartEdit: (messageId: number, current: string) => void;
  onCancelEdit: () => void;
  onSaveEdit: (messageId: number) => void | Promise<void>;
  onDeleteMessage: (messageId: number) => void | Promise<void>;
  onReact: (messageId: number, emoji: string) => void | Promise<void>;
  /** ADR-0052 — Reply was chosen on this message: the view snapshots it and
   *  arms the composer strip. */
  onReply: (messageId: number) => void;
  /** ADR-0052 — the quote block was pressed: scroll to the quoted message if
   *  the timeline holds it, else do nothing. */
  onJumpTo: (messageId: number) => void;
  /** ADR-0052 decision 2 — Forward was chosen on this message: the view opens
   *  the "Forward to" picker for it. */
  onForward: (messageId: number) => void;
  /** ADR-0052 decision 3 — the word the pin row reads for this message
   *  (`pin-sheet.ts`), reactive: `Pin` or `Unpin`. */
  pinLabelOf: (messageId: number) => "Pin" | "Unpin";
  /** ADR-0052 decision 3 — the pin row was chosen on this message. */
  onPin: (messageId: number) => void;
  /** ADR-0052 decision 2 — the "Forwarded from" line was pressed. The view
   *  goes to the original when it holds the origin conversation, else does
   *  nothing and says nothing; `undefined` when the forward carried no
   *  origin, which is the same nothing. */
  onOpenForwardOrigin: (origin: ForwardOrigin | undefined) => void;
  /** ADR-0045 UX section 6.2.5 — whether Edit / Delete / React are offered at
   *  all. `false` once this device's membership has ended: all three compose
   *  an application message, so all three are refused by the same core gate
   *  that refuses Send. Threaded as a prop rather than read here so the two
   *  bubble components stay pure renderers of what `membership-ended.ts`
   *  decided. */
  actionsAvailable: boolean;
  /** ADR-0047 — the media runtime, this row's upload state and its retry. */
  hub: MediaHub;
  mediaUpload: MediaUpload;
  /** Send this row again. One handler for both kinds of outgoing row: from
   *  the bubble a failed take and a failed composer message are the same
   *  gesture, and the person cannot tell them apart. */
  onRetrySend: () => void;
  /** ADR-0047 — the menu's "+" opens the full picker for this id. */
  fullReactPickerId: number | null;
  onOpenFullReactPicker: (messageId: number) => void;
  onCloseFullReactPicker: () => void;
  /** `message-actions.md` section 8 — selection mode. While `selecting`,
   *  every selectable row shows a mark at its leading edge and the whole row
   *  toggles; the action menu does not open. The set itself stays in the
   *  view: the bubble asks whether ITS id is in it and reports a toggle. */
  selecting: boolean;
  isSelected: (messageId: number) => boolean;
  onToggleSelect: (messageId: number) => void;
  /** Section 8 — `Select` was chosen in the card: enter the mode with this
   *  message already selected. */
  onSelect: (messageId: number) => void;
}

export function TimelineBubble(
  props: BubbleHost & {
    row: TimelineRow;
    /** A group timeline: the sender is named and shown once per run. A 1x1
     *  has one other person and shows neither. */
    isGroup: boolean;
    /** First / last row of a run of one sender within five minutes
     *  (`timeline-layout.ts`): the name and avatar go on the first, the tail
     *  radius on the last, tighter spacing inside. */
    startsRun: boolean;
    endsRun: boolean;
  },
): JSX.Element {
  // ADR-0048 — the author label goes through the one display resolver.
  const profiles = useProfiles();
  // ADR-0050 — a message from this PERSON's other device arrives as an
  // incoming row (another leaf sealed it) but is ours: same side, no sender
  // name, exactly as if this device had sent it. The person key is the
  // account key both leaves resolve to.
  const chatsForOwn = useChats();
  const fromOwnDevice = (senderKey: string | undefined): boolean =>
    senderKey !== undefined && senderKey === ownPersonKeyB64(chatsForOwn.identity());
  // System rows + call signaling never carry an op overlay.
  if (props.row.kind === "incoming") {
    const ev = props.row.event;
    if (ev.kind === "system") {
      return (
        <li class="lk-system-row">
          <span>{ev.systemKind === "commit" ? t("chat.system.members_changed") : t("chat.system.join_request")}</span>
        </li>
      );
    }
    if (ev.kind === "callSignaling") {
      // Phase 3a8 — call signaling rides the same MLS group
      // as chat messages, but it isn't part of the visible
      // timeline. The CallContext upstream already routed
      // it into the state machine; nothing renders here.
      return null;
    }
    if (ev.kind === "op") {
      // ADR-0023 — ops are folded into the target message's
      // overlay by the receive loop, never rendered as a
      // standalone bubble. (Defensive: ops don't enter `rows()`,
      // but keep the timeline render total.)
      return null;
    }
    if (ev.kind === "externalJoinRequest") {
      // ADR-0027 (G-3) — surfaced in the members panel, never a timeline
      // bubble (the receive loop skips it before `rows()`). Defensive.
      return null;
    }
    if (ev.kind === "pushRoutingSecret") {
      // ADR-0042 — the receive loop consumes it (persist + subscribe) and
      // never pushes it into `rows()`. Defensive.
      return null;
    }
    if (ev.kind === "committerBeacon") {
      // ADR-0045 — the receive loop consumes it and never pushes it into
      // `rows()`. Defensive.
      return null;
    }
    if (ev.kind === "profile") {
      // ADR-0048 — filed in the profile store by the receive loop, never
      // pushed into `rows()`. Defensive.
      return null;
    }
    if (ev.kind === "profileRequest") {
      // ADR-0048 §2 trigger 4 — answered by the profile provider, never
      // pushed into `rows()`. Defensive.
      return null;
    }
    if (ev.kind === "group-name") {
      // ADR-0051 — consumed by the group-name gate, never pushed into
      // `rows()`. Defensive.
      return null;
    }
    if (ev.kind === "pin") {
      // ADR-0052 decision 3 — consumed by the pin gate, never pushed into
      // `rows()`. Defensive.
      return null;
    }
    if (ev.kind === "shared-channels") {
      // ADR-0059 section 6 — filed in the shared-channels store, never
      // pushed into `rows()`. Defensive.
      return null;
    }
    if (ev.kind === "scopedDiscovery") {
      // ADR-0060 — dispatched to its handler by the receive loop, never
      // pushed into `rows()`. Defensive.
      return null;
    }
    if (ev.kind === "registrarSeed") {
      // ADR-0057 — consumed at receipt by the chat client, never a row.
      // Defensive.
      return null;
    }
    if (ev.kind === "serviceEnvelope") {
      // An unrecognised `lakko/…` service envelope, dropped at receipt by the
      // chat client and never a bubble. Defensive.
      return null;
    }
    if (ev.kind === "membershipProposal") {
      // ADR-0045 UX section 10 — a request is not something that HAPPENED to
      // the group; only outcomes belong in the timeline. The receive loop
      // routes it to the members panel and never pushes it into `rows()`.
      // Defensive.
      return null;
    }
    const own = fromOwnDevice(ev.senderKey);
    const senderName = (): string =>
      ev.senderKey !== undefined ? profiles.resolve(ev.senderKey).name : t("chat.author.unknown");
    // Incoming application message — never our own -> Edit/Delete
    // are not offered (the author-only gate would reject our op
    // anyway); React is.
    return (
      <ChatBubbleBody
        messageId={ev.messageId}
        isOwn={false}
        ack="sent"
        originalText={ev.text}
        attachments={ev.attachments}
        time={shortTime(ev.createdAt, currentLanguage())}
        senderName={props.isGroup && props.startsRun && !own ? senderName() : null}
        avatar={
          props.isGroup && !own
            ? props.startsRun
              ? { label: senderName(), id: ev.senderKey ?? "" }
              : "gutter"
            : null
        }
        tone={own ? "outgoing" : "incoming"}
        reply={ev.reply}
        forward={ev.forward}
        unsupported={ev.unsupportedVersion !== undefined}
        {...props}
      />
    );
  }
  // Outgoing — our own message. Edit/Delete offered once acked
  // (a pending bubble has no messageId yet).
  //
  // Read through `live()` rather than captured, because an outgoing row object
  // is REPLACED on every state change — `{...r, ack}` on the ack flip, and a
  // whole new row when an upload lands — and the timeline now keeps this bubble
  // alive across that replacement instead of rebuilding it
  // (`call-timeline.ts`). Captured, the bubble would go on showing the row it
  // was built with: `Not sent` under a message that has been sent. The reads
  // below are all inside JSX expressions, which is what makes them reactive.
  const built = props.row;
  const live = (): typeof built => (props.row.kind === "outgoing" ? props.row : built);
  return (
    <ChatBubbleBody
      messageId={live().ack === "sent" ? live().messageId : undefined}
      isOwn={true}
      ack={live().ack}
      originalText={live().plaintext}
      attachments={live().attachments}
      time={
        live().ack === "pending"
          ? t("chat.message.sending")
          : live().ack === "failed"
            ? t("chat.message.not_sent")
            : shortTime(live().createdAt, currentLanguage())
      }
      senderName={null}
      avatar={null}
      tone="outgoing"
      mediaLocalKey={live().mediaLocalKey}
      reply={live().reply}
      forward={live().forward}
      {...props}
    />
  );
}

/** The bubble's own interactive parts: a voice message's play button, speed
 *  chip and waveform, a video note's surface, an attachment chip's actions. */
const BUBBLE_CONTROLS = "button, a, input, textarea, video, [role='slider']";

function startsOnControl(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(BUBBLE_CONTROLS) !== null;
}

/**
 * ADR-0023 — the shared bubble body for both incoming + outgoing
 * messages, carrying the op overlay (edited text / deleted
 * tombstone / reaction chips) and the per-message action menu.
 *
 * Reactivity: `opState(messageId)` is read inside the render so a
 * later op fold (or a local own-op apply) re-renders just this
 * bubble. The action affordances are gated on `messageId !==
 * undefined` (an un-acked outgoing bubble has no DS id to
 * reference yet).
 */
function ChatBubbleBody(
  props: BubbleHost & {
    messageId: number | undefined;
    isOwn: boolean;
    ack: "pending" | "sent" | "failed";
    originalText: string;
    attachments: AttachmentRefBytes[];
    /** ADR-0047 — an outgoing media message's local key before upload. */
    mediaLocalKey?: string | undefined;
    /** ADR-0052 — the reply reference this message carries, drawn as the
     *  quote block above the text. The snapshot, never the original. */
    reply?: ReplyRef | undefined;
    /** ADR-0052 decision 2 — the forward attribution this message carries,
     *  drawn as one muted line above the content. A label, never a person. */
    forward?: ForwardRef | undefined;
    /** ADR-0061 section 6 — a message from a newer Lakko this build cannot
     *  read: the bubble says so in the catalog's words, in the language
     *  current when drawn. */
    unsupported?: boolean | undefined;
    /** The local short time (`17:21`), or the pending / failed word. */
    time: string;
    /** The sender's name above the bubble, or `null` (a 1x1, a run, own). */
    senderName: string | null;
    /** The avatar beside an incoming group bubble on the first row of a run,
     *  an empty gutter of the same width on the rest, nothing in a 1x1. */
    avatar: { label: string; id: string } | "gutter" | null;
    tone: "incoming" | "outgoing";
    startsRun: boolean;
    endsRun: boolean;
  },
): JSX.Element {
  // ADR-0048 — reactors and readers are named through the one display resolver.
  const profiles = useProfiles();
  const isIncoming = props.tone === "incoming";
  // The live overlay for this message (reactive). `undefined`
  // until an op lands.
  const state = (): MessageOpState | undefined =>
    props.messageId === undefined ? undefined : props.opState(props.messageId);
  const isDeleted = (): boolean => state()?.deleted === true;
  const editedText = (): string | undefined => state()?.editedText;
  const displayText = (): string => editedText() ?? props.originalText;
  const isEdited = (): boolean => editedText() !== undefined;
  const isEditing = (): boolean =>
    props.messageId !== undefined && props.editingId === props.messageId;

  // ----- the action menu (see `MessageMenu.tsx`) ---------------------------
  //
  // Local, not hoisted to the chat view: only one menu is ever open, and the
  // one thing the view would gain by owning the id — closing a stale menu —
  // the backdrop, Escape and the scroll listener already do.
  const [menuAnchor, setMenuAnchor] = createSignal<MenuAnchor | null>(null);
  let bubbleEl: HTMLDivElement | undefined;
  // Same predicate the strip carried: a DS messageId to reference, a live
  // membership (ADR-0045 UX §6.2.5 — Edit / Delete / React all compose an
  // application message, so all three die with Send), and not mid-edit.
  const menuAvailable = (): boolean =>
    props.actionsAvailable &&
    props.messageId !== undefined &&
    !isEditing() &&
    !isDeleted() &&
    // ADR-0061 section 6 — a message from a newer wire version has no content
    // here to reply to, forward, copy or react to; it behaves like a tombstone.
    props.unsupported !== true &&
    // Section 8 "Entering": "The reaction row and the action card do not open
    // while selecting; a long press or right-click on a row toggles it".
    !props.selecting;

  // Section 8 "Selecting" — the same gate the single actions compute, read
  // here for the mark and the row's tap. Not `menuAvailable`: that one also
  // answers "not while selecting", and this must answer during it.
  const selectable = (): boolean =>
    canSelectRow({
      hasId: props.messageId !== undefined,
      deleted: isDeleted() || props.unsupported === true,
      sendComplete: props.ack === "sent",
    });
  const rowSelectable = (): boolean => props.selecting && selectable();
  const rowSelected = (): boolean =>
    rowSelectable() && props.messageId !== undefined && props.isSelected(props.messageId);
  const toggleRow = (): void => {
    if (rowSelectable() && props.messageId !== undefined) props.onToggleSelect(props.messageId);
  };

  function openMenu(x: number, y: number): void {
    if (!menuAvailable() || bubbleEl === undefined) return;
    const rect = bubbleEl.getBoundingClientRect();
    setMenuAnchor({
      bubble: { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom },
      x,
      y,
      // The empty half of the row is opposite the bubble: to the right of an
      // incoming one, to the left of our own.
    });
  }

  // A touch screen has no right-click, so the deliberate act is a long press.
  // Tracked here rather than in the menu because the gesture belongs to the
  // bubble and must be abandoned the moment the finger starts scrolling.
  let pressTimer: ReturnType<typeof setTimeout> | null = null;
  let pressAt: { x: number; y: number } | null = null;
  const cancelPress = (): void => {
    if (pressTimer !== null) clearTimeout(pressTimer);
    pressTimer = null;
    pressAt = null;
  };
  onCleanup(cancelPress);

  // Reactions sorted for a stable chip order.
  const reactionEntries = (): Array<{
    emoji: string;
    count: number;
    mine: boolean;
    reactedBy: string;
  }> => {
    const m = state()?.reactions;
    if (m === undefined) return [];
    const own = props.ownIdentity ?? OWN_REACTION_SENTINEL;
    return [...m.entries()]
      .map(([emoji, reactors]) => ({
        emoji,
        count: reactors.size,
        mine: reactors.has(own),
        // ADR-0048 UX section 5 — "Reacted by ...", names through the resolver.
        reactedBy: joinList(
          [...reactors].map((r) =>
            r === own
              ? t("chat.author.you")
              : r === OWN_REACTION_SENTINEL
                ? t("chat.author.a_member")
                : profiles.resolve(r).name,
          ),
        ),
      }))
      .filter((e) => e.count > 0)
      .sort((a, b) => a.emoji.localeCompare(b.emoji));
  };

  // ADR-0047 — media attachments render as their own bubbles; anything the
  // hub does not accept as media stays a file chip.
  const mediaRefs = (): AttachmentRefBytes[] =>
    props.attachments.filter((a) => props.hub.decision(a).bubble !== "file");
  const fileRefs = (): AttachmentRefBytes[] =>
    props.attachments.filter((a) => props.hub.decision(a).bubble === "file");
  /**
   * The bubble of a message that is nothing but one piece of media, and where
   * its meta line goes — ONE rule, in `inline-media.ts`, rather than the
   * conditionals that used to encode it here.
   *
   * THE DEFECT: there were two of them, `chromeless` and `mediaOnly`, they did
   * not even agree on whether to read the edited text or the original, and a
   * lone voice message fell through both — so it got the bubble's own row
   * under the player, putting the length of the recording and the time it was
   * sent on two different lines. UX spec §2 puts them on one.
   */
  // ADR-0052 — a reply is never "nothing but media": its quote block needs
  // the bubble's own surface to sit on, so a voice message or a photo sent as
  // a reply keeps the ordinary chrome. A forward keeps it for the same reason:
  // its "Forwarded from" line sits inside the bubble.
  const sole = (): MediaBubble | null =>
    props.reply !== undefined || props.forward !== undefined
      ? null
      : soleMediaBubble(
          {
            // An open edit composer replaces the body, so the message is not
            // "nothing but media" while it is up.
            deleted: isDeleted() || isEditing(),
            text: displayText(),
            attachments: props.attachments,
          },
          // The same module-level allowlist the hub itself is built from
          // (`media-hub.ts`), so the shape decision and the slot decision can
          // never be made against two different answers.
          allowlist,
        );
  const metaSlot = (): BubbleMetaSlot => bubbleMetaSlot(sole());
  // UX spec section 3 — a video note is a circle with no bubble chrome.
  // UX spec section 10 — a lone photo or video fills its bubble, and the time
  // and ticks sit on the media in a translucent pill.
  const mediaOnly = (): boolean => metaSlot() === "media-pill";
  // Kept apart from `mediaOnly` on this client: `.lk-bubble.media-only` draws
  // its own surface and `.lk-bubble.chromeless` removes one, so unlike the
  // natives' single flag these two are exclusive here.
  const chromeless = (): boolean => metaSlot() === "below-bubble";
  const keyFor = (ref: AttachmentRefBytes): string =>
    ref.blobId.length === 0 && props.mediaLocalKey !== undefined
      ? props.mediaLocalKey
      : mediaKey(ref.blobId);
  // A pending media message keeps full opacity: its upload ring is the
  // progress signal, and a dimmed video would read as disabled.
  const pending = (): boolean => props.ack === "pending" && props.mediaLocalKey === undefined;

  /**
   * The message's own line: the time (or the retry affordance in its place),
   * and on our own messages the delivery glyph.
   *
   * A FUNCTION, not a stored element: Solid's JSX creates the DOM node where
   * it is evaluated, so one element placed in two branches would be MOVED
   * between them rather than duplicated. Each call builds its own — the same
   * reason the reaction chip below is written this way.
   */
  const metaLine = (): JSX.Element => (
    <div class="lk-bubble-meta">
      {/* A failed row offers Retry where the message is. The media
          bubbles draw their own, so this is for a row that has none:
          before it, a plain text message that did not send showed the
          words "Not sent" and no way to act on them. */}
      <Show
        when={props.ack === "failed" && mediaRefs().length === 0}
        fallback={<span>{props.time}</span>}
      >
        <NotSent onRetry={() => props.onRetrySend()} />
      </Show>
      {/* ADR-0024 — read state on our OWN acked messages. A single
          check once published ("Sent"); a double check once
          EVERY other member's read watermark covers this message
          ("Read"). Gated on a present messageId. */}
      <Show when={props.isOwn && props.ack === "sent" && props.messageId !== undefined}>
        {(() => {
          const read = (): boolean =>
            props.messageId !== undefined && props.readByOthers(props.messageId);
          // ADR-0048 UX section 5 — read receipts name the readers.
          const readers = (): string =>
            props.messageId === undefined
              ? ""
              : joinList(props.readersOf(props.messageId).map((r) => profiles.resolve(r).name));
          return (
            <span
              class="lk-bubble-ack"
              classList={{ read: read() }}
              title={
                read() && readers().length > 0
                  ? t("chat.message.read_by", { names: readers() })
                  : read()
                    ? t("chat.message.read")
                    : t("chat.message.sent")
              }
              aria-label={read() ? t("chat.message.read") : t("chat.message.sent")}
            >
              <Icon name={read() ? "dcheck" : "check"} size={14} />
            </span>
          );
        })()}
      </Show>
    </div>
  );

  return (
    <li
      class="lk-bubble-row"
      // ADR-0052 — what a quote block's jump looks the row up by. A DS id,
      // never a tempId: an un-acked outgoing row has nothing to be quoted by.
      data-message-id={props.messageId}
      classList={{
        incoming: isIncoming,
        outgoing: !isIncoming,
        "run-start": props.startsRun,
        "run-end": props.endsRun,
        selectable: rowSelectable(),
        selected: rowSelected(),
      }}
      /* Section 8 — "the whole row is the tap target", and on web a
         right-click "toggles it like a tap". The bubble's own controls are
         inert while selecting (`.lk-timeline.selecting`), so a press anywhere
         on the row lands here. A row that is not selectable takes nothing. */
      onClick={() => {
        if (rowSelectable()) toggleRow();
      }}
      onContextMenu={(e) => {
        if (!props.selecting) return;
        e.preventDefault();
        toggleRow();
      }}
    >
      {/* Section 8 "Selecting" — the mark at the leading edge: an empty
          circle, filled with a check when selected. A toggle for
          accessibility purposes, named by the message's text or kind, its
          state in `aria-checked`. A row that cannot be acted on shows NO mark;
          it keeps the mark's width so the bubbles stay in one column. */}
      <Show when={props.selecting}>
        <Show
          when={selectable()}
          fallback={<span class="lk-select-mark-slot" aria-hidden="true" />}
        >
          <button
            type="button"
            class="lk-select-mark"
            role="checkbox"
            aria-checked={rowSelected()}
            aria-label={selectionMarkName(
              { text: displayText(), attachments: props.attachments },
              (a) => props.hub.decision(a).bubble,
            )}
            onClick={(e) => {
              // The row's own handler would toggle it back.
              e.stopPropagation();
              toggleRow();
            }}
          >
            <Show when={rowSelected()}>
              <Icon name="check" size={14} strokeWidth={2.4} />
            </Show>
          </button>
        </Show>
      </Show>
      <Show when={props.avatar !== null}>
        <span class="lk-bubble-gutter">
          <Show when={props.avatar !== "gutter" && props.avatar}>
            {(a) => <Avatar label={a().label} id={a().id} size="sm" />}
          </Show>
        </span>
      </Show>
      <div
        ref={bubbleEl}
        class="lk-bubble"
        classList={{
          incoming: isIncoming,
          outgoing: !isIncoming,
          chromeless: chromeless(),
          "media-only": mediaOnly(),
          pending: pending(),
          "run-start": props.startsRun,
          "run-end": props.endsRun,
        }}
        /* The spec says the keyboard reaches the menu "on the focused
           bubble", so the bubble is focusable. It costs one Tab stop per
           message and buys the first keyboard path there has ever been to
           these actions on a plain text message: `:focus-within` only ever
           fired for a bubble that happened to contain a focusable control. */
        tabindex={menuAvailable() ? 0 : undefined}
        onContextMenu={(e) => {
          if (!menuAvailable()) return;
          // Both the mouse's right-click and the keyboard's context-menu key /
          // Shift+F10 arrive here in the browsers that synthesise them; the
          // keydown handler below covers the ones that do not.
          e.preventDefault();
          openMenu(e.clientX, e.clientY);
        }}
        onKeyDown={(e) => {
          if (!menuAvailable()) return;
          if (e.key !== "ContextMenu" && !(e.key === "F10" && e.shiftKey)) return;
          e.preventDefault();
          const rect = bubbleEl?.getBoundingClientRect();
          openMenu(rect?.left ?? 0, rect?.top ?? 0);
        }}
        onPointerDown={(e) => {
          if (e.pointerType !== "touch" || !menuAvailable()) return;
          // A long press that STARTS on one of the bubble's own controls
          // belongs to that control, not to the message: holding the
          // waveform is a scrub, holding play is still play. This is the
          // touch half of the defect — the menu must not take a gesture the
          // person aimed at something else. A right-click still opens the
          // menu anywhere, because it activates nothing on the way.
          if (startsOnControl(e.target)) return;
          pressAt = { x: e.clientX, y: e.clientY };
          pressTimer = setTimeout(() => {
            const at = pressAt;
            cancelPress();
            if (at !== null) openMenu(at.x, at.y);
          }, LONG_PRESS_MS);
        }}
        onPointerMove={(e) => {
          const at = pressAt;
          if (at === null) return;
          // A finger that has started travelling is scrolling the timeline.
          if (
            Math.abs(e.clientX - at.x) > LONG_PRESS_SLOP_PX ||
            Math.abs(e.clientY - at.y) > LONG_PRESS_SLOP_PX
          ) {
            cancelPress();
          }
        }}
        onPointerUp={cancelPress}
        onPointerCancel={cancelPress}
      >
        <Show when={props.senderName}>{(n) => <div class="lk-sender">{n()}</div>}</Show>
        <Show
          when={!isDeleted()}
          fallback={<div class="lk-bubble-deleted">{t("chat.message.deleted")}</div>}
        >
          <Show
            when={isEditing()}
            fallback={
              <>
                {/* ADR-0052 / section 3.1 — the quote block, above the text,
                    inside the bubble, in the bubble's own colours: the rule,
                    the name, the excerpt, each line truncated. It draws the
                    snapshot the envelope carries and never re-reads the
                    original. A button, named for accessibility, that jumps
                    to the quoted message when this timeline holds it. */}
                {/* ADR-0052 decision 2 / section 5.1 — the forward line: one
                    muted line above the content, a small forward arrow, then
                    "Forwarded from <name>" or "Forwarded". Plain text - no
                    avatar, no username, no link styling: the name is a label,
                    not a person (section 5). A button named for
                    accessibility; the view decides whether the tap goes
                    anywhere, and says nothing when it does not. */}
                <Show when={props.forward}>
                  {(forward) => (
                    <button
                      type="button"
                      class="lk-forward-line"
                      aria-label={forwardLineLabel(forward())}
                      onClick={() => props.onOpenForwardOrigin(forward().origin)}
                    >
                      <Icon name="forward" size={14} />
                      <span class="lk-forward-line-text">{forwardLineLabel(forward())}</span>
                    </button>
                  )}
                </Show>
                <Show when={props.reply}>
                  {(reply) => {
                    const lines = (): ReturnType<typeof replyQuoteLines> =>
                      replyQuoteLines(reply());
                    return (
                      <button
                        type="button"
                        class="lk-reply-quote"
                        aria-label={replyQuoteLabel(lines())}
                        onClick={() => props.onJumpTo(reply().target)}
                      >
                        <span class="lk-reply-quote-name">{lines().name}</span>
                        <span class="lk-reply-quote-excerpt">{lines().excerpt}</span>
                      </button>
                    );
                  }}
                </Show>
                <Show when={props.unsupported === true}>
                  <div class="lk-bubble-deleted">{t("chat.message.unsupported")}</div>
                </Show>
                <Show when={displayText().length > 0}>
                  <div class="lk-bubble-text">
                    {displayText()}
                    <Show when={isEdited()}>
                      <span class="lk-bubble-edited">{t("chat.message.edited")}</span>
                    </Show>
                  </div>
                </Show>
                <Show when={mediaRefs().length > 0}>
                  <div
                    class="lk-bubble-media"
                    classList={{ "after-text": displayText().length > 0 }}
                  >
                    {/* `Index`, not `For`, and it is the media bubble's
                        lifetime that turns on it. A message's attachments are
                        fixed in number and order for as long as it exists, so
                        slot 0 always means the same attachment — while the REF
                        object at that slot is replaced when an outgoing take's
                        upload lands (the placeholder gives way to the uploaded
                        ref). `For` reconciles by reference, so that replacement
                        discarded the bubble and, with it, the element playing
                        the message the person was listening to. */}
                    <Index each={mediaRefs()}>
                      {(ref) => (
                        <MediaAttachment
                          attachment={ref()}
                          hub={props.hub}
                          mediaKey={keyFor(ref())}
                          incoming={isIncoming}
                          upload={props.mediaUpload}
                          onRetry={props.onRetrySend}
                          // UX spec §2 — a voice message alone in its bubble
                          // carries the time on its own quiet line. The slot
                          // decides; `VoiceBubble` is the only bubble that
                          // reads it, and the branch above is what keeps the
                          // row from being drawn twice.
                          meta={metaSlot() === "voice-line" ? metaLine : undefined}
                        />
                      )}
                    </Index>
                  </div>
                </Show>
                <Show when={fileRefs().length > 0}>
                  <AttachmentChips
                    hub={props.hub}
                    attachments={fileRefs()}
                    onDownload={props.onDownload}
                    onPreview={props.onPreview}
                    onHidePreview={props.onHidePreview}
                    getCachedUrl={props.getCachedUrl}
                    tone={props.tone}
                  />
                </Show>
              </>
            }
          >
            {/* Inline edit composer — pre-filled with the current
                text; Save publishes the edit op, Cancel discards. */}
            <div class="lk-bubble-edit">
              <textarea
                value={props.editText}
                onInput={(e) => props.setEditText(e.currentTarget.value)}
                rows={2}
              />
              <div class="lk-bubble-edit-actions">
                <button
                  type="button"
                  onClick={() => {
                    if (props.messageId !== undefined) {
                      void props.onSaveEdit(props.messageId);
                    }
                  }}
                >
                  {t("common.save")}
                </button>
                <button type="button" onClick={() => props.onCancelEdit()}>
                  {t("common.cancel")}
                </button>
              </div>
            </div>
          </Show>
        </Show>

        {/* Reaction chips — rendered under the body (incl. on a
            deleted message: a reaction can outlive the text). */}
        <Show when={reactionEntries().length > 0}>
          <div class="lk-reactions">
            <For each={reactionEntries()}>
              {(re) => {
                // The chip carries BOTH a fact and an affordance, and
                // ADR-0045 UX section 6.2.5 separates them: the count is
                // transcript — part of the history section 6.2.6 keeps whole
                // — while tapping it is the React op, which dies with Send.
                // So once the membership has ended the chip renders as
                // static text rather than as a button. It is not rendered
                // disabled, for the same reason the composer is not: a
                // greyed control on a group you will never be in again
                // promises a future it does not have.
                //
                // A FUNCTION, not a stored element: Solid's JSX creates the
                // DOM node where it is evaluated, so a single shared node
                // placed in both branches would be MOVED between them rather
                // than duplicated. Each branch builds its own.
                const chipBody = (): JSX.Element => (
                  <>
                    <span>{re.emoji}</span>
                    <span class="lk-reaction-count">{re.count}</span>
                  </>
                );
                return (
                  <Show
                    when={props.actionsAvailable}
                    fallback={
                      <span
                        class="lk-reaction"
                        classList={{ mine: re.mine }}
                        title={t("chat.reaction.reacted_by", { names: re.reactedBy })}
                      >
                        {chipBody()}
                      </span>
                    }
                  >
                    <button
                      type="button"
                      class="lk-reaction"
                      classList={{ mine: re.mine }}
                      title={re.mine ? t("chat.reaction.remove_title", { names: re.reactedBy }) : t("chat.reaction.add_title", { names: re.reactedBy })}
                      onClick={() => {
                        if (props.messageId !== undefined) {
                          void props.onReact(props.messageId, re.emoji);
                        }
                      }}
                    >
                      {chipBody()}
                    </button>
                  </Show>
                );
              }}
            </For>
          </div>
        </Show>

        {/* The line itself is one function, called from wherever the slot
            says: here as the bubble's own row, or handed to the voice bubble
            to draw on its quiet line. Two spellings of it would be two things
            to keep in step. */}
        <Show when={metaSlot() !== "voice-line"}>{metaLine()}</Show>

        {/* The action menu, opened by the gestures wired above and rendered
            in a portal OUTSIDE this bubble (`MessageMenu.tsx`). It carries
            both halves the spec names — the quick reaction row and Edit /
            Delete — because one gesture opens both. Gated on `menuAvailable`
            at open AND here, so a menu left open when an eviction commit
            lands mid-poll closes with the affordance rather than floating
            over a dead group. */}
        <Show when={menuAvailable() ? menuAnchor() : null}>
          {(anchor) => (
            <MessageMenu
              anchor={anchor()}
              canEditDelete={props.isOwn}
              /* `message-actions.md` section 3 — offered on "any message with
                 text", which is not the same predicate as Edit / Delete: Copy
                 is offered on somebody else's message too, and withheld from
                 our own lone photo. The text is the one on screen, so an
                 edited message copies what the person is reading. */
              copyText={displayText().length > 0 ? displayText() : null}
              onReact={(emoji) => {
                if (props.messageId !== undefined) void props.onReact(props.messageId, emoji);
              }}
              onMoreReactions={() => {
                if (props.messageId !== undefined) props.onOpenFullReactPicker(props.messageId);
              }}
              onReply={() => {
                if (props.messageId !== undefined) props.onReply(props.messageId);
              }}
              /* Section 5.1 — offered once the send is complete. An incoming
                 row is always complete; an own row is once acked. The id gate
                 is the menu's own (it never opens without one). */
              canForward={props.messageId !== undefined && props.ack === "sent"}
              onForward={() => {
                if (props.messageId !== undefined) props.onForward(props.messageId);
              }}
              /* Section 4.1 — offered on "any message whose send is complete
                 and that is not deleted": the same gate as Forward (the menu
                 never opens on a deleted message). */
              pinLabel={
                props.messageId !== undefined && props.ack === "sent"
                  ? props.pinLabelOf(props.messageId)
                  : null
              }
              onPin={() => {
                if (props.messageId !== undefined) props.onPin(props.messageId);
              }}
              /* Nothing is sent and nothing is stored: the text goes from this
                 device's memory to this device's clipboard. */
              onCopy={(text) => void copyMessageText(text)}
              /* Section 8 — the same gate as Forward, read once. */
              canSelect={selectable()}
              onSelect={() => {
                if (props.messageId !== undefined) props.onSelect(props.messageId);
              }}
              onEdit={() => props.onStartEdit(props.messageId as number, displayText())}
              onDelete={() => {
                if (props.messageId !== undefined) void props.onDeleteMessage(props.messageId);
              }}
              onClose={() => setMenuAnchor(null)}
            />
          )}
        </Show>

        <Show
          when={
            props.actionsAvailable &&
            props.messageId !== undefined &&
            props.fullReactPickerId === props.messageId
          }
        >
          <EmojiPicker
            mode="reaction"
            onEmoji={(emoji) => {
              if (props.messageId !== undefined) void props.onReact(props.messageId, emoji);
            }}
            onClose={() => props.onCloseFullReactPicker()}
            sessionToken={() => null}
            align={isIncoming ? "left" : "right"}
          />
        </Show>
      </div>
    </li>
  );
}

/**
 * Render the attachment-chip strip below a bubble's text body.
 * Each chip shows `name - size` plus action buttons:
 *
 *   * **Open** — inline-preview when the MIME is an allowlisted
 *     image type (`inlineMediaType`); fetches + decrypts the
 *     bytes lazily, caches the object URL.
 *   * **Save** — download + decrypt + trigger a save dialog.
 *
 * `tone` shifts the chip colour so they stay legible inside
 * both the incoming and outgoing bubble bodies.
 */
function AttachmentChips(props: {
  hub: MediaHub;
  attachments: AttachmentRefBytes[];
  onDownload: (ref: AttachmentRefBytes) => void | Promise<void>;
  onPreview: (ref: AttachmentRefBytes) => void | Promise<void>;
  onHidePreview: (ref: AttachmentRefBytes) => void;
  /** Returns the cached object URL for an attachment, or `null`
   *  if it hasn't been decrypted in this session. The bubble
   *  renders an inline `<img>` whenever this is non-null. */
  getCachedUrl: (ref: AttachmentRefBytes) => string | null;
  tone: "incoming" | "outgoing";
}): JSX.Element {
  return (
    <div class="lk-chips">
      <For each={props.attachments}>
        {(ref) => {
          const cachedUrl = (): string | null => props.getCachedUrl(ref);
          // ADR-0047 — inline only for an image type the core's allowlist
          // accepts; never decided by the sender-chosen kind or name.
          const previewable = props.hub.inlineType(ref.mime) === "image";
          return (
            <div class="lk-chip-block">
              <Show when={previewable && cachedUrl() !== null}>
                {/* Inline image preview — renders the decrypted blob URL
                    directly in the bubble, next to its filename. */}
                <img
                  class="lk-chip-image"
                  src={cachedUrl() ?? ""}
                  alt={ref.name ?? t("chat.attachment.image_alt")}
                />
              </Show>
              <span class="lk-chip" classList={{ outgoing: props.tone === "outgoing" }}>
                <span class="lk-chip-name" title={ref.name ?? t("chat.attachment.unnamed")}>
                  {ref.name ?? t("chat.attachment.unnamed")}
                </span>
                <span class="lk-chip-size">{formatAttachmentSize(ref.size)}</span>
                <Show when={previewable}>
                  <Show
                    when={cachedUrl() !== null}
                    fallback={
                      <button
                        type="button"
                        class="lk-chip-action"
                        onClick={() => {
                          void props.onPreview(ref);
                        }}
                      >
                        {t("chat.attachment.open")}
                      </button>
                    }
                  >
                    <button
                      type="button"
                      class="lk-chip-action"
                      onClick={() => props.onHidePreview(ref)}
                    >
                      {t("chat.attachment.hide")}
                    </button>
                  </Show>
                </Show>
                <button
                  type="button"
                  class="lk-chip-action"
                  onClick={() => {
                    void props.onDownload(ref);
                  }}
                >
                  {t("common.save")}
                </button>
              </span>
            </div>
          );
        }}
      </For>
    </div>
  );
}
