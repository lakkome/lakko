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
import { accountSuspended } from "../auth/suspension-state";
import { SuspendedComposerLine } from "../auth/SuspendedReadOnly";
import { createEffect, createMemo, createResource, createSignal, For, on, onCleanup, onMount, Show, untrack, type JSX } from "solid-js";
import { useLocation, useNavigate, useParams } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { useProfiles } from "../profile/ProfileContext";
import {
  composeContentEnvelope,
  composeDeleteEnvelope,
  composeForwardEnvelope,
  contentPartsOfForward,
  parseBodyEnvelopeWith,
  reencryptAttachmentRef,
  type AttachmentRefBytes,
} from "../chat/attachments";
import { base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { ChatClient } from "../chat/client";
import { canForwardContent } from "../chat/forward";
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
  channelReaderDeviceRoleOf,
  channelRoleOf,
  channelSeedFor,
  channelSubscriberCount,
  channelVisibilityOf,
  publishChannelPost,
  subscribeToChannel,
  type ChannelAccessKey,
} from "../channels/channel-access";
import { channelErrorText } from "../channels/channel-error-copy";
import { channelFacts, channelHeaderLine } from "../channels/channel-facts";
import { channelDayLabels, channelTimeline, deletedPostCursors } from "../channels/channel-timeline";
import { DeletedRowStub } from "../chat/DeletedRowStub";
import {
  DeletedHold,
  idsEnteringHold,
  prefersReducedMotion,
  type DeletedPhase,
} from "../chat/deleted-rows";
import { dayLabelOf, sameDay } from "../chat/timeline-layout";
import { ChannelComposer } from "../channels/ChannelComposer";
import { ChannelInfo, ChannelInfoScreenHost, type ChannelInfoScreen } from "../channels/ChannelInfo";
import { ChannelPostItem } from "../channels/ChannelPostItem";
import { useOwnMemberCard } from "../channels/own-member-card";
import { fetchPosts, type ChannelPostView } from "../channels/api";
import { openChannelReport } from "../moderation/channel-report";
import { registerLivePosts } from "../moderation/report-runtime";
import { attachmentsPreviewLabel } from "../chat/list-preview";
import { CONVERSATION_CHANNEL } from "../moderation/report-model";

/** A post's base64url payload as bytes (padding optional). */
function base64UrlToBytesForReport(b64url: string): Uint8Array {
  const b64 = b64url.replace(/-/g, "+").replace(/_/g, "/");
  const bin = atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}
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
import { channelsState } from "../state/channels-state";
import { loadWasm } from "../wasm";
import { getDeviceKey } from "../auth/key-store";
import { loadChannelPostLog } from "../feed/channel-post-log-store";
import { channelWriteControls, heldPostsOf } from "../channels/removed-channel";
import { channelRemovalLine, channelRemovalLineKey } from "../moderation/channel-removal";
import { paths } from "./paths";
import { Portal } from "solid-js/web";
import { ArticleReader } from "../attach/ArticleReader";
import { composeChannelPollClose } from "../attach/ChannelContent";
import { requestConfirm } from "../ui/confirm-dialog";
import { displayLocale } from "../l10n/display-locale";
import type { Article, MessageContent } from "../../wasm/lakko_ffi_wasm";
import type { AttachEditRequest } from "../attach/AttachComposer";
import { connectionPhase } from "../net/connection-state";
import { connectionHeaderText } from "../net/connection-copy";

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
  const profiles = useProfiles();
  const navigate = useNavigate();
  const location = useLocation<ChannelRouteState>();

  const membershipEnd = useMembershipEnd();
  const role = (): ReturnType<typeof channelRoleOf> => channelRoleOf(id);
  /** UX 9.5 — removed from this channel while it was open: the notice is a
   *  dialog over it, not a banner beside it. */
  const endedHere = (): EndNotice | null => membershipEnd.notices().find((n) => n.key === `3:${id}`) ?? null;
  const canPublish = (): boolean => role() === "owner" || role() === "publisher";
  /** 2026-11-13 (UX second-device.md section 4a): the account owns or
   *  publishes in this channel, but this device holds no publishing state for
   *  it: a reader here. */
  const readerRole = (): "owner" | "publisher" | null => channelReaderDeviceRoleOf(id);
  const reading = (): boolean => canPublish() || channelReadStateOf(id) === "reading";
  const facts = (): ReturnType<typeof channelFacts> => channelFacts(id);
  // ADR-0099 step 5: moderators removed the channel. It stays, the posts on
  // this device stay readable, nothing that writes is offered.
  const removalLine = (): ReturnType<typeof channelRemovalLine> => channelRemovalLine(id);
  const removed = (): boolean => removalLine() !== null;
  const controls = (): ReturnType<typeof channelWriteControls> =>
    channelWriteControls({ canPublish: canPublish(), reading: reading(), accountSuspended: accountSuspended(), removed: removed() });

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
      if (removed()) {
        // The service refuses a removed channel: what this device holds is
        // shown instead, and no load failure is claimed.
        setLoadFailed(false);
        if (untrack(posts).length === 0) await showHeldPosts();
      } else {
        setLoadFailed(untrack(posts).length === 0);
      }
    } finally {
      setLoaded(true);
    }
  }

  /** A removed channel opened with nothing fetched in this view: the posts
   *  the channel-post log holds for it. */
  async function showHeldPosts(): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session") return;
    try {
      const key = await getDeviceKey(s.account.accountId);
      if (key === null) return;
      const log = await loadChannelPostLog(s.account.accountId, key);
      const held = heldPostsOf(log?.channels[id]);
      if (held.posts.length === 0 || untrack(posts).length > 0 || !removed()) return;
      setPosts(held.posts);
      setDecoded(held.decoded);
    } catch {
      // Nothing held: the empty line says so.
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
  // Nothing pushes a post (or a comment) to an open channel: it is read again
  // while visible, and the counts of the posts in view with it, which is how
  // a comment written elsewhere reaches the threads on screen (UX
  // `group-and-channel-privacy.md` 13.2).
  onCleanup(
    pollWhileVisible(async () => {
      await load();
      requestCountsInView();
    }),
  );
  // A seed that arrives later (a reseal, being let in) opens what it covers:
  // the held posts are decoded again, the unreadable ones included. Keyed on
  // the read state's VALUE, so the catch-up's own epoch write does not start
  // another read.
  const readState = createMemo(() => channelReadStateOf(id));
  // A restore: the channel answers again, and is read from the start as if
  // it had never been removed (the held posts carry no attachments).
  createEffect(
    on(
      removed,
      (now, before) => {
        if (before !== true || now) return;
        caughtUp = false;
        setPosts([]);
        setDecoded(new Map());
        setEpochs(new Map());
        void load();
      },
      { defer: true },
    ),
  );
  createEffect(on(readState, () => void load({ redecode: true }), { defer: true }));
  // UX 15.2.1 — earlier seeds handed to a new publisher (or kept after a
  // rotation) open what they cover in place: the posts appear and "Earlier
  // posts aren't available." goes once every earlier item opens.
  const heldSeedCount = createMemo(() => channelsState.subscribedSeedsByChannel().get(id)?.size ?? 0);
  createEffect(on(heldSeedCount, () => void load({ redecode: true }), { defer: true }));
  // docs/ux/message-actions.md section 9 — a post deleted while the person
  // looks at the channel reads "Message deleted" for 2 s and collapses;
  // every other deleted post is simply not there. In memory only.
  const [postHeld, setPostHeld] = createSignal<ReadonlyMap<number, DeletedPhase>>(new Map());
  const postHold = new DeletedHold((phases) => setPostHeld(phases), prefersReducedMotion);
  onCleanup(() => postHold.clear());
  onMount(() => {
    const onVisibility = (): void => {
      if (document.visibilityState !== "visible") postHold.clear();
    };
    document.addEventListener("visibilitychange", onVisibility);
    onCleanup(() => document.removeEventListener("visibilitychange", onVisibility));
  });
  const timeline = createMemo(() => channelTimeline(posts(), decoded(), new Set(postHeld().keys())));
  /** Posts that already had their hold: their removal is not a new one. */
  const postsHeldOnce = new Set<number>();
  createEffect(
    on(timeline, (now, prev) => {
      if (prev === undefined) return;
      const still = new Set(now.posts.map((p) => p.cursor));
      // Drawn before (a read post, not a loading card) and gone now.
      const gone = prev.posts
        .filter((p) => p.state === "ok" && p.deleted !== true && !still.has(p.cursor) && !postsHeldOnce.has(p.cursor))
        .map((p) => p.cursor);
      if (gone.length === 0) return;
      const deletedNow = deletedPostCursors(untrack(posts), untrack(decoded));
      const enter = idsEnteringHold({
        drawnBefore: new Set(gone),
        newlyDeleted: gone.filter((c) => deletedNow.has(c)),
        inFront: document.visibilityState === "visible",
      });
      for (const c of enter) postsHeldOnce.add(c);
      postHold.start(enter);
    }),
  );

  // ADR-0060 section 3: a subscriber keeps its member card current while the
  // channel is open: once it is let in and holds a seed (a private channel's
  // subscriber holds none until the owner's device lets it in), after a
  // rotation, a renamed profile or a changed setting. Base walk 2026-11-13:
  // no card was ever uploaded from web since channels became conversations.
  const newestHeldEpoch = createMemo((): number | undefined => {
    const held = channelsState.subscribedSeedsByChannel().get(id);
    if (held === undefined || held.size === 0) return undefined;
    return Math.max(...held.keys());
  });
  useOwnMemberCard({
    channelIdHex: () => id,
    keys: () => channelsState.subscriberKeysByChannel().get(id),
    epoch: newestHeldEpoch,
    seed: () => {
      const epoch = newestHeldEpoch();
      return epoch === undefined ? undefined : channelsState.subscribedSeedsByChannel().get(id)?.get(epoch);
    },
    // A reader device of the owner or a publisher uploads no member card,
    // as the publishing device does not (it may hold no registered key).
    enabled: () => !canPublish() && readerRole() === null && channelReadStateOf(id) === "reading",
  });

  // Reactions in view, one batched read (ADR-0087 section 13a). The comment
  // counts come with them.
  function requestCountsInView(): void {
    const cursors = timeline()
      .posts.filter((p) => p.state === "ok" && p.deleted !== true)
      .map((p) => p.cursor)
      .slice(-50);
    if (cursors.length > 0 && reading() && !removed()) requestReactionCounts(id, cursors);
  }
  createEffect(requestCountsInView);

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
    // ADR-0092 / UX `attachments.md` section 11a — a location, contact card
    // or article travels as the same content (an article's images are the
    // post's attachments, re-encrypted above, in their order).
    const content = post.content;
    if (!canForwardContent(content, (k) => profiles.cardShareableOf(k))) {
      throw new KeyedError("chat.forward.error.generic");
    }
    const envelope =
      content === undefined
        ? await composeForwardEnvelope(post.text, fresh, forward)
        : await composeContentEnvelope(content, fresh, contentPartsOfForward(forward));
    await sendTextToConversation({
      chat: outside,
      accountId: s.account.accountId,
      entry,
      text: content === undefined ? post.text : "",
      envelope,
      attachments: fresh,
      forward,
      ...(content === undefined ? {} : { content }),
    });
    setForwarding(null);
    if (here()) {
      const slug = base64UrlEncodeNoPad(entry.group.groupId);
      navigate(conversationKindOf(entry.group) === "chat" ? paths.chat(slug) : paths.groupChat(slug));
    }
  }

  // ----- ADR-0099 section 2: reporting the channel's posts -----
  /** A post's menu: the channel is the target, the post is included. */
  function reportPost(cursor: number): void {
    openChannelReport({
      channelIdHex: id,
      name: facts().name,
      conversationKind: CONVERSATION_CHANNEL,
      picked: [cursor],
      fixed: [],
      leave: null,
    });
  }
  // The posts as fetched, for a report's evidence and its "Choose messages"
  // list while this view is open.
  onCleanup(
    registerLivePosts(id, {
      post: (cursor) => {
        const view = posts().find((p) => p.cursor === cursor);
        if (view === undefined) return undefined;
        try {
          return base64UrlToBytesForReport(view.payloadB64);
        } catch {
          return undefined;
        }
      },
      candidates: () =>
        timeline().posts.map((p) => ({
          id: p.cursor,
          authorKeyB64: undefined,
          text: p.text,
          filesLabel: attachmentsPreviewLabel(p.attachments.map((a) => ({ mime: a.mime ?? null, kind: a.kind ?? null }))),
          createdAt: new Date(p.serverTsMs).toISOString(),
          deleted: p.state !== "ok" || p.deleted === true,
        })),
    }),
  );

  // ----- publishing -----
  async function publish(plaintext: Uint8Array): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    if (identity === null) throw Object.assign(new Error("sign in"), { code: "sign-in" });
    await publishChannelPost(identity, id, plaintext);
    requestChannelPostSync();
    await load();
  }

  // Delete for everyone, as a chat's Delete is: no dialog (UX 13).
  /** ADR-0092 — an article post open in the reader. */
  const [articleOpen, setArticleOpen] = createSignal<{
    article: Article;
    attachments: readonly AttachmentRefBytes[];
    atMs: number;
  } | null>(null);

  /** ADR-0092 — `Edit checklist` / `Edit` on an article post, handed to the
   *  composer's editor. */
  const [contentEdit, setContentEdit] = createSignal<AttachEditRequest | null>(null);
  async function editContent(cursor: number, content: MessageContent, attachments: readonly AttachmentRefBytes[]): Promise<void> {
    if (content.kind !== "article" || attachments.length === 0) {
      setContentEdit({ messageId: cursor, content });
      return;
    }
    const images: Blob[] = [];
    for (const ref of attachments) {
      try {
        images.push(await (await fetch(await hub.load(ref))).blob());
      } catch {
        setOpError(t("chat.attachment.error.download"));
        return;
      }
    }
    setContentEdit({ messageId: cursor, content, images });
  }

  /** UX 8.2 — `Close checklist`: asked first, final. */
  async function closeChecklist(cursor: number): Promise<void> {
    const ok = await requestConfirm({
      title: t("checklist.close_confirm"),
      body: null,
      confirmLabel: t("checklist.close"),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (!ok) return;
    setOpError(null);
    try {
      const w = await loadWasm();
      await publish(w.composePollCloseEnvelope(BigInt(cursor)));
    } catch (err) {
      setOpError(channelErrorText(err, () => t("channels.publish.error.generic")));
    }
  }

  /** UX 7.7 — `Close poll`: asked first; the close freezes the results this
   *  app reads now into the post. */
  async function closePoll(post: TimelinePost): Promise<void> {
    if (post.content?.kind !== "poll") return;
    const ok = await requestConfirm({
      title: t("poll.close_confirm"),
      body: null,
      confirmLabel: t("poll.close"),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (!ok) return;
    setOpError(null);
    try {
      await publish(await composeChannelPollClose(id, epochs().get(post.cursor), post.cursor, post.content.poll.body));
    } catch (err) {
      setOpError(channelErrorText(err, () => t("channels.publish.error.generic")));
    }
  }

  async function deletePost(cursor: number): Promise<void> {
    setOpError(null);
    try {
      await publish(await composeDeleteEnvelope(cursor));
    } catch (err) {
      setOpError(channelErrorText(err, () => t("channels.publish.error.generic")));
    }
  }

  // UX group-and-channel-privacy.md section 12: the header line carries the
  // subscriber count where this device can know it, the owner's and a
  // publisher's (the count route answers only them, section 15.1).
  const [subscriberCount] = createResource(
    () => (canPublish() && !removed() ? id : null),
    async (channelId) => {
      const identity = chats.conversationAccountIdentity();
      if (identity === null) return null;
      try {
        return await channelSubscriberCount(identity, channelId);
      } catch {
        return null;
      }
    },
  );
  const headerLine = (): string => {
    // docs/ux/connection-state.md: the connection state replaces the line.
    const connection = connectionHeaderText(connectionPhase());
    if (connection !== null) return connection;
    if (role() === null) return "";
    if (!canPublish() && channelReadStateOf(id) === "waiting") return t("channels.subscribe.status.waiting");
    return channelHeaderLine(channelVisibilityOf(id), canPublish() ? (subscriberCount() ?? null) : null);
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
            <Icon name="chevron-back" size={24} />
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
            <Avatar label={facts().name} id={id} size="sm" conversationKey={channelAvatarKey(id)} playVideo="header" />
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
            <Show when={removalLine()}>
              {(line) => (
                <div class="lk-alert info lk-channel-notice lk-channel-removed" role="status">
                  {t(channelRemovalLineKey(line()))}
                </div>
              )}
            </Show>
            <Show when={!removed() && !canPublish() && channelReadStateOf(id) === "waiting"}>
              <div class="lk-alert info lk-channel-notice" role="status">
                {t("channels.link_page.waiting")}
              </div>
            </Show>
            <Show when={timeline().earlierUnavailable}>
              <p class="lk-channel-earlier">{t("channels.posts.earlier_unavailable")}</p>
            </Show>
            <Show when={loadFailed() && !removed()}>
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
                    <Show
                      when={post.deleted !== true}
                      fallback={
                        <DeletedRowStub class="lk-post lk-channel-post" phase={postHeld().get(post.cursor) ?? "hold"} />
                      }
                    >
                    <ChannelPostItem
                      channelIdHex={id}
                      post={post}
                      epoch={epochs().get(post.cursor)}
                      hub={hub}
                      canPublish={controls().editDelete}
                      canReact={controls().reactAndComment}
                      now={clock()}
                      sessionToken={() => auth.identitySecrets()?.sessionToken ?? null}
                      onEdit={(cursor, text) => setEditing({ cursor, text })}
                      onDelete={(cursor) => void deletePost(cursor)}
                      onForward={() => setForwarding(post)}
                      saved={isSaved(post.cursor)}
                      onToggleSaved={() => void toggleSaved(post)}
                      onReport={controls().reportPost ? () => reportPost(post.cursor) : undefined}
                      onClosePoll={() => void closePoll(post)}
                      publish={publish}
                      onEditContent={(cursor, content, attachments) => void editContent(cursor, content, attachments)}
                      onCloseChecklist={(cursor) => void closeChecklist(cursor)}
                      onOpenArticle={(article, attachments) =>
                        setArticleOpen({ article, attachments, atMs: post.serverTsMs })
                      }
                    />
                    </Show>
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
          {/* ADR-0099 step 3: a suspended publisher posts nothing. */}
          <Show when={canPublish() && !removed()}>
            <SuspendedComposerLine />
            <Show when={controls().composer}>
              <ChannelComposer
                publish={publish}
                editing={editing()}
                onCancelEdit={() => setEditing(null)}
                hub={hub}
                contentEdit={contentEdit}
                clearContentEdit={() => setContentEdit(null)}
                trayKey={id}
              />
            </Show>
          </Show>
          {/* UX second-device.md section 4a: the account posts here from
              another device; one line in place of the composer, no control. */}
          <Show when={!canPublish() ? readerRole() : null}>
            {(r) => (
              <p class="lk-channel-reader-line">
                {r() === "owner" ? t("channels.reader_device.owner") : t("channels.reader_device.publisher")}
              </p>
            )}
          </Show>
        </Show>
      </div>
            <Show when={articleOpen()}>
              {(a) => (
                <Portal>
                  <ArticleReader
                    article={a().article}
                    imageUrl={async (index) => {
                      const ref = a().attachments[index];
                      if (ref === undefined) return null;
                      try {
                        return await hub.load(ref);
                      } catch {
                        return null;
                      }
                    }}
                    author={channelFacts(id)?.name ?? ""}
                    dateText={new Date(a().atMs).toLocaleString(displayLocale())}
                    onClose={() => setArticleOpen(null)}
                    onMenu={() => setArticleOpen(null)}
                    onLink={(url) => window.open(url, "_blank", "noopener,noreferrer")}
                  />
                </Portal>
              )}
            </Show>
    </article>
  );
}
