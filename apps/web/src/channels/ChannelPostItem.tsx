// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One post in a channel's timeline (ADR-0087 sections 12, 13 and 13a; UX
 * `docs/ux/group-and-channel-privacy.md` sections 12 and 13): an incoming
 * bubble with no sender line (the channel is the sender, on every device, the
 * owner's included) at the width of the pane, because the reactions and the
 * comments hang under it. It carries what a message carries: text, photos,
 * files, a voice message, a video note, a GIF, drawn by the chat's own media
 * bubbles.
 *
 * The menu is the chat's `MessageMenu` with the channel's rows (UX 12):
 * React, Forward, Save, Copy, and Edit and Delete for the owner and
 * publishers. Reply is absent (a subscriber comments instead).
 */
import { ChannelContent, channelPollClosable } from "../attach/ChannelContent";
import { formatCoordinates, osmUrl } from "../attach/location-format";
import { displayLocale } from "../l10n/display-locale";
import { isDesktop } from "../platform";
import { openHostUrl } from "../platform/host";
import type { Article, MessageContent } from "../../wasm/lakko_ffi_wasm";
import { createSignal, For, Index, Show, type JSX } from "solid-js";

import {
  downloadAndDecryptRef,
  formatAttachmentSize,
  saveDecryptedBytes,
  type AttachmentRefBytes,
} from "../chat/attachments";
import { copyMessageText } from "../chat/copy-message";
import { canForwardContent } from "../chat/forward";
import { useProfiles } from "../profile/ProfileContext";
import { LONG_PRESS_MS, LONG_PRESS_SLOP_PX, MessageMenu, type MenuAnchor } from "../chat/MessageMenu";
import { MessageText } from "../chat/MessageText";
import { maxPerPerson, reactionRow, rowExpands } from "../chat/reaction-rules";
import { EmojiPicker } from "../media/EmojiPicker";
import { MediaAttachment } from "../media/MediaBubbles";
import { mediaKey, type MediaHub } from "../media/media-hub";
import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { channelIdFromHex } from "./composer";
import type { TimelinePost } from "./channel-timeline";
import { ownReactionsOn, ownFeedbackVersion } from "./own-feedback-register";
import { PostComments } from "./PostComments";
import { channelAllowedReactions, PostReactions, toggleChannelPostReaction } from "./PostReactions";
import { postTimeLabel } from "./relative-time";

export interface ChannelPostItemProps {
  readonly channelIdHex: string;
  readonly post: TimelinePost;
  /** The post's epoch (its reactions and comments are keyed under it). */
  readonly epoch: number | undefined;
  readonly hub: MediaHub;
  /** The owner or a publisher: Edit, Delete, moderation of comments. */
  readonly canPublish: boolean;
  /** This device reads the channel (reactions and comments need a seed). */
  readonly canReact: boolean;
  readonly now: Date;
  readonly sessionToken: () => string | null;
  readonly onEdit: (cursor: number, text: string) => void;
  readonly onDelete: (cursor: number) => void;
  readonly onForward: () => void;
  /** ADR-0065 — whether this account saved the post. */
  readonly saved: boolean;
  readonly onToggleSaved: () => void;
  /** ADR-0092 — `Close poll` on a publisher's open poll post. */
  readonly onClosePoll?: (cursor: number) => void;
  /** ADR-0092 — publish one post (a checklist op from a publisher). */
  readonly publish?: (plaintext: Uint8Array) => Promise<void>;
  /** ADR-0092 — `Edit checklist` / `Edit` on an article post. */
  readonly onEditContent?: (cursor: number, content: MessageContent, attachments: readonly AttachmentRefBytes[]) => void;
  /** ADR-0092 — `Close checklist`. */
  readonly onCloseChecklist?: (cursor: number) => void;
  /** ADR-0092 — an article post's reader. */
  readonly onOpenArticle?: (article: Article, attachments: readonly AttachmentRefBytes[]) => void;
}

export function ChannelPostItem(props: ChannelPostItemProps): JSX.Element {
  const profiles = useProfiles();
  const [menu, setMenu] = createSignal<MenuAnchor | null>(null);
  const [picker, setPicker] = createSignal(false);
  let bubble: HTMLDivElement | undefined;
  let pressTimer: ReturnType<typeof setTimeout> | null = null;
  let pressAt: { x: number; y: number } | null = null;

  // ADR-0092 — a content post's attachments are its own (an article's images).
  const plainRefs = (): readonly AttachmentRefBytes[] => (props.post.content !== undefined ? [] : props.post.attachments);
  const mediaRefs = (): AttachmentRefBytes[] =>
    plainRefs().filter((a) => props.hub.decision(a).bubble !== "file");
  const fileRefs = (): AttachmentRefBytes[] =>
    plainRefs().filter((a) => props.hub.decision(a).bubble === "file");

  const own = (): string[] => {
    void ownFeedbackVersion();
    try {
      return ownReactionsOn(channelIdFromHex(props.channelIdHex), props.post.cursor);
    } catch {
      return [];
    }
  };
  const allowed = (): ReturnType<typeof channelAllowedReactions> => channelAllowedReactions(props.channelIdHex);

  const openAt = (x: number, y: number): void => {
    if (props.post.state !== "ok" || bubble === undefined) return;
    const r = bubble.getBoundingClientRect();
    setMenu({ bubble: { left: r.left, top: r.top, right: r.right, bottom: r.bottom }, x, y });
  };

  /** ADR-0092 — the content's own menu rows in a channel. */
  const contentActions = (): { label: string; icon: "lock" | "globe" | "copy" | "pencil"; onClick: () => void }[] => {
    const c = props.post.content;
    if (c === undefined) return [];
    if (c.kind === "location") {
      const loc = c.location;
      return [
        {
          label: t("location.open_osm"),
          icon: "globe",
          onClick: () => {
            const url = osmUrl(loc.latE7, loc.lonE7);
            if (isDesktop()) void openHostUrl(url);
            else window.open(url, "_blank", "noopener,noreferrer");
          },
        },
        {
          label: t("location.copy"),
          icon: "copy",
          onClick: () => void copyMessageText(formatCoordinates(loc.latE7, loc.lonE7, displayLocale())),
        },
      ];
    }
    // UX 8.2 / 11.3 — a publisher edits a checklist or an article post.
    if (props.canPublish && props.onEditContent !== undefined && (c.kind === "checklist" || c.kind === "article")) {
      const edit = props.onEditContent;
      const rows: { label: string; icon: "lock" | "pencil"; onClick: () => void }[] = [];
      if (c.kind === "checklist" && props.post.closed !== true) {
        rows.push({ label: t("checklist.edit"), icon: "pencil", onClick: () => edit(props.post.cursor, c, props.post.attachments) });
        if (props.onCloseChecklist !== undefined) {
          const close = props.onCloseChecklist;
          rows.push({ label: t("checklist.close"), icon: "lock", onClick: () => close(props.post.cursor) });
        }
      }
      if (c.kind === "article") {
        rows.push({ label: t("chat.message.edit"), icon: "pencil", onClick: () => edit(props.post.cursor, c, props.post.attachments) });
      }
      return rows;
    }
    if (channelPollClosable(c, props.post.closed === true, props.canPublish) && props.onClosePoll !== undefined) {
      const close = props.onClosePoll;
      return [{ label: t("poll.close"), icon: "lock", onClick: () => close(props.post.cursor) }];
    }
    return [];
  };

  const react = (emoji: string): void => {
    const epoch = props.epoch;
    if (epoch === undefined) return;
    void toggleChannelPostReaction(props.channelIdHex, props.post.cursor, epoch, emoji, props.post.serverTsMs).catch(
      () => undefined,
    );
  };

  async function download(ref: AttachmentRefBytes): Promise<void> {
    try {
      const bytes = await downloadAndDecryptRef(ref);
      await saveDecryptedBytes(bytes, ref.name ?? t("chat.attachment.unnamed"), ref.mime);
    } catch {
      // The file chip is the only place a download starts; a failed one is
      // tried again by pressing it again.
    }
  }

  return (
    <li class="lk-post lk-channel-post" classList={{ unavailable: props.post.state === "unavailable" }}>
      <div
        ref={bubble}
        class="lk-channel-post-bubble"
        onContextMenu={(e) => {
          e.preventDefault();
          openAt(e.clientX, e.clientY);
        }}
        onPointerDown={(e) => {
          if (e.pointerType !== "touch") return;
          pressAt = { x: e.clientX, y: e.clientY };
          pressTimer = setTimeout(() => {
            if (pressAt !== null) openAt(pressAt.x, pressAt.y);
          }, LONG_PRESS_MS);
        }}
        onPointerMove={(e) => {
          if (pressAt === null) return;
          if (Math.hypot(e.clientX - pressAt.x, e.clientY - pressAt.y) > LONG_PRESS_SLOP_PX) {
            if (pressTimer !== null) clearTimeout(pressTimer);
            pressAt = null;
          }
        }}
        onPointerUp={() => {
          if (pressTimer !== null) clearTimeout(pressTimer);
          pressAt = null;
        }}
        onKeyDown={(e) => {
          if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
            e.preventDefault();
            const r = e.currentTarget.getBoundingClientRect();
            openAt(r.left, r.top);
          }
        }}
        tabIndex={props.post.state === "ok" ? 0 : -1}
      >
        <Show when={props.post.state === "loading"}>
          <div class="lk-post-placeholder">{t("common.loading")}</div>
        </Show>
        <Show when={props.post.state === "unavailable"}>
          <div class="lk-post-unavailable">{t("channels.post.unavailable")}</div>
        </Show>
        <Show when={props.post.state === "ok"}>
          <Show when={mediaRefs().length > 0}>
            <div class="lk-channel-post-media">
              <Index each={mediaRefs()}>
                {(ref) => (
                  <MediaAttachment
                    attachment={ref()}
                    hub={props.hub}
                    mediaKey={mediaKey(ref().blobId)}
                    incoming
                    upload={undefined}
                    onRetry={() => undefined}
                  />
                )}
              </Index>
            </div>
          </Show>
          <Show when={props.post.content}>
            {(content) => (
              <ChannelContent
                channelIdHex={props.channelIdHex}
                cursor={props.post.cursor}
                epoch={props.epoch}
                content={content()}
                attachments={props.post.attachments}
                closed={props.post.closed === true}
                closedTally={props.post.closedTally}
                canReact={props.canReact}
                canPublish={props.canPublish}
                publish={props.publish}
                checklistOps={props.post.checklistOps}
                hub={props.hub}
                onOpenMenu={() => {
                  const r = bubble?.getBoundingClientRect();
                  openAt((r?.left ?? 0) + 12, (r?.top ?? 0) + 12);
                }}
                onOpenArticle={(a, refs) => props.onOpenArticle?.(a, refs)}
              />
            )}
          </Show>
          <Show when={props.post.text.length > 0}>
            <div class="lk-post-text">
              <MessageText text={props.post.text} />
            </div>
          </Show>
          <Show when={fileRefs().length > 0}>
            <ul class="lk-channel-files">
              <For each={fileRefs()}>
                {(ref) => (
                  <li class="lk-channel-file">
                    <button type="button" class="ghost small" onClick={() => void download(ref)}>
                      <Icon name="doc" size={14} />
                      <span>{ref.name ?? t("chat.attachment.unnamed")}</span>
                      <span class="lk-meta">{formatAttachmentSize(ref.size)}</span>
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>
        <div class="lk-post-time">
          <Show when={props.post.edited}>
            <span class="lk-meta">{t("chat.message.edited")} </span>
          </Show>
          <time dateTime={new Date(props.post.serverTsMs).toISOString()}>
            {postTimeLabel(props.post.serverTsMs, props.now)}
          </time>
        </div>
      </div>
      <Show when={props.post.state === "ok" && props.epoch !== undefined && props.canReact}>
        <PostReactions
          channelIdHex={props.channelIdHex}
          cursor={props.post.cursor}
          epoch={props.epoch ?? 0}
          postAtMs={props.post.serverTsMs}
          canReact={props.canReact}
        />
        <PostComments
          channelIdHex={props.channelIdHex}
          cursor={props.post.cursor}
          epoch={props.epoch ?? 0}
          postAtMs={props.post.serverTsMs}
          canModerate={props.canPublish}
        />
      </Show>
      <Show when={menu()}>
        {(anchor) => (
          <MessageMenu
            anchor={anchor()}
            canEditDelete={props.canPublish}
            copyText={props.post.text.length > 0 ? props.post.text : null}
            onReact={react}
            onMoreReactions={() => setPicker(true)}
            reactionRow={props.canReact ? reactionRow(null, [], allowed(), []) : []}
            ownReactions={own()}
            showExpand={props.canReact && rowExpands(allowed())}
            capReached={own().length >= maxPerPerson()}
            onReply={() => undefined}
            canReply={false}
            /* UX `attachments.md` section 11a: never a poll or checklist, a
               contact card only while its person lets it be shared. */
            canForward={canForwardContent(props.post.content, (k) => profiles.cardShareableOf(k))}
            onForward={() => props.onForward()}
            pinLabel={null}
            onPin={() => undefined}
            saveLabel={props.saved ? "unsave" : "save"}
            onSave={() => props.onToggleSaved()}
            onCopy={(text) => void copyMessageText(text)}
            canSelect={false}
            onSelect={() => undefined}
            onEdit={() => props.onEdit(props.post.cursor, props.post.text)}
            canEdit={props.post.content === undefined}
            extraActions={contentActions()}
            onDelete={() => props.onDelete(props.post.cursor)}
            onClose={() => setMenu(null)}
          />
        )}
      </Show>
      <Show when={picker()}>
        <EmojiPicker
          mode="reaction"
          sessionToken={props.sessionToken}
          allowedOnly={((): readonly string[] | undefined => {
            const a = allowed();
            return a.mode === "only" ? a.emoji : undefined;
          })()}
          isDisabled={(emoji) => own().length >= maxPerPerson() && !own().includes(emoji)}
          onEmoji={(emoji) => {
            setPicker(false);
            react(emoji);
          }}
          onClose={() => setPicker(false)}
        />
      </Show>
    </li>
  );
}
