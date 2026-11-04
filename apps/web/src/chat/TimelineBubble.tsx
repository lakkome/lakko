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
import { createEffect, createSignal, For, Index, onCleanup, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { A } from "@solidjs/router";

import { callInviteOf } from "../calls/call-invite-link";

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
import { heldBy, type MessageOpState } from "./message-ops";
import {
  chipRow,
  maxPerPerson,
  reactorsByEmoji,
  rowExpands,
  shownKinds,
  type AllowedSetting,
  type ChipConversation,
  type ChipRowView,
} from "./reaction-rules";
import { emojiOnlyCount, maxReactionKindsShown } from "../../wasm/lakko_ffi_wasm";

/** The most reaction kinds a message shows (11, UX D6). */
const MAX_KINDS = ((): number => {
  try {
    return maxReactionKindsShown();
  } catch {
    return 11;
  }
})();
import {
  LONG_PRESS_MS,
  LONG_PRESS_SLOP_PX,
  MessageMenu,
  type MenuAnchor,
} from "./MessageMenu";
import { forwardLineLabel } from "./forward";
import { MessageText } from "./MessageText";
import { membershipSentences } from "./membership-rows";
import { realNameOf } from "../profile/member-label";
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
import { ownAvatarLabel } from "../profile/member-label";
import { OWN_AVATAR_KEY } from "../profile/avatar-runtime";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { joinList, t } from "../l10n";
import { displayLocale } from "../l10n/display-locale";
import { publicOrigin } from "../platform";
import type { MessageRef, TextSlot } from "../translation/controller";
import { useTranslation } from "../translation/context";
import { TranslationLine } from "../translation/TranslationLine";
import type { TranscriptHost } from "../translation/Transcript";

/** The callbacks and reactive reads a bubble needs from the chat view. */
export interface BubbleHost {
  /** ADR-0082 — the conversation a translation or transcript is filed
   *  under (the group id, standard base64). Absent: no translation. */
  conversationId?: string | undefined;
  /** ADR-0065 section 2 — whether this account saved the message (the mark
   *  beside the time and the menu's `Remove from saved`). Reactive. */
  isSavedOf?: (messageId: number) => boolean;
  /** ADR-0065 section 2 — `Save` / `Remove from saved` was chosen. */
  onToggleSaved?: (messageId: number) => void;
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
  /** The edit was opened from the keyboard (desktop): its field takes the
   *  focus. Unset, the edit opens as it always did. */
  focusEditOnOpen?: boolean;
  setEditText: (v: string) => void;
  onStartEdit: (messageId: number, current: string) => void;
  onCancelEdit: () => void;
  onSaveEdit: (messageId: number) => void | Promise<void>;
  onDeleteMessage: (messageId: number) => void | Promise<void>;
  onReact: (messageId: number, emoji: string) => void | Promise<void>;
  /** `docs/ux/reactions-and-emoji.md` section 6 — a double click on the
   *  bubble outside its text and controls: the person's quick reaction. */
  onQuickReact: (messageId: number) => void;
  /** Section 9 — what this conversation takes. Reactive. */
  reactionAllowed: () => AllowedSetting;
  /** Section 7 — how the chips draw (one-to-one or group). */
  chipConversation: ChipConversation;
  /** Section 5 — the six emoji of this message's reaction row. Reactive. */
  reactionRowFor: (messageId: number) => readonly string[];
  /** The person's account key, base64: who "mine" means (ADR-0050). */
  ownReactorKey: string | null;
  /** Section 10 — the message whose chips show the three-reactions line for
   *  about three seconds, or `null`. */
  capNoticeId: number | null;
  /** Section 8.1 — a row of a contact in the reactions list was pressed. */
  onOpenPerson?: (personKeyB64: string) => void;
  /** Section 8.1 — whether a reactor is a contact (only a contact's row is a
   *  control). */
  isContactKey?: (personKeyB64: string) => boolean;
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
  /** ADR-0073 R.5 — set on a row of an OLDER segment that is pinned in a
   *  way this device may clear: its menu opens with the `Unpin` row alone
   *  (`pin-bar.ts` `historyRowOffersUnpin`), whatever `actionsAvailable`
   *  says. */
  unpinOnly?: boolean;
  /** Whether the pin row is offered (default yes). An older segment's row
   *  opens the restricted menu for Save (2026-10-27) and offers Unpin only
   *  when `historyRowOffersUnpin` says so. */
  pinRowShown?: boolean;
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
      if (ev.systemKind === "commit") {
        // docs/ux/second-device.md section 4 — a commit row is drawn only when
        // it names the people a batch added or removed (the timeline already
        // left out every other one, and every one in a one-to-one); one
        // sentence per person, joined first, each name resolved now from the
        // stored person key.
        const change = ev.membership;
        if (change === undefined) return null;
        const sentences = (): string[] =>
          membershipSentences(
            change,
            (key, name) => t(key, { name }),
            (k) => realNameOf(profiles.resolve(k)),
          );
        return (
          <Show when={sentences().length > 0}>
            <li class="lk-system-row lk-system-row--membership">
              <For each={sentences()}>{(line) => <span>{line}</span>}</For>
            </li>
          </Show>
        );
      }
      return (
        <li class="lk-system-row">
          <span>{t("chat.system.join_request")}</span>
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
    if (ev.kind === "group-name" || ev.kind === "group-setting") {
      // ADR-0051 / ADR-0087 — consumed by the group-name gate, never pushed
      // into `rows()`. Defensive.
      return null;
    }
    if (ev.kind === "group-closed") {
      // ADR-0073 R.6 — a successor claim, consumed by the claim rule; never a row.
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
        time={shortTime(ev.createdAt, displayLocale())}
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
            : shortTime(live().createdAt, displayLocale())
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
  if (!(target instanceof Element)) return false;
  // A long press on a link in the text is the message's menu: the link opens
  // on a tap (`docs/ux/message-links.md` section 2).
  if (target.closest(".lk-text-link") !== null) return false;
  return target.closest(BUBBLE_CONTROLS) !== null;
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
  /** `docs/ux/call-links.md` section 3: a message that is one call link of
   *  this deployment is an invitation, drawn with a `Join` button instead of
   *  a line of characters. */
  const callInvite = (): ReturnType<typeof callInviteOf> => {
    let origin: string;
    try {
      origin = publicOrigin();
    } catch {
      return null;
    }
    return callInviteOf(displayText(), origin);
  };
  const isEditing = (): boolean =>
    props.messageId !== undefined && props.editingId === props.messageId;

  // ----- ADR-0082: translation and transcription, on this device ----------
  //
  // Everything below reads text this device already decrypted and hands it
  // to an on-device engine through the controller; nothing leaves the
  // device. The ORIGINAL stays the message: Reply, Forward, Save and Select
  // read `displayText()` and the message id as before, and only Copy and the
  // bubble's own text show the translation (UX 3.2).
  const translation = useTranslation();
  const messageRef = (): MessageRef | null =>
    props.messageId === undefined || props.conversationId === undefined
      ? null
      : {
          conversation: props.conversationId,
          messageId: props.messageId,
          // ADR-0082 section 5 — the DS id of the latest edit op, 0 when
          // the message was never edited.
          lastOpId: state()?.lastOpId ?? 0,
        };
  const textView = () => {
    const ref = messageRef();
    return translation === undefined || ref === null || isEditing()
      ? ({ phase: "none" } as const)
      : translation.textView("text", ref);
  };
  /** The text the bubble shows: the translation while it is shown. */
  const shownText = (): string => {
    const v = textView();
    return v.phase === "translated" ? v.text : displayText();
  };
  /** The transcript open under a voice / video message without a caption,
   *  which the menu then treats as the message's text (UX 4.2). */
  const openTranscript = (): string | null => {
    const ref = messageRef();
    if (translation === undefined || ref === null || displayText().length > 0) return null;
    const v = translation.transcriptView(ref);
    return v.phase === "shown" ? v.text : null;
  };
  /** What Translate acts on, and in which slot. */
  const translateTarget = (): { slot: TextSlot; text: string } | null => {
    if (displayText().length > 0) return { slot: "text", text: displayText() };
    const tr = openTranscript();
    return tr === null ? null : { slot: "transcript-translation", text: tr };
  };
  const translateLabel = (): "translate" | "show-original" | null => {
    const ref = messageRef();
    const target = translateTarget();
    if (translation === undefined || ref === null || target === null) return null;
    if (translation.textView(target.slot, ref).phase === "translated") return "show-original";
    return translation.offer(ref, target.text, props.tone === "outgoing") === "none" ? null : "translate";
  };
  /** Copy is "the text I see" (UX 3.2): the translation while it is shown. */
  const copyableText = (): string | null => {
    const ref = messageRef();
    if (displayText().length > 0) return shownText();
    const tr = openTranscript();
    if (tr === null || translation === undefined || ref === null) return null;
    const v = translation.textView("transcript-translation", ref);
    return v.phase === "translated" ? v.text : tr;
  };
  // The language of a shown message is known before its menu opens, so the
  // row and its source are decided without a wait (UX 3.1). Detection runs on
  // the device and only where its model is already present.
  createEffect(() => {
    const ref = messageRef();
    if (translation === undefined || ref === null || isDeleted() || props.tone === "outgoing") return;
    const text = displayText();
    if (text.length > 0) translation.offer(ref, text, false);
  });
  // UX 3.7 — automatic translation as the message is shown.
  createEffect(() => {
    const ref = messageRef();
    if (translation === undefined || ref === null || isDeleted() || isEditing()) return;
    const text = displayText();
    if (text.length === 0) return;
    translation.autoTranslate(ref, text, props.tone === "outgoing");
  });
  /** The transcript host of one media attachment, or `undefined`. */
  const transcriptHost = (ref: AttachmentRefBytes): TranscriptHost | undefined => {
    const m = messageRef();
    if (translation === undefined || m === null || props.ack !== "sent") return undefined;
    const kind = props.hub.decision(ref).bubble;
    if (kind !== "voice" && kind !== "video_note") return undefined;
    return {
      controller: translation,
      messageRef: m,
      loadAudio: async () => {
        const key = keyFor(ref);
        // The same fetch and AEAD open playing uses; never partial bytes.
        if (props.hub.blobFor(key) === null && ref.blobId.length > 0) await props.hub.load(ref);
        return props.hub.blobFor(key);
      },
    };
  };

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
  /** The menu is the pin row alone (an older segment's pinned row). */
  const pinOnlyMenu = (): boolean => !props.actionsAvailable && props.unpinOnly === true;
  const menuAvailable = (): boolean =>
    (props.actionsAvailable || props.unpinOnly === true) &&
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

  // Section 7 — the chips, from the core's chip rule. A message deleted for
  // everyone shows none.
  const chips = (): ChipRowView => {
    const st = state();
    if (st === undefined || st.held.length === 0 || isDeleted()) return { chips: [], overflow: 0 };
    return chipRow(st.held, props.ownReactorKey, props.chipConversation, props.reactionAllowed());
  };
  const ownGiven = (): string[] => heldBy(state(), props.ownReactorKey);
  const rowExpandsFor = (): boolean => rowExpands(props.reactionAllowed());
  /** The reactions list (section 8.1): open at a chip, on the tab of its
   *  emoji (`null` = All). */
  const [reactorsList, setReactorsList] = createSignal<{ x: number; y: number; tab: string | null } | null>(null);
  const reactorTabs = (): Array<{ emoji: string; people: string[] }> => {
    const st = state();
    if (st === undefined) return [];
    const by = reactorsByEmoji(st.held);
    const order = chips().chips.map((c) => c.emoji);
    for (const e of by.keys()) if (!order.includes(e)) order.push(e);
    return order
      .filter((e) => by.has(e))
      .map((emoji) => {
        const people = [...(by.get(emoji) ?? [])];
        // The person's own row reads "You" and comes first.
        people.sort((a, b) => (a === props.ownReactorKey ? -1 : b === props.ownReactorKey ? 1 : 0));
        return { emoji, people };
      });
  };
  const personName = (k: string): string =>
    k === props.ownReactorKey ? t("chat.author.you") : profiles.resolve(k).name;
  // The avatar of a reactor: this account's own photo and initials for its
  // own reaction (never the letters of "You"), anyone else's as resolved.
  const ownUsername = (): string => profiles.ownUsername() ?? "";
  const avatarOf = (k: string): { label: string; personKey: string } =>
    k === props.ownReactorKey
      ? { label: ownAvatarLabel(profiles.ownProfile().displayName, ownUsername()), personKey: OWN_AVATAR_KEY }
      : { label: profiles.resolve(k).name, personKey: k };

  /** Section 13 — a message of one to three emoji and nothing else, no
   *  attachment, no quote, no forward line, is drawn large with no bubble. */
  const emojiOnly = (): number => {
    if (props.attachments.length > 0 || props.reply !== undefined || props.forward !== undefined) return 0;
    if (isDeleted() || props.unsupported === true) return 0;
    try {
      return emojiOnlyCount(displayText());
    } catch {
      return 0;
    }
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
      {/* ADR-0065 section 2 — the saved mark, seen only by this account. */}
      <Show when={props.messageId !== undefined && props.isSavedOf?.(props.messageId) === true}>
        <span class="lk-bubble-saved" role="img" aria-label={t("chat.message.saved_mark")} title={t("chat.message.saved_mark")}>
          <Icon name="bookmark" size={12} />
        </span>
      </Show>
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
            {(a) => <Avatar label={a().label} id={a().id} size="sm" personKey={a().id} />}
          </Show>
        </span>
      </Show>
      <div
        ref={bubbleEl}
        class="lk-bubble"
        classList={{
          incoming: isIncoming,
          outgoing: !isIncoming,
          chromeless: chromeless() || emojiOnly() > 0,
          "emoji-only": emojiOnly() > 0,
          [`emoji-only-${emojiOnly()}`]: emojiOnly() > 0,
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
        onDblClick={(e) => {
          // Section 6 (D2) — a double click reacts with the quick reaction,
          // except on the text (a double click there selects a word) and on a
          // control (play, link, attachment, quote block).
          if (!menuAvailable() || props.messageId === undefined || props.ack !== "sent") return;
          if (startsOnControl(e.target)) return;
          if (e.target instanceof Element && e.target.closest(".lk-bubble-text, .lk-reactions") !== null) return;
          e.preventDefault();
          props.onQuickReact(props.messageId);
        }}
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
                <Show when={callInvite()}>
                  {(invite) => (
                    <div class="lk-bubble-call-invite">
                      <span class="lk-bubble-call-invite-icon" aria-hidden="true">
                        <Icon name="phone" size={20} />
                      </span>
                      <span class="lk-bubble-call-invite-title">
                        {t("chat.call_invite.title")}
                      </span>
                      <A class="lk-button-link primary" href={invite().href}>
                        {t("chat.call.join_short")}
                      </A>
                    </div>
                  )}
                </Show>
                <Show when={displayText().length > 0 && callInvite() === null}>
                  <div class="lk-bubble-text" dir="auto">
                    <MessageText text={shownText()} />
                    <Show when={isEdited()}>
                      <span class="lk-bubble-edited">{t("chat.message.edited")}</span>
                    </Show>
                  </div>
                  {/* UX 3.2 — machine output is always marked, and the
                      original is one tap away on this line. */}
                  <Show when={translation !== undefined && messageRef()}>
                    {(ref) => (
                      <TranslationLine
                        controller={translation!}
                        slot="text"
                        messageRef={ref()}
                        text={displayText()}
                        view={textView()}
                      />
                    )}
                  </Show>
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
                          transcript={transcriptHost(ref())}
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
                ref={(el) => {
                  if (props.focusEditOnOpen === true) {
                    queueMicrotask(() => {
                      el.focus();
                      el.setSelectionRange(el.value.length, el.value.length);
                    });
                  }
                }}
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

        {/* Section 7 — the reaction chips, from the core's chip rule: most
            people first, at most eleven, `+N` for the rest; a group shows the
            avatars of up to three reactors. None on a deleted message. */}
        <Show when={chips().chips.length > 0}>
          <div class="lk-reactions">
            <For each={chips().chips}>
              {(re) => {
                // The chip carries BOTH a fact and an affordance, and
                // ADR-0045 UX section 6.2.5 separates them: the count is
                // transcript, while tapping it is the React op, which dies
                // with Send. Once the membership has ended the chip renders
                // as static text rather than as a button.
                const chipBody = (): JSX.Element => (
                  <>
                    <span class="lk-reaction-emoji">{re.emoji}</span>
                    <Show when={re.face === "avatars"}>
                      <span class="lk-reaction-avatars" aria-hidden="true">
                        <For each={re.reactors}>
                          {(k) => (
                            <Avatar
                              label={avatarOf(k).label}
                              id={k}
                              size="sm"
                              personKey={avatarOf(k).personKey}
                              class="lk-reaction-avatar"
                            />
                          )}
                        </For>
                      </span>
                    </Show>
                    <Show when={re.face === "count"}>
                      <span class="lk-reaction-count">{re.count}</span>
                    </Show>
                  </>
                );
                const label = (): string =>
                  re.mine
                    ? t("chat.reaction.chip_yours_a11y", { emoji: re.emoji })
                    : t("chat.reaction.chip_a11y", { emoji: re.emoji, count: re.count });
                const openList = (e: MouseEvent): void => {
                  e.preventDefault();
                  e.stopPropagation();
                  setReactorsList({ x: e.clientX, y: e.clientY, tab: re.emoji });
                };
                return (
                  <Show
                    when={props.actionsAvailable}
                    fallback={
                      <span class="lk-reaction" classList={{ mine: re.mine }} aria-label={label()} onContextMenu={openList}>
                        {chipBody()}
                      </span>
                    }
                  >
                    <button
                      type="button"
                      class="lk-reaction"
                      classList={{ mine: re.mine }}
                      aria-label={label()}
                      aria-description={t("chat.reaction.show_who_a11y")}
                      onContextMenu={openList}
                      onClick={(e) => {
                        e.stopPropagation();
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
            <Show when={chips().overflow > 0}>
              <button
                type="button"
                class="lk-reaction more"
                aria-label={t("chat.reaction.show_who_a11y")}
                onClick={(e) => {
                  e.stopPropagation();
                  setReactorsList({ x: e.clientX, y: e.clientY, tab: null });
                }}
              >
                {`+${chips().overflow}`}
              </button>
            </Show>
          </div>
        </Show>
        <Show when={props.messageId !== undefined && props.capNoticeId === props.messageId}>
          <p class="lk-reaction-cap" role="status">{t("chat.reaction.cap_reached")}</p>
        </Show>
        <Show when={reactorsList()}>
          {(at) => (
            <ReactorsList
              x={at().x}
              y={at().y}
              initialTab={at().tab}
              tabs={reactorTabs()}
              nameOf={personName}
              avatarOf={avatarOf}
              ownKey={props.ownReactorKey}
              onOpenPerson={props.onOpenPerson}
              isContact={(k) => props.isContactKey?.(k) === true}
              onClose={() => setReactorsList(null)}
            />
          )}
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
              pinOnly={pinOnlyMenu()}
              canEditDelete={props.isOwn}
              /* `message-actions.md` section 3 — offered on "any message with
                 text", which is not the same predicate as Edit / Delete: Copy
                 is offered on somebody else's message too, and withheld from
                 our own lone photo. The text is the one on screen, so an
                 edited message copies what the person is reading. */
              copyText={copyableText()}
              translateLabel={translateLabel()}
              onTranslate={() => {
                const ref = messageRef();
                const target = translateTarget();
                if (translation === undefined || ref === null || target === null) return;
                translation.toggle(target.slot, ref, target.text);
              }}
              onReact={(emoji) => {
                if (props.messageId !== undefined) void props.onReact(props.messageId, emoji);
              }}
              onMoreReactions={() => {
                if (props.messageId !== undefined) props.onOpenFullReactPicker(props.messageId);
              }}
              reactionRow={
                props.messageId !== undefined && props.ack === "sent" ? props.reactionRowFor(props.messageId) : []
              }
              ownReactions={ownGiven()}
              showExpand={rowExpandsFor()}
              capReached={ownGiven().length >= maxPerPerson()}
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
                props.messageId !== undefined && props.ack === "sent" && props.pinRowShown !== false
                  ? props.pinLabelOf(props.messageId)
                  : null
              }
              onPin={() => {
                if (props.messageId !== undefined) props.onPin(props.messageId);
              }}
              /* ADR-0065 section 2 — under Pin, on a message whose send is
                 complete and that is not deleted (the menu never opens on a
                 deleted one). */
              saveLabel={
                props.messageId !== undefined && props.ack === "sent" && props.onToggleSaved !== undefined
                  ? props.isSavedOf?.(props.messageId) === true
                    ? "unsave"
                    : "save"
                  : null
              }
              onSave={() => {
                if (props.messageId !== undefined) props.onToggleSaved?.(props.messageId);
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
            allowedOnly={(() => {
              const a = props.reactionAllowed();
              return a.mode === "only" ? a.emoji : undefined;
            })()}
            isDisabled={(emoji) => {
              const own = ownGiven();
              if (own.includes(emoji)) return false;
              if (own.length >= maxPerPerson()) return true;
              const kinds = shownKinds(state(), props.reactionAllowed());
              return kinds.length >= MAX_KINDS && !kinds.includes(emoji);
            }}
            notice={ownGiven().length >= maxPerPerson() ? t("chat.reaction.cap_reached") : undefined}
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

/**
 * `docs/ux/reactions-and-emoji.md` section 8.1 — who reacted: a popover at
 * the chip (web), tabs `All {count}` then one per emoji in chip order, rows of
 * avatar and name, the emoji at the trailing end on `All`. The person's own
 * row reads `You` and comes first. Built from what the device holds; nothing
 * is fetched or sent.
 */
function ReactorsList(props: {
  x: number;
  y: number;
  initialTab: string | null;
  tabs: Array<{ emoji: string; people: string[] }>;
  nameOf: (personKeyB64: string) => string;
  avatarOf: (personKeyB64: string) => { label: string; personKey: string };
  ownKey: string | null;
  onOpenPerson: ((personKeyB64: string) => void) | undefined;
  isContact: (personKeyB64: string) => boolean;
  onClose: () => void;
}): JSX.Element {
  const [tab, setTab] = createSignal<string | null>(props.initialTab);
  const total = (): number => props.tabs.reduce((n, t) => n + t.people.length, 0);
  const rows = (): Array<{ person: string; emoji: string | null }> => {
    const current = tab();
    if (current === null) {
      const out: Array<{ person: string; emoji: string | null }> = [];
      for (const tb of props.tabs) for (const person of tb.people) out.push({ person, emoji: tb.emoji });
      out.sort((a, b) => (a.person === props.ownKey ? -1 : b.person === props.ownKey ? 1 : 0));
      return out;
    }
    return (props.tabs.find((tb) => tb.emoji === current)?.people ?? []).map((person) => ({ person, emoji: null }));
  };
  const onKey = (e: KeyboardEvent): void => {
    if (e.key === "Escape") {
      e.preventDefault();
      props.onClose();
    }
  };
  globalThis.document?.addEventListener("keydown", onKey);
  onCleanup(() => globalThis.document?.removeEventListener("keydown", onKey));
  const left = (): number => Math.max(8, Math.min(props.x, (globalThis.innerWidth ?? 360) - 288));
  const top = (): number => Math.max(8, Math.min(props.y, (globalThis.innerHeight ?? 640) - 320));
  return (
    <Portal>
      <div class="lk-msg-menu-backdrop" onPointerDown={() => props.onClose()} />
      <div
        class="lk-reactors-list lk-msg-menu-surface"
        role="dialog"
        aria-label={t("chat.reaction.list_title")}
        style={{ left: `${left()}px`, top: `${top()}px` }}
      >
        <Show when={props.tabs.length > 1}>
          <div class="lk-reactors-tabs" role="tablist">
            <button type="button" role="tab" aria-selected={tab() === null} onClick={() => setTab(null)}>
              {t("chat.reaction.list_all", { count: total() })}
            </button>
            <For each={props.tabs}>
              {(tb) => (
                <button type="button" role="tab" aria-selected={tab() === tb.emoji} onClick={() => setTab(tb.emoji)}>
                  {`${tb.emoji} ${tb.people.length}`}
                </button>
              )}
            </For>
          </div>
        </Show>
        <ul class="lk-reactors-rows">
          <For each={rows()}>
            {(r) => {
              const body = (): JSX.Element => (
                <>
                  <Avatar
                    label={props.avatarOf(r.person).label}
                    id={r.person}
                    size="sm"
                    personKey={props.avatarOf(r.person).personKey}
                  />
                  <span class="lk-reactors-name">{props.nameOf(r.person)}</span>
                  <Show when={r.emoji}>{(e) => <span class="lk-reactors-emoji">{e()}</span>}</Show>
                </>
              );
              return (
                <li>
                  <Show
                    when={props.onOpenPerson !== undefined && r.person !== props.ownKey && props.isContact(r.person)}
                    fallback={<div class="lk-reactors-row">{body()}</div>}
                  >
                    <button
                      type="button"
                      class="lk-reactors-row"
                      onClick={() => {
                        props.onOpenPerson?.(r.person);
                        props.onClose();
                      }}
                    >
                      {body()}
                    </button>
                  </Show>
                </li>
              );
            }}
          </For>
        </ul>
      </div>
    </Portal>
  );
}
