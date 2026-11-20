// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Comments under a channel post, in place (UX `group-and-channel-privacy.md`
 * 13.2 and 13.3; ADR-0087 section 13a).
 *
 * Collapsed: the first 2, then `Show {n} more comments` (or `Show more
 * comments` while a page is unread), or a quiet `Comment` link when there are
 * none. Open: 20 at a time, oldest first, `Show more` at the end, the field,
 * and `Hide comments` to fold it again. Comments that arrive while it is open
 * wait behind `{n} new comments`.
 *
 * Each comment is sealed under the post's epoch seed and signed by a key of
 * its own; this account's comments are known from its synced register (their
 * nonces), never asked of the service. A long press or a right click opens
 * Reply, Copy, Delete (its author; the owner and publishers) and `Ban from
 * comments` (the owner and publishers), and `React`: a comment takes
 * reactions the way a post does (phase B, `comment-feedback.ts`).
 *
 * The count under a collapsed post is the service's maintained comment count
 * (read with the post's reaction counts), not a count of rows read; a post
 * whose count is 0 reads no page.
 */
import { accountSuspended } from "../auth/suspension-state";
import { SuspendedComposerLine } from "../auth/SuspendedReadOnly";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";

import { base64Encode } from "../auth/account-id";
import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { MessageText } from "../chat/MessageText";
import { uiRules } from "../chat/ui-rules";
import { copyMessageText } from "../chat/copy-message";
import { b64ToBytes, bytesToB64, type AvatarData } from "../profile/avatar-model";
import { rememberCardAvatar } from "../profile/avatar-runtime";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { createLongPress } from "../ui/long-press";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { loadWasm, wasmSync, type Wasm } from "../wasm";
import { banFromComments, isBannedAuthor, rememberCommentAuthor } from "./ban-list";
import { channelSeedFor } from "./channel-access";
import { bindChannelFeedback } from "./ChannelFeedbackRuntime";
import { EmojiPicker } from "../media/EmojiPicker";
import { MessageMenu, type MenuAnchor } from "../chat/MessageMenu";
import { reactionRow, rowExpands } from "../chat/reaction-rules";
import { channelAllowedReactions } from "./ban-list";
import { requestCommentReactionCounts, ownCommentReactions, commentReactionCap, toggleCommentReaction, CommentReactionCapError } from "./comment-feedback";
import { CommentReactions, createCapLine } from "./CommentReactions";
import { postCommentCount, requestReactionCounts } from "./post-feedback";
import {
  COMMENT_PAGE,
  collapsedView,
  needsFirstRead,
  EMPTY_THREAD,
  openView,
  quotedComment,
  showArrived,
  showMorePage,
  withoutComment,
  withPage,
  type CommentView,
  type ThreadState,
} from "./comment-thread";
import {
  addCommentRow,
  bytesOfHex,
  deleteCommentAsAuthor,
  deleteCommentAsPublisher,
  hexOf,
  listCommentRows,
  type CommentRow,
} from "./feedback-api";
import { ownCommentNoncesOn, ownFeedbackPort } from "./own-feedback-register";

type Identity = InstanceType<Wasm["Identity"]>;

/** Between reads of an open thread for new comments (chosen). */
const OPEN_POLL_MS = 15_000;

/** A comment this device is sending, or failed to send. */
interface OutgoingComment {
  readonly localId: number;
  readonly text: string;
  readonly replyTo: number | null;
  readonly nonce: Uint8Array;
  readonly sealed: Uint8Array;
  readonly writerKey: Uint8Array;
  readonly status: "sending" | "failed";
}

let nextLocalId = 1;

/** Open one row; `null` when it does not open (fail-closed: never shown). */
function openRow(
  w: Wasm,
  seed: Uint8Array,
  channelId: Uint8Array,
  epoch: number,
  postCursor: number,
  row: CommentRow,
): CommentView | null {
  let opened;
  try {
    opened = w.openChannelComment(seed, channelId, epoch, BigInt(postCursor), bytesOfHex(row.writerKeyHex), row.sealed);
  } catch {
    return null;
  }
  try {
    const authorHex = hexOf(opened.authorKey);
    const a = opened.avatar;
    let avatar: AvatarData | null = null;
    if (a !== undefined) {
      try {
        avatar = {
          still: {
            blobIdB64: bytesToB64(a.blobId),
            keyB64: bytesToB64(a.key),
            nonceB64: bytesToB64(a.nonce),
            size: a.size,
            side: a.side,
          },
          video: null,
        };
      } finally {
        a.free();
      }
    }
    rememberCardAvatar(base64Encode(opened.authorKey), avatar);
    rememberCommentAuthor(authorHex, opened.name);
    const reply = opened.replyTo;
    return {
      cursor: row.cursor,
      writerKeyHex: row.writerKeyHex,
      authorHex,
      name: opened.name,
      text: opened.text,
      replyTo: reply === undefined ? null : Number(reply),
      serverTsMs: row.serverTsMs,
    };
  } finally {
    opened.free();
  }
}

export function PostComments(props: {
  channelIdHex: string;
  cursor: number;
  epoch: number;
  postAtMs: number;
  canModerate: boolean;
}): JSX.Element {
  const chats = useChats();
  const auth = useAuth();
  bindChannelFeedback(auth, chats);
  const profiles = useProfiles();
  const channelId = createMemo(() => bytesOfHex(props.channelIdHex));
  const seed = (): Uint8Array | undefined => channelSeedFor(props.channelIdHex, props.epoch);

  const [thread, setThread] = createSignal<ThreadState>(EMPTY_THREAD);
  const [status, setStatus] = createSignal<"loading" | "ready" | "failed">("loading");
  const [outgoing, setOutgoing] = createSignal<readonly OutgoingComment[]>([]);
  const [draft, setDraft] = createSignal("");
  const [replyTo, setReplyTo] = createSignal<CommentView | null>(null);
  const [error, setError] = createLiveText();
  const [menu, setMenu] = createSignal<{ readonly anchor: MenuAnchor; readonly c: CommentView } | null>(null);

  const identity = (): Identity | null => chats.conversationAccountIdentity();
  const banned = (authorHex: string): boolean => isBannedAuthor(props.channelIdHex, authorHex);

  /** This account's comments under the post: writer key hex -> nonce. */
  const ownKeys = createMemo((): ReadonlyMap<string, Uint8Array> => {
    const id = identity();
    const out = new Map<string, Uint8Array>();
    if (id === null) return out;
    let w: Wasm;
    try {
      w = wasmSync();
    } catch {
      return out;
    }
    for (const nonce of ownCommentNoncesOn(channelId(), props.cursor)) {
      try {
        const wk = new w.ChannelCommentWriteKey(id, channelId(), BigInt(props.cursor), nonce);
        try {
          out.set(hexOf(wk.publicKey), nonce);
        } finally {
          wk.free();
        }
      } catch {
        // A nonce of the wrong length is skipped.
      }
    }
    return out;
  });

  async function read(after: number, mode: "page" | "poll"): Promise<void> {
    const s = seed();
    if (s === undefined) return;
    const w = await loadWasm();
    const page = await listCommentRows(props.channelIdHex, props.cursor, after, COMMENT_PAGE);
    const opened: CommentView[] = [];
    for (const row of page.comments) {
      const v = openRow(w, s, channelId(), props.epoch, props.cursor, row);
      if (v !== null) opened.push(v);
    }
    // The page length decides `done`, so rows that did not open still count.
    setThread(withPage(thread(), opened, page.nextAfter, mode, page.comments.length));
  }

  async function loadFirst(): Promise<void> {
    setStatus("loading");
    try {
      await read(0, "page");
      setStatus("ready");
    } catch {
      setStatus("failed");
    }
  }

  const serverCount = (): number | undefined => postCommentCount(props.channelIdHex, props.cursor);

  // The post's comment count comes with its reaction counts (one batched
  // call for the posts in view). The first page is read once the count says
  // there is something to read; a post with none reads nothing. If the count
  // never comes (an older service, a failed call), the page is read anyway.
  let started = false;
  function start(): void {
    if (started || seed() === undefined) return;
    started = true;
    void loadFirst();
  }
  onMount(() => {
    requestReactionCounts(props.channelIdHex, [props.cursor]);
    const fallback = setTimeout(start, 3000);
    onCleanup(() => clearTimeout(fallback));
  });
  createEffect(() => {
    const n = serverCount();
    if (n === undefined || seed() === undefined) return;
    if (needsFirstRead(n)) {
      // A count above what was read (someone commented since): read on.
      if (!started) start();
      else if (status() === "ready" && !thread().expanded && thread().done && n > thread().rowsRead) {
        void read(thread().nextAfter, "page").catch(() => undefined);
      }
      return;
    }
    if (!started) {
      started = true;
      setThread({ ...thread(), done: true });
      setStatus("ready");
    }
  });

  // An open thread looks for new comments while the page is visible.
  onMount(() => {
    const timer = setInterval(() => {
      if (!thread().expanded || document.visibilityState !== "visible" || status() !== "ready") return;
      void read(thread().nextAfter, "poll").catch(() => undefined);
    }, OPEN_POLL_MS);
    onCleanup(() => clearInterval(timer));
  });

  async function onShowMore(): Promise<void> {
    const st = thread();
    const view = openView(st, banned);
    const visibleCount = st.rows.filter((c) => !banned(c.authorHex)).length;
    if (view.shown.length >= visibleCount && !st.done) {
      try {
        await read(st.nextAfter, "page");
      } catch {
        setStatus("failed");
        return;
      }
    }
    setThread(showMorePage(thread()));
  }

  function expand(): void {
    setThread({ ...thread(), expanded: true });
  }

  function collapse(): void {
    setThread({ ...showArrived(thread()), expanded: false, shownCount: COMMENT_PAGE });
    setReplyTo(null);
  }

  async function postOutgoing(o: OutgoingComment): Promise<void> {
    const id = identity();
    if (id === null) return;
    const w = await loadWasm();
    const wk = new w.ChannelCommentWriteKey(id, channelId(), BigInt(props.cursor), o.nonce);
    try {
      await addCommentRow(props.channelIdHex, props.cursor, hexOf(wk.publicKey), o.sealed, wk.signAdd(o.sealed));
    } catch {
      setOutgoing((list) => list.map((x) => (x.localId === o.localId ? { ...x, status: "failed" } : x)));
      return;
    } finally {
      wk.free();
    }
    setOutgoing((list) => list.filter((x) => x.localId !== o.localId));
    requestReactionCounts(props.channelIdHex, [props.cursor]);
    // The own comment shows in place: read on from the end at once.
    try {
      await read(thread().nextAfter, "page");
    } catch {
      // The next read shows it.
    }
  }

  async function onSend(): Promise<void> {
    const text = draft().trim();
    if (text.length === 0) return;
    const id = identity();
    const s = seed();
    const port = ownFeedbackPort();
    if (id === null || s === undefined || port === null) return;
    const w = await loadWasm();
    try {
      w.checkCommentText(text);
    } catch {
      setError(() => t("channels.comments.invalid"));
      return;
    }
    setError(null);
    const own = profiles.ownProfile();
    const name = own.displayName ?? profiles.ownUsername() ?? "";
    const nonce = w.newChannelCommentNonce();
    const wk = new w.ChannelCommentWriteKey(id, channelId(), BigInt(props.cursor), nonce);
    let sealed: Uint8Array;
    const writerKey = wk.publicKey;
    let avatarRef: InstanceType<Wasm["AvatarRef"]> | undefined;
    let avatarHandedOver = false;
    try {
      const still = own.avatar?.still;
      if (still !== undefined) {
        try {
          avatarRef = new w.AvatarRef(
            b64ToBytes(still.blobIdB64),
            b64ToBytes(still.keyB64),
            b64ToBytes(still.nonceB64),
            still.size,
            still.side,
          );
        } catch {
          avatarRef = undefined;
        }
      }
      const target = replyTo();
      avatarHandedOver = true;
      sealed = w.sealChannelComment(
        id,
        s,
        channelId(),
        props.epoch,
        BigInt(props.cursor),
        writerKey,
        name,
        avatarRef,
        text,
        target === null ? undefined : BigInt(target.cursor),
      );
    } catch {
      setError(() => t("channels.comments.invalid"));
      return;
    } finally {
      // `avatarRef` is not freed once handed over: `sealChannelComment` takes
      // it by value, and a free on the emptied wrapper throws.
      wk.free();
      if (!avatarHandedOver) avatarRef?.free();
    }
    // Recorded before it is posted, so any device of the account can delete
    // it whatever happens next.
    try {
      await port.recordComment(channelId(), props.cursor, nonce, props.postAtMs);
    } catch {
      // The register could not be written: the comment is still sent; only
      // the account's other devices will not know it is theirs.
    }
    const o: OutgoingComment = {
      localId: nextLocalId++,
      text,
      replyTo: replyTo()?.cursor ?? null,
      nonce,
      sealed,
      writerKey,
      status: "sending",
    };
    setOutgoing((list) => [...list, o]);
    setDraft("");
    setReplyTo(null);
    await postOutgoing(o);
  }

  async function onDelete(c: CommentView): Promise<void> {
    setMenu(null);
    const id = identity();
    if (id === null) return;
    const nonce = ownKeys().get(c.writerKeyHex);
    try {
      if (nonce !== undefined) {
        const w = await loadWasm();
        const wk = new w.ChannelCommentWriteKey(id, channelId(), BigInt(props.cursor), nonce);
        try {
          await deleteCommentAsAuthor(props.channelIdHex, props.cursor, c.cursor, wk.signDelete(BigInt(c.cursor)));
        } finally {
          wk.free();
        }
        await ownFeedbackPort()?.deleteComment(channelId(), props.cursor, nonce, props.postAtMs);
      } else if (props.canModerate) {
        await deleteCommentAsPublisher(id, props.channelIdHex, props.cursor, c.cursor);
      } else {
        return;
      }
      setThread(withoutComment(thread(), c.cursor));
      requestReactionCounts(props.channelIdHex, [props.cursor]);
    } catch {
      setError(() => t("channels.comments.delete_failed"));
    }
  }

  async function onBan(c: CommentView): Promise<void> {
    setMenu(null);
    const id = identity();
    if (id === null) return;
    try {
      await banFromComments(id, props.channelIdHex, c.authorHex);
    } catch {
      setError(() => t("channels.ban.failed"));
    }
  }

  const collapsed = createMemo(() => collapsedView(thread(), banned, serverCount(), uiRules()));
  const opened = createMemo(() => openView(thread(), banned));
  const shown = (): readonly CommentView[] => (thread().expanded ? opened().shown : collapsed().shown);

  // The reaction counts of the comments on screen, one call per post.
  createEffect(() => {
    const cursors = shown().map((c) => c.cursor);
    if (cursors.length > 0) requestCommentReactionCounts(props.channelIdHex, props.cursor, cursors);
  });

  /** The comment whose `React` picker is open. */
  const [picking, setPicking] = createSignal<CommentView | null>(null);
  /** Comment cursor -> cap line shown under it. */
  const [capFor, setCapFor] = createSignal<number | null>(null);
  const cap = createCapLine();
  const raiseCap = (cursor: number): void => {
    setCapFor(cursor);
    cap.raise();
  };
  const ownOn = (c: CommentView): readonly string[] =>
    ownCommentReactions({ idHex: props.channelIdHex, post: props.cursor, comment: c.cursor });
  async function react(c: CommentView, emoji: string): Promise<void> {
    try {
      await toggleCommentReaction(
        { idHex: props.channelIdHex, post: props.cursor, comment: c.cursor, epoch: props.epoch, postAtMs: props.postAtMs },
        emoji,
      );
    } catch (err) {
      if (err instanceof CommentReactionCapError) raiseCap(c.cursor);
      // eslint-disable-next-line no-console
      else console.warn("[channels] comment reaction failed", err instanceof Error ? err.name : "unknown");
    }
  }

  function CommentRowView(rowProps: { c: CommentView }): JSX.Element {
    const c = rowProps.c;
    const isOwn = (): boolean => ownKeys().has(c.writerKeyHex);
    const quoted = (): CommentView | null => quotedComment(thread().rows, c.replyTo);
    let li: HTMLLIElement | undefined;
    const openMenu = (x: number, y: number): void => {
      const r = li?.getBoundingClientRect();
      const bubble = r === undefined ? { left: x, top: y, right: x, bottom: y } : { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
      setMenu({ anchor: { bubble, x, y }, c });
    };
    const longPress = createLongPress(openMenu);
    return (
      <li
        ref={(el) => {
          li = el;
        }}
        class="lk-comment"
        classList={{ own: isOwn() }}
        onContextMenu={(e) => {
          e.preventDefault();
          openMenu(e.clientX, e.clientY);
        }}
        onPointerDown={longPress.onPointerDown}
        onPointerMove={longPress.onPointerMove}
        onPointerUp={longPress.onPointerUp}
        onPointerCancel={longPress.onPointerCancel}
        onPointerLeave={longPress.onPointerLeave}
        style={{ display: "flex", gap: "var(--space-2)", "align-items": "flex-start" }}
      >
        <Avatar label={c.name} id={c.authorHex} size="sm" personKey={base64Encode(bytesOfHex(c.authorHex))} />
        <div class="lk-comment-body" style={{ "min-width": "0" }}>
          <bdi class="lk-comment-name" style={{ "font-weight": "500" }}>
            {c.name}
          </bdi>
          <Show when={c.replyTo !== null}>
            <div class="lk-comment-quote lk-meta">
              {quoted() === null
                ? t("chat.reply.quote")
                : t("chat.reply.quote_to", { name: quoted()?.name ?? "" })}
              <Show when={quoted()}>{(q) => <span class="lk-ellipsis"> {q().text}</span>}</Show>
            </div>
          </Show>
          <div class="lk-comment-text">
            <MessageText text={c.text} />
          </div>
          <CommentReactions
            channelIdHex={props.channelIdHex}
            post={props.cursor}
            comment={c.cursor}
            epoch={props.epoch}
            postAtMs={props.postAtMs}
            capLine={cap.shown() && capFor() === c.cursor}
            onCap={() => raiseCap(c.cursor)}
          />
        </div>
      </li>
    );
  }

  return (
    <Show when={seed() !== undefined}>
      <section class="lk-post-comments" aria-label={t("channels.comments.comment")}>
        <Show when={status() === "loading" && thread().rows.length === 0}>
          <div
            class="lk-comments-loading"
            role="progressbar"
            aria-label={t("common.loading")}
            style={{ height: "2px", background: "var(--surface-border)", "border-radius": "var(--radius-pill)" }}
          />
        </Show>
        <Show when={status() === "failed"}>
          <p class="lk-meta" role="status">
            {t("channels.comments.failed")}{" "}
            <button type="button" class="ghost small" onClick={() => void loadFirst()}>
              {t("common.try_again")}
            </button>
          </p>
        </Show>
        <Show when={shown().length > 0}>
          <ul class="lk-comment-list" style={{ "list-style": "none", margin: "0", padding: "0" }}>
            <For each={shown()}>{(c) => <CommentRowView c={c} />}</For>
          </ul>
        </Show>
        <Show when={thread().expanded}>
          <For each={outgoing()}>
            {(o) => (
              <div class="lk-comment pending" style={{ display: "flex", gap: "var(--space-2)" }}>
                <div class="lk-comment-text">
                  <MessageText text={o.text} />
                </div>
                <Show
                  when={o.status === "failed"}
                  fallback={<span class="lk-meta">{t("common.sending")}</span>}
                >
                  <button
                    type="button"
                    class="ghost small"
                    onClick={() => {
                      setOutgoing((list) =>
                        list.map((x) => (x.localId === o.localId ? { ...x, status: "sending" } : x)),
                      );
                      void postOutgoing({ ...o, status: "sending" });
                    }}
                  >
                    {t("chat.media.not_sent_retry")}
                  </button>
                </Show>
              </div>
            )}
          </For>
          <Show when={opened().showMore}>
            <button type="button" class="ghost small lk-comments-more" onClick={() => void onShowMore()}>
              {t("channels.comments.more")}
            </button>
          </Show>
          <Show when={opened().arrived > 0}>
            <button type="button" class="ghost small" onClick={() => setThread(showArrived(thread()))}>
              {t("channels.comments.new", { count: opened().arrived })}
            </button>
          </Show>
          <Show when={replyTo()}>
            {(r) => (
              <div class="lk-reply-strip lk-meta" style={{ display: "flex", gap: "var(--space-2)", "align-items": "center" }}>
                <span class="lk-ellipsis">{t("chat.reply.strip_label", { name: r().name })}</span>
                <button
                  type="button"
                  class="lk-icon-button"
                  aria-label={t("chat.reply.cancel")}
                  onClick={() => setReplyTo(null)}
                >
                  <Icon name="xmark" size={20} />
                </button>
              </div>
            )}
          </Show>
          {/* ADR-0098 step 3: a suspended account comments on nothing. */}
          <SuspendedComposerLine />
          <form
            class="lk-comment-field"
            style={{ display: accountSuspended() ? "none" : "flex", gap: "var(--space-2)" }}
            onSubmit={(e) => {
              e.preventDefault();
              void onSend();
            }}
          >
            <input
              type="text"
              class="lk-search"
              placeholder={t("channels.comments.write")}
              aria-label={t("channels.comments.write")}
              value={draft()}
              onInput={(e) => setDraft(e.currentTarget.value)}
            />
            <button
              type="submit"
              class="lk-send-glyph"
              aria-label={t("chat.composer.send")}
              title={t("chat.composer.send")}
              disabled={draft().trim().length === 0}
            >
              <Icon name="arrow-up-circle-fill" size={31} />
            </button>
          </form>
          <Show when={error()}>
            {(msg) => (
              <p class="form-error" role="alert">
                {msg()}
              </p>
            )}
          </Show>
          <button type="button" class="ghost small lk-comments-hide" onClick={collapse}>
            {t("channels.comments.hide")}
          </button>
        </Show>
        <Show when={!thread().expanded && status() === "ready"}>
          <Show when={collapsed().more}>
            {(more) => (
              <button type="button" class="ghost small lk-comments-more" onClick={expand}>
                {(() => {
                  const m = more();
                  if (m.kind === "count") return t("channels.comments.show_more", { count: m.n });
                  if (m.kind === "unknown") return t("channels.comments.show_more_unknown");
                  return t("channels.comments.comment");
                })()}
              </button>
            )}
          </Show>
          <Show when={collapsed().more === null}>
            <button type="button" class="ghost small lk-comments-more" onClick={expand}>
              {t("channels.comments.write")}
            </button>
          </Show>
        </Show>
        {/* Keyed: the handlers read the comment from this render, never a
            stale accessor after the menu closed. */}
        <Show when={menu()} keyed>
          {(m) => {
            const own = ownKeys().has(m.c.writerKeyHex);
            const allowed = channelAllowedReactions(props.channelIdHex);
            const mine = ownOn(m.c);
            return (
              <MessageMenu
                anchor={m.anchor}
                canEditDelete={own || props.canModerate}
                canEdit={false}
                copyText={m.c.text.length > 0 ? m.c.text : null}
                onReact={(emoji) => void react(m.c, emoji)}
                onMoreReactions={() => setPicking(m.c)}
                reactionRow={allowed.mode === "off" ? [] : reactionRow(null, [], allowed, [])}
                ownReactions={mine}
                showExpand={allowed.mode !== "off" && rowExpands(allowed)}
                capReached={mine.length >= commentReactionCap()}
                onReply={() => {
                  setThread({ ...thread(), expanded: true });
                  setReplyTo(m.c);
                }}
                canReply={true}
                canForward={false}
                onForward={() => undefined}
                pinLabel={null}
                onPin={() => undefined}
                saveLabel={null}
                onCopy={(text) => void copyMessageText(text)}
                canSelect={false}
                onSelect={() => undefined}
                onEdit={() => undefined}
                onDelete={() => void onDelete(m.c)}
                extraDestructive={
                  props.canModerate && !own
                    ? [{ label: t("channels.ban.action"), icon: "nosign", onClick: () => void onBan(m.c) }]
                    : []
                }
                onClose={() => setMenu(null)}
              />
            );
          }}
        </Show>
        <Show when={picking()} keyed>
          {(c) => (
            <EmojiPicker
              mode="reaction"
              sessionToken={() => null}
              allowedOnly={((): readonly string[] | undefined => {
                const a = channelAllowedReactions(props.channelIdHex);
                return a.mode === "only" ? a.emoji : undefined;
              })()}
              isDisabled={(emoji) => ownOn(c).length >= commentReactionCap() && !ownOn(c).includes(emoji)}
              onEmoji={(emoji) => {
                setPicking(null);
                void react(c, emoji);
              }}
              onClose={() => setPicking(null)}
            />
          )}
        </Show>
      </section>
    </Show>
  );
}
