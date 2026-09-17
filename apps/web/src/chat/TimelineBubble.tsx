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
 * text, deleted tombstone, reaction chips), the hover actions, the reaction
 * pickers, the media bubbles and the file chips — is the ADR-0023 / ADR-0024
 * / ADR-0047 one; only the chrome is here. The date / run computation the
 * caller feeds in is `timeline-layout.ts`.
 */
import { For, Show, type JSX } from "solid-js";

import type { MemberIdentity } from "./author-identity";
import { formatAttachmentSize, type AttachmentRefBytes } from "./attachments";
import { useChats } from "./ChatContext";
import { OWN_REACTION_SENTINEL, REACTION_EMOJI, type MessageOpState } from "./message-ops";
import { ownPersonKeyB64 } from "./person-key";
import { shortTime } from "./timeline-layout";
import type { TimelineRow } from "./timeline-row";
import { EmojiPicker } from "../media/EmojiPicker";
import { MediaAttachment, type MediaUpload } from "../media/MediaBubbles";
import { mediaKey, type MediaHub } from "../media/media-hub";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";

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
  reactPickerId: number | null;
  onOpenReactPicker: (messageId: number) => void;
  onCloseReactPicker: () => void;
  onReact: (messageId: number, emoji: string) => void | Promise<void>;
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
  onRetryMedia: () => void;
  /** ADR-0047 — the reaction bar's "+" opens the full picker for this id. */
  fullReactPickerId: number | null;
  onOpenFullReactPicker: (messageId: number) => void;
  onCloseFullReactPicker: () => void;
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
          <span>{ev.systemKind === "commit" ? "Members changed" : "Join request"}</span>
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
    if (ev.kind === "group-name") {
      // ADR-0051 — consumed by the group-name gate, never pushed into
      // `rows()`. Defensive.
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
      ev.senderKey !== undefined ? profiles.resolve(ev.senderKey).name : "Unknown sender";
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
        time={shortTime(ev.createdAt)}
        senderName={props.isGroup && props.startsRun && !own ? senderName() : null}
        avatar={
          props.isGroup && !own
            ? props.startsRun
              ? { label: senderName(), id: ev.senderKey ?? "" }
              : "gutter"
            : null
        }
        tone={own ? "outgoing" : "incoming"}
        {...props}
      />
    );
  }
  // Outgoing — our own message. Edit/Delete offered once acked
  // (a pending bubble has no messageId yet).
  const r = props.row;
  return (
    <ChatBubbleBody
      messageId={r.ack === "sent" ? r.messageId : undefined}
      isOwn={true}
      ack={r.ack}
      originalText={r.plaintext}
      attachments={r.attachments}
      time={r.ack === "pending" ? "Sending..." : r.ack === "failed" ? "Not sent" : shortTime(r.createdAt)}
      senderName={null}
      avatar={null}
      tone="outgoing"
      mediaLocalKey={r.mediaLocalKey}
      {...props}
    />
  );
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
  const pickerOpen = (): boolean =>
    props.messageId !== undefined && props.reactPickerId === props.messageId;
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
        reactedBy: [...reactors]
          .map((r) =>
            r === own ? "You" : r === OWN_REACTION_SENTINEL ? "A member" : profiles.resolve(r).name,
          )
          .join(", "),
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
  // UX spec section 3 — a video note is a circle with no bubble chrome.
  const chromeless = (): boolean =>
    props.originalText.length === 0 &&
    props.attachments.length === 1 &&
    props.attachments[0] !== undefined &&
    props.hub.decision(props.attachments[0]).bubble === "video_note";
  const keyFor = (ref: AttachmentRefBytes): string =>
    ref.blobId.length === 0 && props.mediaLocalKey !== undefined
      ? props.mediaLocalKey
      : mediaKey(ref.blobId);
  // A pending media message keeps full opacity: its upload ring is the
  // progress signal, and a dimmed video would read as disabled.
  const pending = (): boolean => props.ack === "pending" && props.mediaLocalKey === undefined;

  return (
    <li
      class="lk-bubble-row"
      classList={{
        incoming: isIncoming,
        outgoing: !isIncoming,
        "run-start": props.startsRun,
        "run-end": props.endsRun,
      }}
    >
      <Show when={props.avatar !== null}>
        <span class="lk-bubble-gutter">
          <Show when={props.avatar !== "gutter" && props.avatar}>
            {(a) => <Avatar label={a().label} id={a().id} size="sm" />}
          </Show>
        </span>
      </Show>
      <div
        class="lk-bubble"
        classList={{
          incoming: isIncoming,
          outgoing: !isIncoming,
          chromeless: chromeless(),
          pending: pending(),
          "run-start": props.startsRun,
          "run-end": props.endsRun,
        }}
      >
        <Show when={props.senderName}>{(n) => <div class="lk-sender">{n()}</div>}</Show>
        <Show
          when={!isDeleted()}
          fallback={<div class="lk-bubble-deleted">Message deleted</div>}
        >
          <Show
            when={isEditing()}
            fallback={
              <>
                <Show when={displayText().length > 0}>
                  <div class="lk-bubble-text">
                    {displayText()}
                    <Show when={isEdited()}>
                      <span class="lk-bubble-edited">(edited)</span>
                    </Show>
                  </div>
                </Show>
                <Show when={mediaRefs().length > 0}>
                  <div
                    class="lk-bubble-media"
                    classList={{ "after-text": displayText().length > 0 }}
                  >
                    <For each={mediaRefs()}>
                      {(ref) => (
                        <MediaAttachment
                          attachment={ref}
                          hub={props.hub}
                          mediaKey={keyFor(ref)}
                          incoming={isIncoming}
                          upload={props.mediaUpload}
                          onRetry={props.onRetryMedia}
                        />
                      )}
                    </For>
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
                  Save
                </button>
                <button type="button" onClick={() => props.onCancelEdit()}>
                  Cancel
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
                        title={`Reacted by ${re.reactedBy}`}
                      >
                        {chipBody()}
                      </span>
                    }
                  >
                    <button
                      type="button"
                      class="lk-reaction"
                      classList={{ mine: re.mine }}
                      title={`${re.mine ? "Remove your reaction" : "React"} - Reacted by ${re.reactedBy}`}
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

        <div class="lk-bubble-meta">
          <span>{props.time}</span>
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
                  : props
                      .readersOf(props.messageId)
                      .map((r) => profiles.resolve(r).name)
                      .join(", ");
              return (
                <span
                  class="lk-bubble-ack"
                  classList={{ read: read() }}
                  title={
                    read() && readers().length > 0 ? `Read by ${readers()}` : read() ? "Read" : "Sent"
                  }
                  aria-label={read() ? "Read" : "Sent"}
                >
                  <Icon name={read() ? "dcheck" : "check"} size={14} />
                </span>
              );
            })()}
          </Show>
        </div>

        {/* Per-message action menu — gated on the message having a
            DS messageId (the op references its target by id) and on
            the membership still holding (ADR-0045 UX section 6.2.5: Edit /
            Delete / React each compose an application message, so
            all three die with Send — the menu is WITHDRAWN, not
            disabled). The hover-revealed strip carries React on any
            message, plus Edit + Delete on our own. Inline-edit mode
            hides it. */}
        <Show
          when={
            props.actionsAvailable && props.messageId !== undefined && !isEditing() && !isDeleted()
          }
        >
          <div
            class="lakko-msg-actions"
            role="group"
            aria-label="Message actions"
            style={{ [isIncoming ? "right" : "left"]: "0.4rem" }}
          >
            <button
              type="button"
              title="React"
              aria-label="React"
              onClick={() =>
                pickerOpen()
                  ? props.onCloseReactPicker()
                  : props.onOpenReactPicker(props.messageId as number)
              }
            >
              <Icon name="smile" size={16} />
            </button>
            <Show when={props.isOwn}>
              <button
                type="button"
                title="Edit"
                aria-label="Edit"
                onClick={() => props.onStartEdit(props.messageId as number, displayText())}
              >
                <Icon name="pencil" size={16} />
              </button>
              <button
                type="button"
                title="Delete"
                aria-label="Delete"
                onClick={() => {
                  if (props.messageId !== undefined) {
                    void props.onDeleteMessage(props.messageId);
                  }
                }}
              >
                <Icon name="trash" size={16} />
              </button>
            </Show>
          </div>
        </Show>

        {/* Emoji picker popover. Gated on the same predicate as the strip
            that opens it, so a picker left open when the eviction commit
            lands mid-poll closes with it rather than floating over a dead
            group. */}
        <Show when={props.actionsAvailable && pickerOpen()}>
          <div class="lk-react-bar" style={{ [isIncoming ? "right" : "left"]: "0.4rem" }}>
            <For each={REACTION_EMOJI}>
              {(emoji) => (
                <button
                  type="button"
                  onClick={() => {
                    if (props.messageId !== undefined) {
                      void props.onReact(props.messageId, emoji);
                    }
                  }}
                >
                  {emoji}
                </button>
              )}
            </For>
            {/* ADR-0047 — "+" opens the full picker in reaction mode. */}
            <button
              type="button"
              class="more"
              aria-label="More reactions"
              title="More reactions"
              onClick={() => {
                if (props.messageId !== undefined) props.onOpenFullReactPicker(props.messageId);
              }}
            >
              <Icon name="plus" size={16} />
            </button>
          </div>
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
                  alt={ref.name ?? "image attachment"}
                />
              </Show>
              <span class="lk-chip" classList={{ outgoing: props.tone === "outgoing" }}>
                <span class="lk-chip-name" title={ref.name ?? "(unnamed)"}>
                  {ref.name ?? "(unnamed)"}
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
                        Open
                      </button>
                    }
                  >
                    <button
                      type="button"
                      class="lk-chip-action"
                      onClick={() => props.onHidePreview(ref)}
                    >
                      Hide
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
                  Save
                </button>
              </span>
            </div>
          );
        }}
      </For>
    </div>
  );
}
