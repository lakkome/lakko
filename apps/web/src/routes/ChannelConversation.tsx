// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/channel/:id` — a channel, opened where every conversation opens: beside
 * the Chats list (ADR-0087 section 12; UX
 * `docs/ux/group-and-channel-privacy.md` sections 12 and 13).
 *
 * The header is a conversation header (avatar, title, one line) and the name
 * opens the info as a pushed screen. The timeline holds the posts as cards at
 * the width of the pane, each with its reactions and comments under it. The
 * owner and the publishers have the composer; a subscriber has none, and the
 * timeline runs to the bottom of the pane. There are no call buttons.
 *
 * The channel route, its own list pane, its own header and the Manage page
 * are gone. The machinery that kept a channel alive while the Manage page was
 * mounted runs app-wide (`channels/ChannelPublisherLoops.tsx`).
 */
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, untrack, type JSX } from "solid-js";
import { useLocation, useNavigate, useParams } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import {
  composeDeleteEnvelope,
  composeForwardEnvelope,
  parseBodyEnvelopeWith,
  reencryptAttachmentRef,
  type AttachmentRefBytes,
} from "../chat/attachments";
import { base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { ChatClient } from "../chat/client";
import { conversationKindOf } from "../chat/conversation-kind";
import { ForwardPicker } from "../chat/ForwardPicker";
import type { ForwardPickerRow } from "../chat/forward-picker";
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";
import { saveEdit, savedEntryKey, savedKeySet, saveRefusal, unsaveEdit } from "../chat/saved-messages";
import { sendTextToConversation } from "../chat/send-text";
import { channelIdFromHex } from "../channels/composer";
import type { TimelinePost } from "../channels/channel-timeline";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { KeyedError } from "../l10n/keyed-error";
import { sealedWakeForGroup } from "../push/group-routing";
import { screenAnchor } from "../ui/screen-anchor";
import { infoHeading, infoOpenLabel } from "../chat/conversation-info";
import { useChats } from "../chat/ChatContext";
import { useMembershipEnd } from "../chat/MembershipEndContext";
import { MembershipEndDialog } from "../chat/MembershipEndNotices";
import type { EndNotice } from "../chat/membership-end";
import { channelAvatarKey } from "../channels/channel-avatar";
import {
  catchUpChannel,
  channelReadStateOf,
  channelRoleOf,
  channelSeedFor,
  channelVisibilityOf,
  publishChannelPost,
  subscribeToChannel,
  type ChannelAccessKey,
} from "../channels/channel-access";
import { channelErrorText } from "../channels/channel-error-copy";
import { channelFacts, channelHeaderLine } from "../channels/channel-facts";
import { channelDayLabels, channelTimeline } from "../channels/channel-timeline";
import { dayLabelOf, sameDay } from "../chat/timeline-layout";
import { ChannelComposer } from "../channels/ChannelComposer";
import { ChannelInfo, ChannelInfoScreenHost, type ChannelInfoScreen } from "../channels/ChannelInfo";
import { ChannelPostItem } from "../channels/ChannelPostItem";
import { fetchPosts, type ChannelPostView } from "../channels/api";
import { foldBanListFromPlaintext } from "../channels/ban-list";
import { foldChannelMeta } from "../channels/channel-meta";
import { decodeAndDecryptPost } from "../channels/composer";
import { pollWhileVisible } from "../channels/open-channel-poll";
import { requestReactionCounts } from "../channels/PostReactions";
import { toPostDecode, type PostDecode } from "../channels/post-decode";
import { requestChannelPostSync } from "../feed/ChannelPostSync";
import { markChannelRead } from "../feed/feed-marks-store";
import { createMediaHub } from "../media/media-hub";
import { t } from "../l10n";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { PushedScreen } from "../ui/PushedScreen";
import { loadWasm } from "../wasm";
import { paths } from "./paths";

/** What a search card or a link page may hand the route: how to subscribe
 *  when the person has not yet. */
export interface ChannelRouteState {
  readonly access?: ChannelAccessKey;
  readonly name?: string;
  readonly description?: string;
  readonly handle?: string;
}

export default function ChannelConversation(): JSX.Element {
  const route = useParams<{ id: string }>();
  // Keyed on the id: work an instance starts (a fetch, a decode) spans awaits
  // and must finish under its own channel, never the next one opened.
  return (
    <Show when={route.id} keyed>
      {(id) => <ChannelConversationFor id={id} />}
    </Show>
  );
}

function ChannelConversationFor(props: { readonly id: string }): JSX.Element {
  const id = untrack(() => props.id);
  const auth = useAuth();
  const chats = useChats();
  const navigate = useNavigate();
  const location = useLocation<ChannelRouteState>();

  const membershipEnd = useMembershipEnd();
  const role = (): ReturnType<typeof channelRoleOf> => channelRoleOf(id);
  /** UX 9.5 — removed from this channel while it was open: the notice is a
   *  dialog over it, not a banner beside it. */
  const endedHere = (): EndNotice | null => membershipEnd.notices().find((n) => n.key === `3:${id}`) ?? null;
  const canPublish = (): boolean => role() === "owner" || role() === "publisher";
  const reading = (): boolean => canPublish() || channelReadStateOf(id) === "reading";
  const facts = (): ReturnType<typeof channelFacts> => channelFacts(id);

  const [infoOpen, setInfoOpen] = createSignal(false);
  const [infoScreen, setInfoScreen] = createSignal<ChannelInfoScreen | null>(null);
  const [posts, setPosts] = createSignal<readonly ChannelPostView[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [loadFailed, setLoadFailed] = createSignal(false);
  const [decoded, setDecoded] = createSignal<ReadonlyMap<number, PostDecode>>(new Map());
  const [epochs, setEpochs] = createSignal<ReadonlyMap<number, number>>(new Map());
  const [editing, setEditing] = createSignal<{ cursor: number; text: string } | null>(null);
  const [opError, setOpError] = createSignal<string | null>(null);

  // ----- subscribing from the route (a card or a link page handed the key) -----
  const [subscribing, setSubscribing] = createSignal(false);
  const [subscribeError, setSubscribeError] = createSignal<string | null>(null);
  async function subscribe(): Promise<void> {
    const state = location.state;
    if (state?.access === undefined || subscribing()) return;
    setSubscribing(true);
    setSubscribeError(null);
    try {
      await subscribeToChannel(id, state.access, {
        name: state.name ?? "",
        description: state.description ?? "",
        ...(state.handle === undefined ? {} : { handle: state.handle }),
      });
      void load();
    } catch (err) {
      setSubscribeError(channelErrorText(err, () => t("channels.subscribe.error.generic")));
    } finally {
      setSubscribing(false);
    }
  }

  // ----- the posts: fetched, decoded under the seed of each post's epoch -----
  let decodeSeq = 0;
  /** Decode `page` (the whole timeline held), and say whether a post names an
   *  epoch this device holds no seed for: only then are seeds caught up. */
  async function decodeAll(page: readonly ChannelPostView[]): Promise<boolean> {
    const mine = ++decodeSeq;
    const wasm = await loadWasm();
    const held = untrack(decoded);
    const heldEpochs = untrack(epochs);
    const out = new Map<number, PostDecode>();
    const ep = new Map<number, number>();
    let missingSeed = false;
    for (const p of page) {
      // A post already opened is not opened again on every poll.
      const prior = held.get(p.cursor);
      const priorEpoch = heldEpochs.get(p.cursor);
      if (prior !== undefined && priorEpoch !== undefined && prior.kind !== "older-key" && prior.kind !== "unreadable") {
        out.set(p.cursor, prior);
        ep.set(p.cursor, priorEpoch);
        continue;
      }
      const res = await decodeAndDecryptPost(id, p.payloadB64, (e) => channelSeedFor(id, e));
      ep.set(p.cursor, res.epochId);
      if (!res.hadSeed) missingSeed = true;
      const d = toPostDecode(
        res,
        (bytes) => wasm.classifyChannelPostPlaintext(bytes),
        (bytes) => parseBodyEnvelopeWith(wasm, bytes),
      );
      out.set(p.cursor, d);
      if (d.kind === "ban-list") foldBanListFromPlaintext(id, d.plaintext);
    }
    await foldChannelMeta(id, out.values());
    if (mine !== decodeSeq) return missingSeed;
    setDecoded(out);
    setEpochs(ep);
    return missingSeed;
  }

  /**
   * Read the channel. The first read pulls the seeds and the whole retained
   * page; a later one (every 10 s while the channel is open and visible) asks
   * only for posts after the newest held, and pulls seeds only when a new post
   * names an epoch this device cannot open yet (a rotation) - an idle open
   * channel costs one small request per poll.
   */
  let caughtUp = false;
  async function loadOnce(): Promise<void> {
    if (!reading()) {
      setLoaded(true);
      return;
    }
    try {
      if (!caughtUp) {
        await catchUpChannel(id);
        caughtUp = true;
      }
      const held = untrack(posts);
      const after = held.reduce((max, p) => Math.max(max, p.cursor), 0);
      const page = await fetchPosts(id, after);
      setLoadFailed(false);
      if (page.posts.length === 0 && untrack(decoded).size === held.length && held.length > 0) return;
      const known = new Set(held.map((p) => p.cursor));
      const merged = [...held, ...page.posts.filter((p) => !known.has(p.cursor))];
      setPosts(merged);
      if (await decodeAll(merged)) {
        await catchUpChannel(id);
        await decodeAll(merged);
      }
    } catch {
      setLoadFailed(untrack(posts).length === 0);
    } finally {
      setLoaded(true);
    }
  }

  /** One read at a time: a load asked for while one runs is folded into a
   *  single follow-up pass, so opening a channel reads the epoch and the
   *  posts once each instead of once per trigger (mount, poll, read state). */
  let inFlight: Promise<void> | null = null;
  let again = false;
  let redecode = false;
  function load(opts: { readonly redecode?: boolean } = {}): Promise<void> {
    if (opts.redecode === true) redecode = true;
    if (inFlight !== null) {
      again = true;
      return inFlight;
    }
    inFlight = (async (): Promise<void> => {
      do {
        again = false;
        await loadOnce();
        if (redecode) {
          redecode = false;
          await decodeAll(untrack(posts));
        }
      } while (again);
    })().finally(() => {
      inFlight = null;
    });
    return inFlight;
  }
  onMount(() => void load());
  // Nothing pushes a post to an open channel: it is read again while visible.
  onCleanup(pollWhileVisible(() => load()));
  // A seed that arrives later (a reseal, being let in) opens what it covers:
  // the held posts are decoded again, the unreadable ones included. Keyed on
  // the read state's VALUE, so the catch-up's own epoch write does not start
  // another read.
  const readState = createMemo(() => channelReadStateOf(id));
  createEffect(on(readState, () => void load({ redecode: true }), { defer: true }));
  const timeline = createMemo(() => channelTimeline(posts(), decoded()));

  // Reactions in view, one batched read (ADR-0087 section 13a).
  createEffect(() => {
    const cursors = timeline()
      .posts.filter((p) => p.state === "ok")
      .map((p) => p.cursor)
      .slice(-50);
    if (cursors.length > 0 && reading()) requestReactionCounts(id, cursors);
  });

  // ----- the list scrolls to its end and the newest post shown is read -----
  let scroller: HTMLDivElement | undefined;
  const markNewestRead = (): void => {
    const s = auth.state();
    const newest = timeline().posts.at(-1)?.cursor;
    if (s.kind !== "session" || newest === undefined || document.visibilityState !== "visible") return;
    void markChannelRead(s.account.accountId, id, newest).catch(() => undefined);
  };
  createEffect(
    on(
      () => timeline().posts.length,
      () =>
        queueMicrotask(() => {
          if (scroller !== undefined) scroller.scrollTop = scroller.scrollHeight;
          markNewestRead();
        }),
    ),
  );
  onMount(() => {
    const onVisible = (): void => markNewestRead();
    document.addEventListener("visibilitychange", onVisible);
    onCleanup(() => document.removeEventListener("visibilitychange", onVisible));
  });

  const [clock, setClock] = createSignal(new Date());
  onMount(() => {
    const timer = setInterval(() => setClock(new Date()), 60_000);
    onCleanup(() => clearInterval(timer));
  });
  // A day separator precedes the first post of its day, as in chats.
  const dayLabels = createMemo(() =>
    channelDayLabels(timeline().posts, (iso) => dayLabelOf(iso, clock()), sameDay),
  );

  const hub = createMediaHub({ voiceQueue: () => [] });
  onCleanup(() => hub.dispose());

  // ----- Save and Forward (UX 12: the post menu's rows) -----
  const sync = useDeviceGroupSync();
  const savedKeys = createMemo(() => savedKeySet(sync.saved()));
  const isSaved = (cursor: number): boolean => savedKeys().has(savedEntryKey(id, cursor));
  async function toggleSaved(post: TimelinePost): Promise<void> {
    setOpError(null);
    const ref = { kind: 3 as const, id: channelIdFromHex(id) };
    try {
      if (isSaved(post.cursor)) {
        await sync.applyFolderEdits([unsaveEdit(ref, post.cursor)]);
        return;
      }
      if (saveRefusal(sync.saved().length, 1) !== null) {
        setOpError(t("chat.error.save"));
        return;
      }
      await sync.applyFolderEdits([
        saveEdit(
          { ref, messageId: post.cursor, author: facts().name, text: post.text, attachments: post.attachments, sentAtMs: post.serverTsMs },
          Date.now(),
        ),
      ]);
    } catch {
      setOpError(t("chat.error.save"));
    }
  }

  const outside = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    wakeSealer: async (groupId, kind) => {
      const s = auth.state();
      if (s.kind !== "session") return null;
      return sealedWakeForGroup(s.account.accountId, base64Encode(groupId), kind);
    },
  });
  const [forwarding, setForwarding] = createSignal<TimelinePost | null>(null);
  /** A post forwarded into a conversation: its files re-encrypted (no blob is
   *  shared between the two), attributed to the channel by name, with no
   *  origin to jump to (ADR-0052 decision 2). */
  async function forwardTo(row: ForwardPickerRow): Promise<void> {
    const post = forwarding();
    const s = auth.state();
    if (post === null || s.kind !== "session") return;
    const entry = chats.get(row.id);
    if (entry === undefined) throw new KeyedError("chat.error.conversation_gone");
    const here = screenAnchor(location);
    const fresh: AttachmentRefBytes[] = [];
    for (const a of post.attachments) fresh.push(await reencryptAttachmentRef(a));
    const forward = { author: facts().name };
    const envelope = await composeForwardEnvelope(post.text, fresh, forward);
    await sendTextToConversation({
      chat: outside,
      accountId: s.account.accountId,
      entry,
      text: post.text,
      envelope,
      attachments: fresh,
      forward,
    });
    setForwarding(null);
    if (here()) {
      const slug = base64UrlEncodeNoPad(entry.group.groupId);
      navigate(conversationKindOf(entry.group) === "chat" ? paths.chat(slug) : paths.groupChat(slug));
    }
  }

  // ----- publishing -----
  async function publish(plaintext: Uint8Array): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    if (identity === null) throw Object.assign(new Error("sign in"), { code: "sign-in" });
    await publishChannelPost(identity, id, plaintext);
    requestChannelPostSync();
    await load();
  }

  // Delete for everyone, as a chat's Delete is: no dialog (UX 13).
  async function deletePost(cursor: number): Promise<void> {
    setOpError(null);
    try {
      await publish(await composeDeleteEnvelope(cursor));
    } catch (err) {
      setOpError(channelErrorText(err, () => t("channels.publish.error.generic")));
    }
  }

  const headerLine = (): string => {
    if (role() === null) return "";
    if (!canPublish() && channelReadStateOf(id) === "waiting") return t("channels.subscribe.status.waiting");
    return channelHeaderLine(channelVisibilityOf(id), null);
  };

  return (
    <article class="lk-chat lk-channel">
      <div class="lk-chat-body" style={{ position: "relative" }}>
        <header class="lk-chat-head">
          <button
            type="button"
            class="lk-icon-button lk-narrow-only"
            title={t("nav.chats")}
            aria-label={t("chat.header.back")}
            onClick={() => navigate(paths.home)}
          >
            <Icon name="back" size={20} />
          </button>
          <button
            type="button"
            class="lk-chat-title lk-channel-title"
            title={infoOpenLabel("channel")}
            aria-label={infoOpenLabel("channel")}
            aria-haspopup="dialog"
            disabled={role() === null}
            onClick={() => setInfoOpen(true)}
          >
            <Avatar label={facts().name} id={id} size="sm" conversationKey={channelAvatarKey(id)} />
            <div class="lk-chat-title-text">
              <h1>{facts().name}</h1>
              <Show when={headerLine().length > 0}>
                <span class="lk-chat-sub">{headerLine()}</span>
              </Show>
            </div>
          </button>
        </header>

        <ForwardPicker open={forwarding() !== null} onClose={() => setForwarding(null)} onPick={forwardTo} />

        <PushedScreen open={infoOpen()} title={infoHeading("channel")} onBack={() => setInfoOpen(false)}>
          <ChannelInfo
            id={id}
            onOpenScreen={setInfoScreen}
            onEnded={() => {
              setInfoOpen(false);
              navigate(paths.home, { replace: true });
            }}
          />
        </PushedScreen>
        {/* A screen the info pushed covers the pane, info header included. */}
        <ChannelInfoScreenHost id={id} screen={infoOpen() ? infoScreen() : null} onClose={() => setInfoScreen(null)} />

        <Show when={endedHere()} keyed>
          {(notice) => (
            <MembershipEndDialog
              notice={notice}
              body={t("channels.ended.notice_body")}
              onDone={() => {
                membershipEnd.dismissNotice(notice.key);
                // Chats, with the ended channel not left behind in history.
                navigate(paths.home, { replace: true });
              }}
            />
          )}
        </Show>

        <Show
          when={role() !== null}
          fallback={
            <div class="lk-empty">
              <Icon name="channels" size={40} strokeWidth={1.4} />
              <strong>{t("channels.subscribe.prompt_title")}</strong>
              <Show when={location.state?.access !== undefined}>
                <button type="button" class="primary" disabled={subscribing()} onClick={() => void subscribe()}>
                  {subscribing() ? t("channels.subscribe.status.subscribing") : t("channels.subscribe.action")}
                </button>
              </Show>
              <Show when={subscribeError()}>
                {(m) => (
                  <p class="form-error" role="alert">
                    {m()}
                  </p>
                )}
              </Show>
            </div>
          }
        >
          <div class="lk-chat-scroll lk-channel-scroll" ref={scroller}>
            <Show when={!canPublish() && channelReadStateOf(id) === "waiting"}>
              <div class="lk-alert info lk-channel-notice" role="status">
                {t("channels.link_page.waiting")}
              </div>
            </Show>
            <Show when={timeline().earlierUnavailable}>
              <p class="lk-channel-earlier">{t("channels.posts.earlier_unavailable")}</p>
            </Show>
            <Show when={loadFailed()}>
              <div class="lk-alert danger lk-channel-notice" role="alert">
                {t("channels.posts.error.load")}
                <button type="button" class="ghost small" onClick={() => void load()}>
                  {t("common.try_again")}
                </button>
              </div>
            </Show>
            <Show
              when={timeline().posts.length > 0}
              fallback={
                <Show when={reading()}>
                  <p class="lk-channel-empty">
                    {!loaded()
                      ? t("common.loading")
                      : canPublish()
                        ? t("channels.posts.empty_owner")
                        : t("channels.posts.empty")}
                  </p>
                </Show>
              }
            >
              <ul class="lk-post-list">
                <For each={timeline().posts}>
                  {(post, i) => (
                    <>
                    <Show when={dayLabels()[i()]}>
                      {(label) => (
                        <li class="lk-day-sep" aria-label={label()}>
                          <span>{label()}</span>
                        </li>
                      )}
                    </Show>
                    <ChannelPostItem
                      channelIdHex={id}
                      post={post}
                      epoch={epochs().get(post.cursor)}
                      hub={hub}
                      canPublish={canPublish()}
                      canReact={reading()}
                      now={clock()}
                      sessionToken={() => auth.identitySecrets()?.sessionToken ?? null}
                      onEdit={(cursor, text) => setEditing({ cursor, text })}
                      onDelete={(cursor) => void deletePost(cursor)}
                      onForward={() => setForwarding(post)}
                      saved={isSaved(post.cursor)}
                      onToggleSaved={() => void toggleSaved(post)}
                    />
                    </>
                  )}
                </For>
              </ul>
            </Show>
          </div>
          <Show when={opError()}>
            {(m) => (
              <div class="lk-alert danger lk-channel-notice" role="alert">
                {m()}
              </div>
            )}
          </Show>
          <Show when={canPublish()}>
            <ChannelComposer publish={publish} editing={editing()} onCancelEdit={() => setEditing(null)} hub={hub} />
          </Show>
        </Show>
      </div>
    </article>
  );
}
