// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Channels routes (docs/ux/client-redesign.md section 5.4).
 *
 *   NewChannel       — `/channels/new`      — the create form.
 *   JoinCoPublisher  — `/channels/join`     — become a co-publisher from an
 *                                             owner's link (ADR-0028 OQ3).
 *   ChannelView      — `/channels/:id`      — posts as cards, the publish
 *                                             composer and the Manage screen
 *                                             for an owner; a Subscribe
 *                                             prompt for anyone else.
 *   ChannelSubscribe — `/channels/:id/subscribe` — the subscriber's view:
 *                                             register, wait to be let in,
 *                                             read.
 *
 * The view components live in `src/channels/` (`PostCard`, `PublishComposer`,
 * `ManagePanel` and its three sections, `ChannelRow` for the list pane).
 * This file is the route-level orchestration: fetching, decoding, and the
 * publish / subscribe flows. Real ChannelPost composition + signature verify
 * + AEAD open are wired: the composer composes a signed encrypted post via
 * `composePost` and the subscriber path opens the seed envelope via
 * `openSubscriberSeedEnvelope`. The server stays opaque.
 *
 * Styling is the `lk-` class vocabulary in `styles.css` (tokens, light and
 * dark); this file carries no colour literal and no local palette.
 */

import { anonTokenMessage } from "../tokens/token-copy";
import {
  createEffect,
  createMemo,
  createResource,
  createSignal,
  For,
  Match,
  onCleanup,
  onMount,
  Show,
  Switch,
  type Component,
  type JSX,
} from "solid-js";
import { Navigate, useLocation, useNavigate, useParams } from "@solidjs/router";

import {
  apiError,
  createChannel,
  fetchEnvelopes,
  fetchEpoch,
  fetchPosts,
  publishPost,
  subscribe,
  type ChannelPostView,
  channelErrorText,
} from "../channels/api";
import { requestChannelPostSync } from "../feed/ChannelPostSync";
import {
  bytesToHex,
  composePost,
  decodeAndDecryptPost,
  freshPublisherKeys,
  freshSubscriberKeys,
  openSubscriberSeedEnvelope,
  utf8Encode,
} from "../channels/composer";
import { pollForCoPublishWelcome, requestToCoPublish } from "../channels/copublisher";
import { LegacyChannelError } from "../channels/legacy-v1";
import { coPublisherJoinFailure } from "../channels/copublisher-join-failure";
import { JOIN_TRY_AGAIN } from "./join-group-failure";
import { DiscoveryHttpError, registerChannelHandle } from "../channels/channel-discovery";
import { channelTitle, displayHandle } from "../channels/channel-row";
import { knownSubscriberCount } from "../channels/ui-cache";
import { channelInfoSections, infoHeading, infoOpenLabel } from "../chat/conversation-info";
import { uiRules } from "../chat/ui-rules";
import { foldChannelMeta, publishChannelMeta } from "../channels/channel-meta";
import { ManagePanel } from "../channels/ManagePanel";
import { isHiddenPostDecode, PostCard, toPostDecode, type PostDecode } from "../channels/PostCard";
import { PublishComposer } from "../channels/PublishComposer";
import { DsClient } from "../ds/client";
import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import {
  awaitChannelsHydration,
  channelsActions,
  channelsState,
} from "../state/channels-state";
import { loadWasm } from "../wasm";
import { paths } from "./paths";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { ModalLayer } from "../ui/ModalLayer";
import { requestConfirm } from "../ui/confirm-dialog";

// ============================================================================
// Shared pieces
// ============================================================================

/** Posts oldest first, so the newest sits at the bottom like a chat. */
function oldestFirst(posts: ReadonlyArray<ChannelPostView>): ChannelPostView[] {
  return [...posts].sort((a, b) => a.cursor - b.cursor);
}

/**
 * The channel header: avatar, name, one line under it. The name opens the
 * channel's info (`docs/ux/conversation-info.md` section 1), which is the
 * same gesture the chat header carries and the only way in.
 */
function ChannelHead(props: {
  readonly id: string;
  readonly name: string;
  readonly sub: string;
  readonly onOpenInfo: () => void;
  readonly actions?: JSX.Element;
}): JSX.Element {
  const navigate = useNavigate();
  return (
    <header class="lk-chat-head">
      <button
        type="button"
        class="lk-icon-button lk-narrow-only"
        title="Channels"
        aria-label="Back to channels"
        onClick={(): void => navigate(paths.channels)}
      >
        <Icon name="back" size={20} />
      </button>
      <button
        type="button"
        class="lk-chat-title lk-channel-title"
        title={infoOpenLabel("channel")}
        aria-label={infoOpenLabel("channel")}
        aria-haspopup="dialog"
        onClick={(): void => props.onOpenInfo()}
      >
        <Avatar label={props.name} id={props.id} size="sm" />
        <div class="lk-chat-title-text">
          <h1>{props.name}</h1>
          <Show when={props.sub.length > 0}>
            <span class="lk-chat-sub">{props.sub}</span>
          </Show>
        </div>
      </button>
      <div class="lk-chat-actions">{props.actions}</div>
    </header>
  );
}

/**
 * What a channel's header opens (`docs/ux/conversation-info.md` section
 * 2.3): name, handle and description, how many subscribers where this device
 * knows, and the one action that belongs to the viewer - the owner's manage
 * screen for a publisher, stop following for a subscriber.
 *
 * Which of those appear is decided in `chat/conversation-info.ts`, where it
 * can be tested; this renders what it is told.
 */
function ChannelInfoBody(props: {
  readonly id: string;
  readonly isPublisher: boolean;
  readonly isSubscriber: boolean;
  readonly onManage: () => void;
  readonly onUnsubscribe: () => void;
}): JSX.Element {
  const meta = (): ReturnType<typeof channelMetaOf> => channelMetaOf(props.id);
  const subscribers = (): number | null => knownSubscriberCount(props.id);
  const sections = createMemo(() =>
    channelInfoSections(uiRules(), {
      isPublisher: props.isPublisher,
      isSubscriber: props.isSubscriber,
      // Only where this device has actually counted them. A reader inventing
      // a number it cannot know would be worse than showing none, so the
      // count this device holds is the capability, not the role.
      subscriberCountKnown: subscribers() !== null,
    }),
  );

  return (
    <>
      <header class="lk-contact-head">
        <Avatar label={meta().name} id={props.id} size="xl" />
        <h1>{meta().name}</h1>
        <Show when={meta().handle.length > 0}>
          <div class="lk-contact-username">{meta().handle}</div>
        </Show>
        <Show when={meta().description.length > 0}>
          <p class="lk-meta">{meta().description}</p>
        </Show>
        {/* Only where this device has actually counted them; an absent count
            is left out rather than shown as zero. */}
        <Show when={sections().includes("subscriber-count") && subscribers()}>
          {(n) => (
            <div class="lk-meta">
              {n()} {n() === 1 ? "subscriber" : "subscribers"}
            </div>
          )}
        </Show>
      </header>

      <Show when={sections().includes("manage-channel")}>
        <section class="lk-card">
          <div class="lk-card-row">
            <span class="lk-card-row-icon">
              <Icon name="settings" size={18} />
            </span>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">Manage channel</span>
              <span class="lk-card-row-sub">
                Its name and description, who reads it, and who else may post.
              </span>
            </div>
            <button
              type="button"
              class="lk-card-row-action"
              style={{ color: "var(--accent)" }}
              onClick={(): void => props.onManage()}
            >
              Open
            </button>
          </div>
        </section>
      </Show>

      <Show when={sections().includes("unsubscribe")}>
        <section class="lk-card danger">
          <div class="lk-card-row">
            <span class="lk-card-row-icon">
              <Icon name="trash" size={18} />
            </span>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">Stop following</span>
              <span class="lk-card-row-sub">
                Takes the channel off your list and stops this device reading
                it. You can subscribe again later.
              </span>
            </div>
            <button
              type="button"
              class="danger lk-card-row-action"
              onClick={(): void => props.onUnsubscribe()}
            >
              Stop following
            </button>
          </div>
        </section>
      </Show>
    </>
  );
}

/** The scrolling post list, newest at the bottom; scrolls to the end when
 *  the list grows. */
function PostList(props: {
  readonly posts: ReadonlyArray<ChannelPostView>;
  readonly decodedByCursor: ReadonlyMap<number, PostDecode>;
  readonly loading: boolean;
  readonly emptyText?: string;
}): JSX.Element {
  let scroller: HTMLDivElement | undefined;
  // ADR-0051 — a metadata record (and a malformed one) is never a card.
  const ordered = createMemo(() =>
    oldestFirst(props.posts).filter((p) => !isHiddenPostDecode(props.decodedByCursor.get(p.cursor))),
  );
  createEffect(() => {
    void ordered().length;
    void props.decodedByCursor;
    queueMicrotask((): void => {
      if (scroller !== undefined) scroller.scrollTop = scroller.scrollHeight;
    });
  });
  // The card times relabel themselves as the day turns.
  const [clock, setClock] = createSignal(new Date());
  onMount(() => {
    const t = setInterval(() => setClock(new Date()), 60_000);
    onCleanup(() => clearInterval(t));
  });
  return (
    <div class="lk-chat-scroll lk-channel-scroll" ref={scroller}>
      <Show
        when={ordered().length > 0}
        fallback={
          <p class="lk-channel-empty">
            {props.loading ? "Loading..." : (props.emptyText ?? "No posts yet.")}
          </p>
        }
      >
        <ul class="lk-post-list">
          <For each={ordered()}>
            {(p): JSX.Element => (
              <PostCard post={p} decoded={props.decodedByCursor.get(p.cursor)} now={clock()} />
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
}

/**
 * Stop following a channel on this device: forget the reader keypair and the
 * seeds it recovered, and drop the channel from the local list.
 *
 * Local by design AND by what exists - `services/channels` has no route that
 * retires a subscriber, so nothing here can claim the publisher was told.
 * The copy says what actually happens: the channel leaves the list and this
 * device stops reading it.
 */
async function confirmStopFollowing(channelIdHex: string, name: string): Promise<boolean> {
  const ok = await requestConfirm({
    title: `Stop following ${name}?`,
    body:
      "It leaves your list and this device stops reading it. " +
      "You can subscribe again later.",
    confirmLabel: "Stop following",
    cancelLabel: "Cancel",
    tone: "danger",
  });
  if (!ok) return false;
  channelsActions.clearSubscriberKeys(channelIdHex);
  channelsActions.removeChannel(channelIdHex);
  return true;
}

/** Metadata for a channel id (ADR-0051 §5): the shared record first, then
 *  the local fields, then the handle. */
function channelMetaOf(id: string): { name: string; description: string; handle: string } {
  const h = channelsState.channels().find((c) => c.id === id);
  const shared = channelsState.metaByChannel().get(id);
  const handle = displayHandle(h?.localHandle);
  return {
    name: channelTitle(shared?.name, h?.localName ?? "", h?.localHandle),
    description: (shared?.description ?? h?.localDescription ?? "").trim(),
    handle,
  };
}

/** Decode a fetched page: every plaintext is classified (record / text /
 *  malformed) and the newest record is folded into the channel's shared
 *  meta. Returns the per-cursor outcomes for the list. */
async function decodePage(
  channelIdHex: string,
  posts: ReadonlyArray<ChannelPostView>,
  seedForEpoch: (epoch: number) => Uint8Array | undefined,
): Promise<ReadonlyMap<number, PostDecode>> {
  const wasm = await loadWasm();
  const entries = await Promise.all(
    posts.map(async (p) => {
      const res = await decodeAndDecryptPost(channelIdHex, p.payloadB64, seedForEpoch);
      return [p.cursor, toPostDecode(res, (bytes) => wasm.classifyChannelPostPlaintext(bytes))] as const;
    }),
  );
  await foldChannelMeta(
    channelIdHex,
    entries.map(([, d]) => d),
  );
  return new Map(entries);
}

// ============================================================================
// NewChannel — `/channels/new`
// ============================================================================

const NewChannel: Component = () => {
  const navigate = useNavigate();
  const auth = useAuth();
  const chats = useChats();
  const [name, setName] = createSignal("");
  const [description, setDescription] = createSignal("");
  // Optional `@handle` claimed via OPRF discovery (G4-5) so others can
  // subscribe by handle instead of the raw channel id.
  const [handle, setHandle] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  /** Generate a fresh 16-byte channel id locally — the server has no role in
   *  id allocation per ADR-0010 (the id is a pseudonym chosen by the
   *  publisher). `crypto.getRandomValues` is the secure-context source
   *  web-side; the route is behind the RequireSession gate so secure context
   *  is guaranteed. */
  function freshChannelIdHex(): string {
    const buf = new Uint8Array(16);
    crypto.getRandomValues(buf);
    return Array.from(buf, (b) => b.toString(16).padStart(2, "0")).join("");
  }

  async function onCreate(): Promise<void> {
    if (busy()) return;
    if (name().trim().length === 0) {
      setError("Give the channel a name.");
      return;
    }
    const secrets = auth.identitySecrets();
    if (secrets === null) {
      setError("Sign in again to create a channel.");
      return;
    }
    setBusy(true);
    setError(null);
    const id = freshChannelIdHex();
    try {
      // G4-5: claim the optional `@handle` FIRST (before the channel exists
      // on the channels service) so a first-come-first-served collision
      // (409) aborts cleanly without leaving a half-created channel. An
      // empty handle skips discovery entirely. The mapping is `@handle ->
      // channel_id`; the OPRF hash is all the server ever sees.
      const identity = chats.accountIdentity();
      if (identity === null) {
        setError("Sign in again to create a channel.");
        setBusy(false);
        return;
      }
      const wantedHandle = handle().trim().replace(/^@/, "");
      if (wantedHandle.length > 0) {
        try {
          await registerChannelHandle(wantedHandle, id, secrets.sessionToken);
        } catch (err) {
          const token = anonTokenMessage(
            err,
            "Could not claim the handle. Check your connection and try again.",
          );
          if (err instanceof DiscoveryHttpError && err.status === 409) {
            setError(`@${wantedHandle} is already taken. Pick another, or leave it blank.`);
          } else if (token !== null) {
            // ADR-0054 S3: the lookup and the register each spend a token.
            setError(token);
          } else {
            setError(`Could not claim the handle. Try again. (${String(err)})`);
          }
          setBusy(false);
          return;
        }
      }

      // The channel is registered under the owner key this account derives
      // for it (ADR-0059 section 1), so the identity is needed first.
      await createChannel(id, identity);
      // Mint local publisher keys — the content-key seed comes from an MLS
      // PublisherGroup exporter (ADR-0010 Tier 1), the signing-key seed is a
      // fresh Ed25519 keypair seed. The group is created in the SESSION-LONG
      // account identity's provider (ADR-0017 canonical identity), which the
      // channel log snapshots — a throwaway `Identity.fromSeed` would take
      // the group, and the seed every subscriber holds, with it.
      await awaitChannelsHydration();
      const keys = await freshPublisherKeys(
        identity,
        id,
        channelsState.keysByChannel().get(id)?.signingKeySeed,
      );
      channelsActions.addChannel({
        id,
        localName: name().trim(),
        localDescription: description().trim(),
        addedAtMs: Date.now(),
        ...(wantedHandle.length > 0 ? { localHandle: wantedHandle } : {}),
      });
      channelsActions.setKeys(id, keys);
      // ADR-0051 §4 — the first metadata record, right after creation, so
      // the first subscribers title the channel from it. The channel exists
      // either way; a failed post is logged and the next Save or rotation
      // re-issues it.
      try {
        await publishChannelMeta(id, { name: name(), description: description() });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("channel metadata record not posted at creation", err);
      }
      navigate(paths.channel(id));
    } catch (err) {
      if (err instanceof LegacyChannelError) {
        setError(err.message);
      } else if (apiError.is(err)) {
        setError(channelErrorText(err));
      } else {
        setError(`Could not reach Lakko. Check your connection and try again. (${String(err)})`);
      }
      setBusy(false);
    }
  }

  return (
    <div class="lk-page-pane narrow lk-stack">
      <button
        type="button"
        class="ghost small lk-narrow-only lk-self-start"
        onClick={(): void => navigate(paths.channels)}
      >
        <Icon name="back" size={16} />
        Channels
      </button>
      <div class="lk-page-head">
        <h1>New channel</h1>
        <p>Subscribers see the name and description. They travel encrypted; only ciphertext reaches the server.</p>
      </div>
      <form
        class="lk-card"
        onSubmit={(e): void => {
          e.preventDefault();
          void onCreate();
        }}
      >
        <label>
          Name
          <input
            type="text"
            required
            value={name()}
            onInput={(e): void => {
              setName(e.currentTarget.value);
            }}
          />
        </label>
        <label>
          Description (optional)
          <textarea
            rows={3}
            value={description()}
            onInput={(e): void => {
              setDescription(e.currentTarget.value);
            }}
          />
        </label>
        <label>
          Public @handle (optional)
          <input
            type="text"
            value={handle()}
            placeholder="@my-channel"
            autocapitalize="none"
            autocomplete="off"
            spellcheck={false}
            onInput={(e): void => {
              setHandle(e.currentTarget.value);
            }}
          />
          <span class="lk-meta lk-block lk-mt-4">Others can find and subscribe to the channel by it.</span>
        </label>
        <Show when={error()}>
          {(msg) => (
            <div class="lk-alert danger" role="alert">
              {msg()}
            </div>
          )}
        </Show>
        <div class="lk-actions lk-row-end">
          <button
            type="button"
            class="ghost"
            onClick={(): void => navigate(paths.channels)}
            disabled={busy()}
          >
            Cancel
          </button>
          <button type="submit" class="primary" disabled={busy()}>
            {busy() ? "Creating..." : "Create"}
          </button>
        </div>
      </form>
    </div>
  );
};

// ============================================================================
// JoinCoPublisher — `/channels/join`
// ============================================================================

/**
 * ADR-0028 OQ3, joiner side: become a co-publisher of an existing channel
 * from a `/g/` link the owner shared. Posts an external-join proposal to the
 * PublisherGroup journal, polls until the owner approves, joins, mints local
 * publisher keys, and opens the channel.
 */
const JoinCoPublisher: Component = () => {
  const navigate = useNavigate();
  const auth = useAuth();
  const chats = useChats();
  const [link, setLink] = createSignal("");
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  /** `Try again` under a failed request — only for the general failure. */
  const [offerRetry, setOfferRetry] = createSignal(false);
  const [status, setStatus] = createSignal<string | null>(null);

  async function onJoin(): Promise<void> {
    const l = link().trim();
    if (l.length === 0) return;
    // The session-long account identity: the external-join KeyPackage this
    // request mints, and the PublisherGroup the Welcome lands, live in its
    // provider, which the channel log snapshots. A throwaway identity would
    // lose both on the next reload.
    const identity = chats.accountIdentity();
    if (auth.identitySecrets() === null || identity === null) {
      setError("Sign in again to join a channel.");
      return;
    }
    setBusy(true);
    setError(null);
    setOfferRetry(false);
    setStatus("Sending your request...");
    try {
      await awaitChannelsHydration();
      const ds = new DsClient();
      const { groupId, channelIdHex, joinerKey } = await requestToCoPublish(ds, identity, l);
      // The proposal's KeyPackage private keys are in the provider now; get
      // them on disk so a reload while waiting can still open the Welcome.
      channelsActions.notePublisherGroupChanged();
      setStatus("Waiting for the channel owner to approve...");
      let keys: Awaited<ReturnType<typeof pollForCoPublishWelcome>> = null;
      for (let i = 0; i < 60 && keys === null; ++i) {
        keys = await pollForCoPublishWelcome(
          ds,
          identity,
          groupId,
          joinerKey,
          channelIdHex,
          channelsState.keysByChannel().get(channelIdHex)?.signingKeySeed,
        );
        if (keys === null) await new Promise((r) => setTimeout(r, 3000));
      }
      if (keys === null) {
        setError("No approval yet. Try again once the owner has approved.");
        return;
      }
      channelsActions.addChannel({
        id: channelIdHex,
        localName: name().trim() || "Co-published channel",
        localDescription: "",
        addedAtMs: Date.now(),
      });
      channelsActions.setKeys(channelIdHex, keys);
      navigate(paths.channel(channelIdHex));
    } catch (err) {
      // The knock's three sentences (ADR-0053); never the raw error.
      // eslint-disable-next-line no-console
      console.warn("co-publisher join: request failed", err);
      const notice = coPublisherJoinFailure(err);
      setError(notice.message);
      setOfferRetry(notice.offerRetry);
    } finally {
      setBusy(false);
      setStatus(null);
    }
  }

  return (
    <div class="lk-page-pane narrow lk-stack">
      <button
        type="button"
        class="ghost small lk-narrow-only lk-self-start"
        onClick={(): void => navigate(paths.channels)}
      >
        <Icon name="back" size={16} />
        Channels
      </button>
      <div class="lk-page-head">
        <h1>Join as co-publisher</h1>
        <p>
          Paste the link a channel owner shared. Once they approve, this device can post to the
          channel.
        </p>
      </div>
      <form
        class="lk-card"
        onSubmit={(e): void => {
          e.preventDefault();
          void onJoin();
        }}
      >
        <label>
          Link
          <input
            type="text"
            placeholder="https://lakko.me/g/..."
            autocapitalize="none"
            autocomplete="off"
            spellcheck={false}
            value={link()}
            disabled={busy()}
            onInput={(e): void => {
              setLink(e.currentTarget.value);
            }}
          />
        </label>
        <label>
          Name on this device (optional)
          <input
            type="text"
            value={name()}
            disabled={busy()}
            onInput={(e): void => {
              setName(e.currentTarget.value);
            }}
          />
        </label>
        <Show when={status()}>
          {(s) => (
            <div class="lk-alert info" role="status">
              {s()}
            </div>
          )}
        </Show>
        <Show when={error()}>
          {(msg) => (
            <div class="lk-alert danger" role="alert">
              {msg()}
            </div>
          )}
        </Show>
        <Show when={error() !== null && offerRetry()}>
          <div class="lk-actions">
            <button
              type="button"
              class="primary"
              disabled={busy()}
              onClick={(): void => void onJoin()}
            >
              {JOIN_TRY_AGAIN}
            </button>
          </div>
        </Show>
        <div class="lk-actions lk-row-end">
          <button
            type="button"
            class="ghost"
            onClick={(): void => navigate(paths.channels)}
            disabled={busy()}
          >
            Cancel
          </button>
          <button type="submit" class="primary" disabled={busy() || link().trim().length === 0}>
            {busy() ? "Requesting..." : "Request to join"}
          </button>
        </div>
      </form>
    </div>
  );
};

// ============================================================================
// ChannelView — `/channels/:id`
// ============================================================================

const ChannelView: Component = () => {
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [publishBusy, setPublishBusy] = createSignal(false);
  const [publishError, setPublishError] = createSignal<string | null>(null);
  const [composeText, setComposeText] = createSignal("");
  const [manageOpen, setManageOpen] = createSignal(false);
  const [infoOpen, setInfoOpen] = createSignal(false);

  const isOwner = (): boolean => channelsState.keysByChannel().get(params.id) !== undefined;
  const isSubscriber = (): boolean =>
    channelsState.subscriberKeysByChannel().get(params.id) !== undefined;

  // Fetches the post list + epoch when the route first opens. Both come
  // from the same server, pulled in parallel.
  const [data, { refetch }] = createResource(
    () => params.id,
    async (
      id: string,
    ): Promise<{ readonly posts: ReadonlyArray<ChannelPostView>; readonly epoch: number }> => {
      const [postsPage, epochView] = await Promise.all([fetchPosts(id, 0), fetchEpoch(id)]);
      return { posts: postsPage.posts, epoch: epochView.epoch };
    },
  );

  // Sync the cache as soon as a fetch completes so the list pane's time
  // stays fresh on back-nav.
  createEffect(() => {
    const d = data();
    if (d !== undefined) {
      channelsActions.setPosts(params.id, d.posts);
      channelsActions.setEpoch(params.id, d.epoch);
    }
  });

  const meta = (): ReturnType<typeof channelMetaOf> => channelMetaOf(params.id);

  async function onPublish(): Promise<void> {
    if (publishBusy()) return;
    const text = composeText().trim();
    if (text.length === 0) return;
    const keys = channelsState.keysByChannel().get(params.id);
    if (keys === undefined) {
      setPublishError("This device cannot post to this channel.");
      return;
    }
    setPublishBusy(true);
    setPublishError(null);
    try {
      const epoch = channelsState.epochByChannel().get(params.id) ?? 0;
      const counter = channelsActions.nextCounter(params.id);
      const wire = await composePost(keys, params.id, epoch, counter, utf8Encode(text));
      await publishPost(params.id, wire);
      setComposeText("");
      // The author's own post goes into the feed now, not at the next sync.
      requestChannelPostSync();
      await refetch();
    } catch (err) {
      if (apiError.is(err)) {
        setPublishError(`Could not publish. ${channelErrorText(err)}`);
      } else {
        setPublishError(`Could not publish. Try again. (${String(err)})`);
      }
    } finally {
      setPublishBusy(false);
    }
  }

  /** Decoded outcome per post, keyed by cursor. */
  const [decodedByCursor, setDecodedByCursor] = createSignal<ReadonlyMap<number, PostDecode>>(
    new Map(),
  );

  // Decode every post in the latest fetch. The publisher holds a single
  // content-key seed (its PublisherGroup exporter output) which it applies
  // to every post it can read; a post it cannot open renders "Not
  // available". (The subscriber path uses the ADR-0031 epoch -> seed map.)
  createEffect(() => {
    const d = data();
    const keys = channelsState.keysByChannel().get(params.id);
    if (d === undefined || keys === undefined) {
      setDecodedByCursor(new Map());
      return;
    }
    const seed = keys.seed;
    void decodePage(params.id, d.posts, () => seed).then(setDecodedByCursor);
  });

  /** Under the name (UX spec §5): the description when there is one, else
   *  the handle. */
  const subLine = (): string => {
    const m = meta();
    if (manageOpen()) return "Manage";
    if (m.description.length > 0) return m.description;
    return m.handle;
  };

  return (
    <Show
      when={isOwner() || !isSubscriber()}
      // A subscriber's reading view is the subscribe route: it holds the
      // per-epoch catch-up this view does not.
      fallback={<Navigate href={paths.channelSubscribe(params.id)} />}
    >
      <article class="lk-chat lk-channel">
        <div class="lk-chat-body">
          <ChannelHead
            id={params.id}
            name={meta().name}
            sub={subLine()}
            onOpenInfo={(): void => {
              setInfoOpen(true);
            }}
            actions={
              <Show when={isOwner()}>
                <button
                  type="button"
                  class="lk-icon-button"
                  title={manageOpen() ? "Back to posts" : "Manage"}
                  aria-label={manageOpen() ? "Back to posts" : "Manage channel"}
                  aria-pressed={manageOpen()}
                  onClick={(): void => {
                    setManageOpen(!manageOpen());
                  }}
                >
                  <Icon name={manageOpen() ? "close" : "settings"} size={20} />
                </button>
              </Show>
            }
          />
          {/* `docs/ux/conversation-info.md` — the channel's info, over the
              channel rather than beside it. The owner's manage screen is
              REACHED from here rather than moved into here: it stays mounted
              behind this view so the fan-out, the rotation heartbeat and the
              request poll keep running, and a layer that unmounts on close
              would stop them. */}
          <ModalLayer
            open={infoOpen()}
            title={infoHeading("channel")}
            onClose={(): void => {
              setInfoOpen(false);
            }}
          >
            <ChannelInfoBody
              id={params.id}
              isPublisher={isOwner()}
              isSubscriber={isSubscriber()}
              onManage={(): void => {
                setInfoOpen(false);
                setManageOpen(true);
              }}
              onUnsubscribe={(): void => {
                void confirmStopFollowing(params.id, meta().name).then((done) => {
                  if (!done) return;
                  setInfoOpen(false);
                  navigate(paths.channels);
                });
              }}
            />
          </ModalLayer>
          <Show
            when={isOwner()}
            fallback={
              <div class="lk-empty">
                <Icon name="channels" size={40} strokeWidth={1.4} />
                <strong>Subscribe to read this channel</strong>
                <span>Posts appear here once the channel lets this device in.</span>
                <button
                  type="button"
                  class="primary"
                  onClick={(): void => navigate(paths.channelSubscribe(params.id))}
                >
                  Subscribe
                </button>
              </div>
            }
          >
            <div class="lk-channel-posts" hidden={manageOpen()}>
              <PostList
                posts={data()?.posts ?? []}
                decodedByCursor={decodedByCursor()}
                loading={data.loading}
                emptyText="No posts yet. Write the first one below."
              />
              <PublishComposer
                value={composeText()}
                onInput={setComposeText}
                onSubmit={(): void => void onPublish()}
                busy={publishBusy()}
                error={publishError()}
              />
            </div>
            {/* Stays mounted while hidden: the fan-out, the rotation
                heartbeat and the request poll run whether or not the owner
                is looking (see ManagePanel). */}
            <div class="lk-chat-scroll lk-channel-manage" hidden={!manageOpen()}>
              <ManagePanel channelIdHex={params.id} />
            </div>
          </Show>
        </div>
      </article>
    </Show>
  );
};

// ============================================================================
// ChannelSubscribe — `/channels/:id/subscribe`
//   Subscriber-side view: generate (or restore) an X25519 keypair, register
//   it on the channel, poll for a seed envelope, open it, then render decoded
//   posts using the recovered seed.
// ============================================================================

const ChannelSubscribe: Component = () => {
  const params = useParams<{ id: string }>();
  const location = useLocation<{ handle?: string }>();
  const navigate = useNavigate();
  const [infoOpen, setInfoOpen] = createSignal(false);
  const [bootstrap, setBootstrap] = createSignal<
    "idle" | "registering" | "polling" | "active" | "error"
  >("idle");
  const [bootstrapError, setBootstrapError] = createSignal<string | null>(null);
  const [posts, setPosts] = createSignal<ReadonlyArray<ChannelPostView>>([]);
  const [decodedByCursor, setDecodedByCursor] = createSignal<ReadonlyMap<number, PostDecode>>(
    new Map(),
  );

  /** The handle the channel was found under, when the list pane passed one. */
  const foundHandle = (): string | undefined => {
    const state = location.state;
    return typeof state?.handle === "string" ? state.handle.replace(/^@/, "") : undefined;
  };

  // Persisted keypair lookup. If none exists on first mount, generate one
  // and store it. Waits for the per-account hydration first: on a reload
  // this route can mount before the encrypted channel log is read, and
  // minting a fresh keypair then would orphan the seeds sealed to the old
  // public key until the publisher's next rotation.
  async function ensureKeypair(): Promise<{
    readonly secret: Uint8Array;
    readonly public: Uint8Array;
  }> {
    await awaitChannelsHydration();
    const existing = channelsState.subscriberKeysByChannel().get(params.id);
    if (existing !== undefined) {
      return existing;
    }
    const fresh = await freshSubscriberKeys();
    channelsActions.setSubscriberKeys(params.id, fresh);
    return fresh;
  }

  /** Put the channel in the list pane once the registration succeeded, so a
   *  subscribed channel has a row to come back to. Idempotent on the id; an
   *  existing row (an owner's, a co-publisher's) keeps its metadata. */
  function rememberSubscribed(): void {
    if (channelsState.channels().some((c) => c.id === params.id)) return;
    const handle = foundHandle();
    channelsActions.addChannel({
      id: params.id,
      localName: handle ?? "",
      localDescription: "",
      addedAtMs: Date.now(),
      ...(handle !== undefined ? { localHandle: handle } : {}),
    });
  }

  /** Whether this view has already spent its one `channel-subscribe` token.
   *  At most one registration per open: a fresh subscribe IS that
   *  registration, and the re-sync below re-registers only a device that did
   *  not register this time. Without it the 3 s catch-up poll re-registered
   *  on every tick while waiting for the publisher's fanout, draining the
   *  token allowance in about a minute. */
  let registeredThisView = false;

  /** Drive the registration + catch-up dance. Idempotent — the server treats
   *  a re-register of the same pubkey as a no-op, and re-running the
   *  catch-up re-opens whatever the retained set currently is. A registration
   *  spends a `channel-subscribe` token (ADR-0059 section 4), so a channel
   *  this device already follows with a key it already holds is not
   *  registered again on every open; the re-sync path below re-registers
   *  when the current epoch's seed is missing. */
  async function bootstrap_(): Promise<void> {
    setBootstrap("registering");
    setBootstrapError(null);
    try {
      const alreadyFollowing =
        channelsState.channels().some((c) => c.id === params.id) &&
        channelsState.subscriberKeysByChannel().has(params.id);
      const kp = await ensureKeypair();
      const pubHex = bytesToHex(kp.public);
      if (!alreadyFollowing) {
        registeredThisView = true;
        await subscribe(params.id, pubHex);
      }
      rememberSubscribed();

      setBootstrap("polling");
      // First-attempt catch-up in case the publisher has already fanned
      // out. If it recovers at least one epoch's seed we can read;
      // otherwise the polling effect below keeps trying.
      const opened = await catchUpSeeds(pubHex);
      if (opened > 0) {
        setBootstrap("active");
      }
    } catch (err) {
      const token = anonTokenMessage(
        err,
        "Could not follow the channel. Check your connection and try again.",
      );
      if (apiError.is(err)) {
        setBootstrapError(channelErrorText(err));
      } else if (token !== null) {
        setBootstrapError(token);
      } else {
        setBootstrapError(String(err));
      }
      setBootstrap("error");
    }
  }

  /**
   * ADR-0031 catch-up: fetch the server's retained seed-envelope set (up to
   * N=4 most-recent epochs), open EACH under its own epoch into the
   * per-channel `epoch -> seed` map, and detect a re-sync. Returns the
   * number of epoch seeds now recovered for this channel (0 => nothing to
   * read yet, keep polling).
   *
   * Re-sync (offline past retention): if the map has no seed for the
   * server's CURRENT epoch after opening the retained set, re-register
   * (idempotent) so the publisher reseals the current seed on its next
   * fanout. Posts older than the retained window stay unreadable — an
   * availability cost, never a wrong-seed decrypt (invariant #4).
   */
  async function catchUpSeeds(pubHex: string): Promise<number> {
    const kp = channelsState.subscriberKeysByChannel().get(params.id);
    if (kp === undefined) return 0;

    const epochView = await fetchEpoch(params.id);
    channelsActions.setEpoch(params.id, epochView.epoch);

    // Retained set (plural) — the authoritative, epoch-tagged source. Each
    // entry is opened under ITS epoch; one bad envelope does not abort the
    // whole catch-up.
    const retained = await fetchEnvelopes(params.id, pubHex);
    for (const { epoch, envelope } of retained) {
      try {
        const seed = await openSubscriberSeedEnvelope(kp.secret, params.id, epoch, envelope);
        channelsActions.setSubscribedSeedForEpoch(params.id, epoch, seed);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(`seed envelope open failed for epoch ${epoch}`, err);
      }
    }

    const map = channelsState.subscribedSeedsByChannel().get(params.id);
    if ((map === undefined || !map.has(epochView.epoch)) && !registeredThisView) {
      // Missing the current epoch's seed — re-register so the next rotation
      // fanout reseals us. Idempotent; a failure here is non-fatal. Once per
      // view: the token is spent whether or not the request lands.
      registeredThisView = true;
      try {
        await subscribe(params.id, pubHex);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("re-subscribe (re-sync) failed", err);
      }
    }
    return map?.size ?? 0;
  }

  // First-load: drive the bootstrap.
  void bootstrap_();

  // Polling loop — runs while `bootstrap === "polling"`. 3-second cadence;
  // cancels itself once at least one epoch's seed is recovered.
  createEffect(() => {
    if (bootstrap() !== "polling") return;
    const kp = channelsState.subscriberKeysByChannel().get(params.id);
    if (kp === undefined) return;
    const pubHex = bytesToHex(kp.public);
    const handle = setInterval(() => {
      void (async (): Promise<void> => {
        try {
          const opened = await catchUpSeeds(pubHex);
          if (opened > 0) {
            clearInterval(handle);
            setBootstrap("active");
          }
        } catch (err) {
          // Transient network errors: keep polling. A permanent failure
          // (404 unknown-channel etc.) is surfaced by the registration step.
          // eslint-disable-next-line no-console
          console.warn("envelope poll error", err);
        }
      })();
    }, 3000);
    onCleanup(() => {
      clearInterval(handle);
    });
  });

  // Posts loader — fetch the page once we go active.
  createEffect(() => {
    if (bootstrap() !== "active") return;
    void (async (): Promise<void> => {
      try {
        const page = await fetchPosts(params.id, 0);
        setPosts(page.posts);
        channelsActions.setPosts(params.id, page.posts);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("subscribe post-load error", err);
      }
    })();
  });

  // Decode-by-epoch — re-runs whenever the post list OR the recovered seed
  // map changes, so a seed that arrives later (a re-sync reseal) upgrades a
  // "Not available" card to text without a reload. Each post is opened
  // under the seed for ITS epoch (ADR-0031); a post whose epoch has no seed
  // renders "Not available" — never force-decrypted with a wrong-epoch seed
  // (invariant #4).
  createEffect(() => {
    const ps = posts();
    const seedMap = channelsState.subscribedSeedsByChannel().get(params.id);
    if (ps.length === 0) {
      setDecodedByCursor(new Map());
      return;
    }
    void decodePage(params.id, ps, (epoch) => seedMap?.get(epoch)).then(setDecodedByCursor);
  });

  const meta = (): ReturnType<typeof channelMetaOf> => channelMetaOf(params.id);
  const statusLine = (): string => {
    switch (bootstrap()) {
      case "idle":
      case "registering":
        return "Subscribing...";
      case "polling":
        return "Waiting to be let in...";
      case "active": {
        // UX spec §5 — the description under the header when present.
        const m = meta();
        if (m.description.length > 0) return m.description;
        return m.handle.length > 0 ? m.handle : "Subscribed";
      }
      case "error":
        return "Not subscribed";
    }
  };

  return (
    <article class="lk-chat lk-channel">
      <div class="lk-chat-body">
        <ChannelHead
          id={params.id}
          name={meta().name}
          sub={statusLine()}
          onOpenInfo={(): void => {
              setInfoOpen(true);
            }}
        />
        {/* The reader's view of the same info the publisher's view opens. */}
        <ModalLayer
          open={infoOpen()}
          title={infoHeading("channel")}
          onClose={(): void => {
              setInfoOpen(false);
            }}
        >
          <ChannelInfoBody
            id={params.id}
            isPublisher={false}
            isSubscriber={channelsState.subscriberKeysByChannel().get(params.id) !== undefined}
            onManage={(): void => undefined}
            onUnsubscribe={(): void => {
              void confirmStopFollowing(params.id, meta().name).then((done) => {
                if (!done) return;
                setInfoOpen(false);
                navigate(paths.channels);
              });
            }}
          />
        </ModalLayer>
        <Switch>
          <Match when={bootstrap() === "polling"}>
            <div class="lk-alert info lk-channel-notice" role="status">
              Subscribed. Posts appear once the channel lets this device in; that usually takes a
              moment.
            </div>
          </Match>
          <Match when={bootstrap() === "error"}>
            <div class="lk-alert danger lk-channel-notice" role="alert">
              {bootstrapError() === null
                ? "Could not subscribe. Try again later."
                : `Could not subscribe. ${bootstrapError()}`}
            </div>
          </Match>
        </Switch>
        <PostList
          posts={bootstrap() === "active" ? posts() : []}
          decodedByCursor={decodedByCursor()}
          loading={bootstrap() !== "active" && bootstrap() !== "error"}
          emptyText={bootstrap() === "error" ? "" : "No posts yet."}
        />
      </div>
    </article>
  );
};

// ============================================================================
// Module exports
// ============================================================================

export { NewChannel, JoinCoPublisher, ChannelView, ChannelSubscribe };
