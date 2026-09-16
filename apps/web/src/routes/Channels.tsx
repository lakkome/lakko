// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Channels routes — Phase 4 G4-2 web.
 *
 * Three screens behind one file (small enough that
 * splitting hurts more than it helps):
 *
 *   ChannelsList    — `/channels` — shows the user's
 *                     locally-tracked channels.
 *   NewChannel      — `/channels/new` — create form
 *                     (generates a fresh 16-byte id
 *                     locally, calls
 *                     services/channels::create).
 *   ChannelView     — `/channels/:id` — server epoch +
 *                     posts list + publish form.
 *
 * Real ChannelPost composition + signature verify + AEAD
 * open are wired (G4-2-crypto): the publish form composes
 * a signed encrypted post via `composePost` (the `composer.ts`
 * shim over `lakko-ffi-wasm`) and the subscriber path opens
 * the HPKE seed envelope via `openSubscriberSeedEnvelope`.
 * The server stays opaque — it stores and hands back the
 * ciphertext bytes without inspecting them.
 */

import {
  createEffect,
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
import { useNavigate, useParams } from "@solidjs/router";

import {
  apiError,
  createChannel,
  deliverEnvelope,
  fetchEnvelopes,
  fetchEpoch,
  fetchPosts,
  listSubscribers,
  publishPost,
  subscribe,
  type ChannelPostView,
  channelErrorText,
} from "../channels/api";
import {
  bytesToHex,
  composePost,
  decodeAndDecryptPost,
  freshPublisherKeys,
  freshSubscriberKeys,
  openSubscriberSeedEnvelope,
  sealSubscriberSeedEnvelope,
  utf8Decode,
  utf8Encode,
  type DecodedPostResult,
  type WasmIdentity,
} from "../channels/composer";
import {
  approveCoPublisher,
  createCoPublisherLink,
  openPublisherGroup,
  pendingCoPublishers,
  pollCoPublisherJournal,
  pollForCoPublishWelcome,
  rejectCoPublisher,
  requestToCoPublish,
} from "../channels/copublisher";
import { LegacyChannelError, channelErrorMessage } from "../channels/legacy-v1";
// ADR-0045 UX §6 / §12 item 6 — one refusal table, shared with the chat
// members panel. The co-publisher queue is a different surface but the same
// core refusals, and a second vocabulary for them is the drift §12.6 flags.
import { membershipRefusalOf } from "../chat/membership-refusal";
import { membershipRefusalRetryMaySucceed } from "../../wasm/lakko_ffi_wasm";
import {
  runRotationOnce,
  startRotationCron,
  type RotationContext,
  type RotationCronHandle,
} from "../channels/rotation-cron";
import {
  loadRotationState,
  saveRotationState,
  type ChannelRotationState,
} from "../channels/rotation-state-store";
import {
  DiscoveryHttpError,
  registerChannelHandle,
} from "../channels/channel-discovery";
import { DsClient } from "../ds/client";
import { useAuth } from "../auth/AuthContext";
import { channelsActions, channelsState } from "../state/channels-state";
import { loadWasm, type Wasm } from "../wasm";
import { paths } from "./paths";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";

// ============================================================================
// Shared handle / policy types for the publisher-tools panels
// ============================================================================

/** The wasm `PublisherGroup` handle `openPublisherGroup` returns. Shared by
 *  the rotation cron AND the co-publisher panel so a `rotate()` on one is
 *  seen by the other — two handles would commit divergent epochs. */
type PublisherGroupHandle = Awaited<ReturnType<typeof openPublisherGroup>>;
/** wasm-bindgen rotation policy — `{ intervalMs: bigint, maxPosts: number }`. */
type ChannelRotationPolicy = ReturnType<Wasm["channelRotationPolicyDefault"]>;

// Styling is the `lk-` class vocabulary in `styles.css` (tokens, light and
// dark); this file carries no colour literal and no local palette
// (docs/ux/client-redesign.md section 4).

// ============================================================================
// Per-post decode result (ADR-0031 decrypt-by-epoch)
// ============================================================================

/** The three post-render outcomes:
 *  - `text`       — decrypted plaintext.
 *  - `unreadable` — a seed for the post's epoch was present but the
 *                   AEAD open failed (wrong seed / tampered / foreign signer).
 *  - `older-key`  — no seed exists for the post's epoch (older than the
 *                   retained window, or a gap). NOT force-decrypted with a
 *                   wrong-epoch seed (invariant #4). */
type PostDecode =
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "unreadable" }
  | { readonly kind: "older-key" };

/** Map the composer's low-level decode result onto a render outcome. */
function toPostDecode(res: DecodedPostResult): PostDecode {
  if (res.plaintext !== null) {
    return { kind: "text", text: utf8Decode(res.plaintext) };
  }
  return res.hadSeed ? { kind: "unreadable" } : { kind: "older-key" };
}

/** Muted placeholder line (unreadable / older-key). */
function placeholderBubble(text: string): JSX.Element {
  return <div class="lk-post-placeholder">{text}</div>;
}

/** Plaintext post body. */
function textBubble(text: string): JSX.Element {
  return <div class="lk-post-text">{text}</div>;
}

/** Render one post body from its decode outcome. `undefined` means the
 *  decode has not completed yet. An unreadable post says "Not available" with
 *  a one-line reason (docs/ux section 5.4), never why in protocol terms. */
function renderPostBody(decoded: PostDecode | undefined): JSX.Element {
  if (decoded === undefined) {
    return placeholderBubble("Loading...");
  }
  if (decoded.kind === "text") {
    return textBubble(decoded.text);
  }
  if (decoded.kind === "older-key") {
    return placeholderBubble("Not available. This post is older than what this device can read.");
  }
  return placeholderBubble("Not available. This post could not be opened on this device.");
}

/** One post card: the body and its time. */
function PostCard(props: { readonly post: ChannelPostView; readonly decoded: PostDecode | undefined }): JSX.Element {
  return (
    <li class="lk-post">
      {renderPostBody(props.decoded)}
      <div class="lk-post-time">{new Date(props.post.serverTsMs).toLocaleString()}</div>
    </li>
  );
}

// ============================================================================
// NewChannel — `/channels/new`
// ============================================================================

const NewChannel: Component = () => {
  const navigate = useNavigate();
  const auth = useAuth();
  const [name, setName] = createSignal("");
  const [description, setDescription] = createSignal("");
  // Optional `@handle` claimed via OPRF discovery (G4-5) so others
  // can subscribe by handle instead of the raw hex channel id.
  const [handle, setHandle] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  /** Generate a fresh 16-byte channel id locally — the
   *  server has no role in id allocation per ADR-0010
   *  (the id is a pseudonym chosen by the publisher).
   *  `crypto.getRandomValues` is the secure-context
   *  source web-side; the route is behind the
   *  RequireSession gate so secure context is
   *  guaranteed. */
  function freshChannelIdHex(): string {
    const buf = new Uint8Array(16);
    crypto.getRandomValues(buf);
    return Array.from(buf, (b) => b.toString(16).padStart(2, "0")).join("");
  }

  async function onCreate(): Promise<void> {
    if (busy()) return;
    if (name().trim().length === 0) {
      setError("Name cannot be empty.");
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
      // G4-5: claim the optional `@handle` FIRST (before the channel
      // exists on the channels service) so a first-come-first-served
      // collision (409) aborts cleanly without leaving a half-created
      // channel. An empty handle skips discovery entirely — the raw
      // hex channel id still works. The mapping is `@handle ->
      // channel_id`; the OPRF hash is all the server ever sees.
      const wantedHandle = handle().trim();
      if (wantedHandle.length > 0) {
        try {
          await registerChannelHandle(wantedHandle, id, secrets.sessionToken);
        } catch (err) {
          if (err instanceof DiscoveryHttpError && err.status === 409) {
            setError(
              `@${wantedHandle.replace(/^@/, "")} is already taken. Pick another, or leave it blank.`,
            );
          } else {
            setError(`Could not claim the handle: ${String(err)}`);
          }
          setBusy(false);
          return;
        }
      }

      await createChannel(id);
      // Mint local publisher keys — the content-key seed
      // comes from an MLS PublisherGroup exporter
      // (ADR-0010 Tier 1), the signing-key seed is a
      // fresh Ed25519 keypair seed. Identity is rebuilt
      // here from the canonical AccountIdentity (phrase)
      // seed on every call (ADR-0017 — was the OPAQUE
      // export key); a future sub-sprint
      // (G4-2-mls-persistence) keeps the PublisherGroup
      // alive across composer calls so it can be rotated
      // rather than rebuilt fresh.
      const wasm = await loadWasm();
      const identity = wasm.Identity.fromSeed(secrets.accountIdentitySeed);
      let keys;
      try {
        keys = await freshPublisherKeys(identity, id);
      } finally {
        identity.free();
      }
      channelsActions.addChannel({
        id,
        localName: name().trim(),
        localDescription: description().trim(),
        addedAtMs: Date.now(),
      });
      channelsActions.setKeys(id, keys);
      navigate(paths.channel(id));
    } catch (err) {
      if (err instanceof LegacyChannelError) {
        setError(err.message);
      } else if (apiError.is(err)) {
        setError(channelErrorText(err));
      } else {
        setError(`Could not reach Lakko: ${String(err)}`);
      }
      setBusy(false);
    }
  }

  // ADR-0028 OQ3 — joiner side: become a co-publisher of an existing channel
  // from a `/g/` co-publisher link the owner shared. Posts an external-join
  // proposal to the PublisherGroup journal, polls until the owner approves,
  // joins, mints local publisher keys, and opens the channel.
  const [coLink, setCoLink] = createSignal("");
  const [coBusy, setCoBusy] = createSignal(false);
  const [coError, setCoError] = createSignal<string | null>(null);
  const [coStatus, setCoStatus] = createSignal<string | null>(null);

  async function onJoinAsCoPublisher(): Promise<void> {
    const link = coLink().trim();
    if (link.length === 0) return;
    const secrets = auth.identitySecrets();
    if (secrets === null) {
      setCoError("Sign in again to join a channel.");
      return;
    }
    setCoBusy(true);
    setCoError(null);
    setCoStatus("Sending your request...");
    try {
      const wasm = await loadWasm();
      const identity = wasm.Identity.fromSeed(secrets.accountIdentitySeed);
      try {
        const ds = new DsClient();
        const { groupId, channelIdHex, joinerKey } = await requestToCoPublish(
          ds,
          identity,
          link,
        );
        setCoStatus("Waiting for the channel owner to approve...");
        let keys: Awaited<ReturnType<typeof pollForCoPublishWelcome>> = null;
        for (let i = 0; i < 60 && keys === null; ++i) {
          keys = await pollForCoPublishWelcome(ds, identity, groupId, joinerKey, channelIdHex);
          if (keys === null) await new Promise((r) => setTimeout(r, 3000));
        }
        if (keys === null) {
          setCoError("No approval yet. Try again once the owner has approved.");
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
      } finally {
        identity.free();
      }
    } catch (err) {
      setCoError(
        err instanceof LegacyChannelError
          ? err.message
          : `Could not join: ${String(err)}`,
      );
    } finally {
      setCoBusy(false);
      setCoStatus(null);
    }
  }

  return (
    <div class="lk-page-pane narrow lk-stack">
      <div class="lk-page-head">
        <h1>New channel</h1>
        <p>The name and description stay on this device; only encrypted posts reach the server.</p>
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
          <span class="lk-meta lk-block lk-mt-4">
            Others can find and subscribe to the channel by it.
          </span>
        </label>
        <Show when={error() !== null}>
          <div class="lk-alert danger" role="alert">
            {error()}
          </div>
        </Show>
        <div class="lk-actions lk-row-end">
          <button type="button" class="ghost" onClick={(): void => navigate(paths.channels)} disabled={busy()}>
            Cancel
          </button>
          <button type="submit" class="primary" disabled={busy()}>
            {busy() ? "Creating..." : "Create"}
          </button>
        </div>
      </form>

      <section class="lk-card">
        <h2>Join as a co-publisher</h2>
        <p class="lk-card-desc">
          Paste a co-publisher link a channel owner shared. Once they approve, this device can
          post to the channel.
        </p>
        <input
          type="text"
          placeholder="https://lakko.me/g/..."
          aria-label="Co-publisher link"
          value={coLink()}
          onInput={(e): void => {
            setCoLink(e.currentTarget.value);
          }}
        />
        <Show when={coStatus() !== null}>
          <div class="lk-meta" role="status">
            {coStatus()}
          </div>
        </Show>
        <Show when={coError() !== null}>
          <div class="lk-alert danger" role="alert">
            {coError()}
          </div>
        </Show>
        <div class="lk-actions">
          <button
            type="button"
            disabled={coBusy() || coLink().trim().length === 0}
            onClick={(): void => void onJoinAsCoPublisher()}
          >
            {coBusy() ? "Requesting..." : "Request to join"}
          </button>
        </div>
      </section>
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

  // Resource that fetches the post list + epoch when the
  // route first opens. Both come from the same server,
  // pulled in parallel.
  const [data, { refetch }] = createResource(
    () => params.id,
    async (
      id: string,
    ): Promise<{
      readonly posts: ReadonlyArray<ChannelPostView>;
      readonly epoch: number;
    }> => {
      const [postsPage, epochView] = await Promise.all([
        fetchPosts(id, 0),
        fetchEpoch(id),
      ]);
      return { posts: postsPage.posts, epoch: epochView.epoch };
    },
  );

  // Sync the cache as soon as a fetch completes so the
  // list route's preview line stays fresh on back-nav.
  createEffect(() => {
    const d = data();
    if (d !== undefined) {
      channelsActions.setPosts(params.id, d.posts);
      channelsActions.setEpoch(params.id, d.epoch);
    }
  });

  const channelMeta = (): { localName: string; localDescription: string } => {
    const h = channelsState.channels().find((c) => c.id === params.id);
    return {
      localName: h?.localName ?? "Channel",
      localDescription: h?.localDescription ?? "",
    };
  };

  async function onPublish(): Promise<void> {
    if (publishBusy()) return;
    const text = composeText().trim();
    if (text.length === 0) {
      setPublishError("Write something first.");
      return;
    }
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
      const wire = await composePost(
        keys,
        params.id,
        epoch,
        counter,
        utf8Encode(text),
      );
      await publishPost(params.id, wire);
      setComposeText("");
      await refetch();
    } catch (err) {
      if (apiError.is(err)) {
        setPublishError(channelErrorText(err));
      } else {
        setPublishError(`Could not publish: ${String(err)}`);
      }
    } finally {
      setPublishBusy(false);
    }
  }

  /** Decoded plaintext per-post, keyed by cursor. `null`
   *  means the decode failed (wrong seed, wrong channel,
   *  tampered ciphertext) — UI surfaces "[unreadable]". */
  const [decodedByCursor, setDecodedByCursor] = createSignal<
    ReadonlyMap<number, PostDecode>
  >(new Map());

  // Decode every post in the latest fetch. Runs whenever the
  // post list or the channel changes. The publisher holds a
  // single content-key seed (its PublisherGroup exporter
  // output) which it applies to every post it can read; a post
  // it cannot open surfaces as [unreadable]. (The subscriber
  // path below uses the ADR-0031 epoch -> seed map instead.)
  createEffect(() => {
    const d = data();
    const keys = channelsState.keysByChannel().get(params.id);
    if (d === undefined || keys === undefined) {
      setDecodedByCursor(new Map());
      return;
    }
    const seed = keys.seed;
    void Promise.all(
      d.posts.map(async (p) => {
        const res = await decodeAndDecryptPost(
          params.id,
          p.payloadB64,
          () => seed,
        );
        return [p.cursor, toPostDecode(res)] as const;
      }),
    ).then((entries) => {
      setDecodedByCursor(new Map(entries));
    });
  });

  return (
    <div class="lk-page-pane wide lk-stack">
      <button
        type="button"
        class="ghost small lk-narrow-only lk-self-start"
        onClick={(): void => navigate(paths.channels)}
      >
        <Icon name="back" size={16} />
        Channels
      </button>
      <div class="lk-page-head row">
        <Avatar label={channelMeta().localName || "?"} id={params.id} size="lg" />
        <div>
          <h1 class="lk-ellipsis">{channelMeta().localName}</h1>
          <Show when={channelMeta().localDescription !== ""}>
            <p>{channelMeta().localDescription}</p>
          </Show>
        </div>
      </div>

      <Show
        when={channelsState.keysByChannel().get(params.id) !== undefined}
        fallback={
          <section class="lk-card">
            <h2>Read-only on this device</h2>
            <p class="lk-card-desc">
              This device cannot post to the channel. To read its posts,{" "}
              <a href={paths.channelSubscribe(params.id)}>subscribe</a>.
            </p>
          </section>
        }
      >
        <form
          onSubmit={(e): void => {
            e.preventDefault();
            void onPublish();
          }}
          class="lk-card"
        >
          <textarea
            rows={3}
            placeholder="Write a post..."
            aria-label="New post"
            value={composeText()}
            onInput={(e): void => {
              setComposeText(e.currentTarget.value);
            }}
            disabled={publishBusy()}
          />
          <div class="lk-actions lk-row-end">
            <button
              type="submit"
              class="primary"
              disabled={publishBusy() || composeText().trim().length === 0}
            >
              {publishBusy() ? "Publishing..." : "Publish"}
            </button>
          </div>
          <Show when={publishError() !== null}>
            <div class="lk-alert danger" role="alert">
              {publishError()}
            </div>
          </Show>
        </form>

        <SubscribersPanel channelIdHex={params.id} />
        <PublisherToolsPanel channelIdHex={params.id} />
      </Show>

      <h2 class="lk-m0">Posts</h2>
      <Show
        when={data() !== undefined && data()!.posts.length > 0}
        fallback={
          <p class="lk-muted">
            <Show when={data.loading} fallback={"No posts yet."}>
              Loading...
            </Show>
          </p>
        }
      >
        <ul class="lk-list">
          <For each={data()!.posts}>
            {(p): JSX.Element => <PostCard post={p} decoded={decodedByCursor().get(p.cursor)} />}
          </For>
        </ul>
      </Show>
    </div>
  );
};

// ============================================================================
// PublisherToolsPanel — opens the ONE shared PublisherGroup handle
//   (ADR-0010 / ADR-0028 OQ3) and threads it into both the rotation cron
//   and the co-publisher panel.
// ============================================================================

/**
 * Opens the channel's persistent `PublisherGroup` **once** and shares the
 * single handle (plus the `Identity`) with `RotationPanel` and
 * `CoPublishersPanel`.
 *
 * **Why one handle (load-bearing).** `PublisherGroup.rotate()` +
 * `approveExternalJoin()` both advance the MLS epoch **durably** (persisted
 * in the OpenMLS provider). Two independent `openPublisherGroup` handles held
 * in the same screen would each cache a stale in-memory epoch — a rotation on
 * one would leave the other believing it is still at epoch N, and the next
 * commit on that stale handle would fork the group (both commit N+1 with
 * divergent content). Sharing one handle (the wasm object is internally safe
 * to share) keeps every commit path on the same epoch.
 *
 * **Fail-closed (invariant #4).** The panels only render once the handle is
 * open; a failed open surfaces the error and renders nothing — the cron never
 * starts and no "Rotate now" / "Create link" affordance appears, so there is
 * never a plaintext or bad-key path.
 */
const PublisherToolsPanel: Component<{ readonly channelIdHex: string }> = (
  props,
) => {
  const auth = useAuth();
  const [identity, setIdentity] = createSignal<WasmIdentity | null>(null);
  const [group, setGroup] = createSignal<PublisherGroupHandle | null>(null);
  const [openError, setOpenError] = createSignal<string | null>(null);

  onMount(() => {
    const secrets = auth.identitySecrets();
    if (secrets === null) {
      setOpenError("Sign in again to manage this channel.");
      return;
    }
    void (async (): Promise<void> => {
      try {
        const wasm = await loadWasm();
        const id = wasm.Identity.fromSeed(secrets.accountIdentitySeed);
        const pg = await openPublisherGroup(id, props.channelIdHex);
        setIdentity(id);
        setGroup(pg);
      } catch (err) {
        setOpenError(
          channelErrorMessage(
            err,
            (e) => `Could not open the channel's settings: ${String(e)}`,
          ),
        );
      }
    })();
  });

  // Own the shared handles' lifetime. The child panels stop their timers on
  // their own cleanup (which runs before this parent cleanup), so no
  // in-flight callback touches a freed handle on unmount.
  onCleanup(() => {
    try {
      group()?.free();
    } catch {
      /* ignore */
    }
    try {
      identity()?.free();
    } catch {
      /* ignore */
    }
  });

  return (
    <Show
      when={group() !== null && identity() !== null}
      fallback={
        <Show when={openError() !== null}>
          <div class="lk-alert info" role="status">
            {openError()}
          </div>
        </Show>
      }
    >
      <RotationPanel
        channelIdHex={props.channelIdHex}
        publisherGroup={group()!}
        identity={identity()!}
      />
      <CoPublishersPanel
        channelIdHex={props.channelIdHex}
        publisherGroup={group()!}
        identity={identity()!}
      />
    </Show>
  );
};

// ============================================================================
// RotationPanel — scheduled content-key rotation (ADR-0010 / C-54)
//   Embedded inside ChannelView when the user holds publisher keys.
//   Drives the `RotationCron` heartbeat while visible + a manual
//   "Rotate now" button. Uses the SHARED PublisherGroup handle.
// ============================================================================

/**
 * The *scheduled* half of content-key rotation (the membership-change half
 * advances automatically on every MLS epoch bump). The pure
 * `channelShouldRotate` policy (default 24 h / 1000 posts) decides *when*;
 * `runRotationOnce` / `startRotationCron` do *how* — advance the shared
 * PublisherGroup epoch, derive the new seed, and reseal it to every
 * subscriber via the atomic `POST /v1/channels/:id/rotate` fanout.
 *
 * **Genesis-seed, not {0,0}.** `channelShouldRotate(default, {0,0}, now)`
 * fires immediately against real wall-clock (`lastRotationMs = 0` reads as
 * "last rotated at the epoch, i.e. forever ago"). Seeding the heartbeat with
 * a `{0,0}` state would therefore rotate + reseal every subscriber the instant
 * the screen opens. So the first open seeds `channelRotationStateAfterRotation(now)`
 * (i.e. "just rotated"), landing the first scheduled rotation ~one interval
 * later. The **"Rotate now"** button, by contrast, deliberately forces a
 * rotation by feeding a fresh `channelRotationStateNew()` ({0,0}) through
 * `runRotationOnce`, then adopts the returned post-rotation state as the new
 * heartbeat baseline (via the context's `saveState`, which both paths share).
 *
 * **Fail-closed (invariant #4).** A rotation that fails part-way never exposes
 * plaintext or a bad key: the reseal batch is atomic server-side, a single
 * seal failure aborts the whole batch, and a failed fanout only leaves
 * subscribers unable to decrypt the *next* post until the next successful
 * reseal — an availability effect, never a downgrade.
 */
const RotationPanel: Component<{
  readonly channelIdHex: string;
  readonly publisherGroup: PublisherGroupHandle;
  readonly identity: WasmIdentity;
}> = (props) => {
  const [status, setStatus] = createSignal("Loading...");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  let cron: RotationCronHandle | null = null;
  let policy: ChannelRotationPolicy | null = null;

  /** Build a fresh rotation context bound to the SHARED handle. Both the
   *  heartbeat and the manual button call this so their `saveState` writes
   *  land in the same localStorage key. */
  function context(): RotationContext {
    return {
      publisherGroup: props.publisherGroup,
      identity: props.identity,
      channelIdHex: props.channelIdHex,
      saveState: (next: ChannelRotationState): void => {
        saveRotationState(props.channelIdHex, next);
      },
      onRotated: (): void => {
        refreshStatus();
      },
    };
  }

  /** Load the persisted state, or genesis-seed `after_rotation(now)` on first
   *  open so the heartbeat does not fire an immediate rotation. */
  async function loadOrSeedState(): Promise<ChannelRotationState> {
    const existing = loadRotationState(props.channelIdHex);
    if (existing !== null) return existing;
    const wasm = await loadWasm();
    const seeded = wasm.channelRotationStateAfterRotation(BigInt(Date.now()));
    saveRotationState(props.channelIdHex, seeded);
    return seeded;
  }

  function refreshStatus(): void {
    const state = loadRotationState(props.channelIdHex);
    if (state === null || state.lastRotationMs <= 0n) {
      setStatus("Not refreshed yet.");
      return;
    }
    setStatus(`Last refreshed ${relativeAgo(state.lastRotationMs)}.`);
  }

  /** Force a rotation regardless of the schedule (e.g. after suspecting a
   *  subscriber compromise). Feeds a fresh `{0,0}` state so the policy gate
   *  always fires; `runRotationOnce` stamps + persists a fresh
   *  post-rotation state via the context's `saveState`. */
  async function onRotateNow(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      const wasm = await loadWasm();
      const forcePolicy = policy ?? wasm.channelRotationPolicyDefault();
      await runRotationOnce(
        context(),
        forcePolicy,
        wasm.channelRotationStateNew(),
      );
      refreshStatus();
    } catch (err) {
      setError(`Could not refresh the key: ${String(err)}`);
    } finally {
      setBusy(false);
    }
  }

  onMount(() => {
    void (async (): Promise<void> => {
      const wasm = await loadWasm();
      const defaultPolicy = wasm.channelRotationPolicyDefault();
      policy = defaultPolicy;
      await loadOrSeedState();
      refreshStatus();
      cron = startRotationCron({
        context: context(),
        policy: defaultPolicy,
        loadState: (): Promise<ChannelRotationState> => loadOrSeedState(),
        onError: (err: unknown): void => {
          setError(`Could not check the key: ${String(err)}`);
        },
      });
    })();
  });

  onCleanup(() => {
    cron?.stop();
    cron = null;
  });

  return (
    <section class="lk-card">
      <h2>Encryption key</h2>
      <p class="lk-card-desc">
        Lakko refreshes the channel's key on its own. Refresh it now if you think a
        subscriber's device was compromised.
      </p>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="key" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-sub" role="status">
            {status()}
          </span>
        </div>
        <button
          type="button"
          class="small lk-card-row-action"
          disabled={busy()}
          onClick={(): void => {
            void onRotateNow();
          }}
        >
          {busy() ? "Refreshing..." : "Refresh now"}
        </button>
      </div>
      <Show when={error() !== null}>
        <div class="lk-alert danger" role="alert">
          {error()}
        </div>
      </Show>
    </section>
  );
};

/** Human-readable "N ago" for a bigint Unix-ms timestamp. */
function relativeAgo(sinceMs: bigint): string {
  const nowMs = BigInt(Date.now());
  const deltaS = nowMs >= sinceMs ? Number((nowMs - sinceMs) / 1000n) : 0;
  if (deltaS < 60) return "just now";
  if (deltaS < 3600) return `${Math.floor(deltaS / 60)}m ago`;
  if (deltaS < 86_400) return `${Math.floor(deltaS / 3600)}h ago`;
  return `${Math.floor(deltaS / 86_400)}d ago`;
}

// ============================================================================
// CoPublishersPanel — owner-side co-publisher management (ADR-0028 OQ3)
// (lives inside ChannelView when the user holds publisher keys)
// ============================================================================

/** Poll cadence for the PublisherGroup DS journal — same 5 s trade-off as the
 *  subscriber fan-out. */
const COPUBLISHER_POLL_INTERVAL_MS = 5_000;

const CoPublishersPanel: Component<{
  readonly channelIdHex: string;
  readonly publisherGroup: PublisherGroupHandle;
  readonly identity: WasmIdentity;
}> = (props) => {
  const [link, setLink] = createSignal<string | null>(null);
  const [pending, setPending] = createSignal<Uint8Array[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  // Shared handles come from `PublisherToolsPanel` (props); this panel does
  // NOT open or free them — a second handle would fork the MLS epoch. `ds` is
  // a stateless HTTP client, so a per-panel instance is fine.
  const ds = new DsClient();
  let stopped = false;

  function hexOf(b: Uint8Array): string {
    let s = "";
    for (const x of b) s += x.toString(16).padStart(2, "0");
    return s;
  }

  async function refreshPending(): Promise<void> {
    try {
      await pollCoPublisherJournal(ds, props.identity, props.publisherGroup);
      setPending(pendingCoPublishers(props.publisherGroup));
    } catch (err) {
      setError(`Could not check for requests: ${String(err)}`);
    }
  }

  async function onCreateLink(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      setLink(
        await createCoPublisherLink(
          props.identity,
          props.publisherGroup,
          window.location.origin,
        ),
      );
    } catch (err) {
      setError(`Could not create the link: ${String(err)}`);
    } finally {
      setBusy(false);
    }
  }

  async function onApprove(ref: Uint8Array): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      await approveCoPublisher(
        ds,
        props.identity,
        props.publisherGroup,
        ref,
        props.channelIdHex,
      );
      setPending(pendingCoPublishers(props.publisherGroup));
    } catch (err) {
      // ADR-0045 UX §6 — the co-publisher approve reaches the SAME core
      // refusals as the chat one (`noSuchJoinRequest` when another owner got
      // there first, `joinerAlreadyAMember` when the joiner already landed),
      // so it reads from the same table rather than dumping the core's prose.
      setError(coPublisherRefusal(err) ?? `Could not approve: ${String(err)}`);
    } finally {
      setBusy(false);
    }
  }

  /** The §6 sentence for a co-publisher approve / reject refusal, or `null`
   *  when the failure is not a membership refusal (a DS write, a reseal) and
   *  its own text should stand. */
  function coPublisherRefusal(err: unknown): string | null {
    try {
      return membershipRefusalOf(err, membershipRefusalRetryMaySucceed)?.message ?? null;
    } catch {
      return null;
    }
  }

  function onReject(ref: Uint8Array): void {
    setError(null);
    try {
      rejectCoPublisher(props.publisherGroup, ref);
      setPending(pendingCoPublishers(props.publisherGroup));
    } catch (err) {
      setError(coPublisherRefusal(err) ?? `Could not reject: ${String(err)}`);
    }
  }

  // First poll + periodic loop. Cancelled on cleanup.
  void refreshPending();
  const handle = setInterval(() => {
    if (!stopped) void refreshPending();
  }, COPUBLISHER_POLL_INTERVAL_MS);

  onCleanup(() => {
    stopped = true;
    clearInterval(handle);
  });

  const [copied, setCopied] = createSignal(false);
  async function copyLink(): Promise<void> {
    const l = link();
    if (l === null) return;
    try {
      await navigator.clipboard.writeText(l);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      /* the button is the only feedback surface */
    }
  }
  const canShare = (): boolean =>
    typeof navigator !== "undefined" && typeof navigator.share === "function";

  return (
    <section class="lk-card">
      <h2>Co-publishers</h2>
      <p class="lk-card-desc">
        People who can post to this channel. Share a link; approve them when they ask.
      </p>
      <Show
        when={link()}
        fallback={
          <div class="lk-actions">
            <button type="button" disabled={busy()} onClick={() => void onCreateLink()}>
              <Icon name="link" size={18} />
              Create co-publisher link
            </button>
          </div>
        }
      >
        {(l) => (
          <div class="lk-link-row" role="group" aria-label="Co-publisher link">
            <span class="lk-link-text" title={l()}>
              {l().replace(/^https?:\/\//, "")}
            </span>
            <button
              type="button"
              class="lk-icon-button"
              title={copied() ? "Copied" : "Copy link"}
              aria-label={copied() ? "Copied" : "Copy link"}
              onClick={() => void copyLink()}
            >
              <Icon name={copied() ? "check" : "copy"} size={16} />
            </button>
            <Show when={canShare()}>
              <button
                type="button"
                class="lk-icon-button"
                title="Share"
                aria-label="Share"
                onClick={() => void navigator.share({ url: l() }).catch(() => undefined)}
              >
                <Icon name="share" size={16} />
              </button>
            </Show>
          </div>
        )}
      </Show>
      <Show when={pending().length > 0}>
        <div class="lk-section-label lk-flush">
          Requests ({pending().length})
        </div>
        <ul class="lk-list bordered">
          <For each={pending()}>
            {(ref, i) => (
              <li>
                <Avatar label={`Request ${i() + 1}`} id={hexOf(ref)} size="sm" />
                <span class="lk-fill">Someone asked to co-publish</span>
                <span class="lk-actions lk-nowrap">
                  <button
                    type="button"
                    class="primary small"
                    disabled={busy()}
                    onClick={() => void onApprove(ref)}
                  >
                    Approve
                  </button>
                  <button type="button" class="small" disabled={busy()} onClick={() => onReject(ref)}>
                    Reject
                  </button>
                </span>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={error() !== null}>
        <div class="lk-alert danger" role="alert">
          {error()}
        </div>
      </Show>
    </section>
  );
};

// ============================================================================
// SubscribersPanel — publisher-side fan-out widget
// (lives inside ChannelView when the user holds publisher keys)
// ============================================================================

/** Auto-fanout poll cadence — every 5 s the publisher
 *  panel re-fetches the subscriber list, diffs against
 *  the already-delivered set, and auto-seals an envelope
 *  for each new pubkey. 5 s is the same trade-off the
 *  subscribe-screen poll uses on the other side. */
const SUBSCRIBERS_POLL_INTERVAL_MS = 5_000;

const SubscribersPanel: Component<{ readonly channelIdHex: string }> = (
  props,
) => {
  const [subs, setSubs] = createSignal<ReadonlyArray<string> | null>(null);
  const [busyPk, setBusyPk] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  /** Per-subscriber delivery status. `"auto"` means the
   *  polling loop sealed + delivered without a manual
   *  button click; `"delivered"` means the publisher
   *  clicked Deliver seed; `"failed"` covers both
   *  manual and auto-trigger failures. */
  const [statusByPk, setStatusByPk] = createSignal<
    ReadonlyMap<string, "delivered" | "auto" | "failed">
  >(new Map());

  async function refresh(): Promise<void> {
    try {
      const list = await listSubscribers(props.channelIdHex);
      setSubs(list.subscribers);
    } catch (err) {
      setError(`Could not load subscribers: ${String(err)}`);
    }
  }

  // First-load fetch.
  void refresh();

  async function deliverTo(
    pubkeyHex: string,
    trigger: "manual" | "auto" = "manual",
  ): Promise<void> {
    // Manual delivery is one-at-a-time (single busy
    // spinner across the panel). The auto path ignores
    // busyPk so a slow manual click on one subscriber
    // does not stall the polling loop's progress on
    // others — auto-triggered deliveries are tracked
    // only through statusByPk.
    if (trigger === "manual" && busyPk() !== null) return;
    const keys = channelsState.keysByChannel().get(props.channelIdHex);
    if (keys === undefined) {
      setError("This device cannot post to this channel.");
      return;
    }
    if (trigger === "manual") {
      setBusyPk(pubkeyHex);
      setError(null);
    }
    try {
      const subPubBytes = new Uint8Array(pubkeyHex.length / 2);
      for (let i = 0; i < subPubBytes.length; i += 1) {
        subPubBytes[i] = Number.parseInt(pubkeyHex.slice(i * 2, i * 2 + 2), 16);
      }
      const epoch = channelsState.epochByChannel().get(props.channelIdHex) ?? 0;
      const wire = await sealSubscriberSeedEnvelope(
        subPubBytes,
        props.channelIdHex,
        epoch,
        keys.seed,
      );
      // ADR-0031 — file the envelope under the concrete epoch it
      // was sealed for so the server retains it per-epoch (and
      // prunes to the last N). Absent → the server would default
      // to its current epoch, which matches here anyway.
      await deliverEnvelope(
        props.channelIdHex,
        pubkeyHex,
        wire,
        undefined,
        epoch,
      );
      setStatusByPk((m) =>
        new Map(m).set(pubkeyHex, trigger === "auto" ? "auto" : "delivered"),
      );
    } catch (err) {
      if (trigger === "manual") {
        if (apiError.is(err)) {
          setError(`Could not send the key: ${channelErrorText(err)}`);
        } else {
          setError(`Could not send the key: ${String(err)}`);
        }
      }
      setStatusByPk((m) => new Map(m).set(pubkeyHex, "failed"));
    } finally {
      if (trigger === "manual") {
        setBusyPk(null);
      }
    }
  }

  // Auto-fanout polling loop. Every SUBSCRIBERS_POLL_INTERVAL_MS:
  //   1. refresh the list
  //   2. for every pubkey we have not yet successfully
  //      delivered to (status is absent or "failed"),
  //      auto-seal + deliver.
  // Cancelled on component cleanup via onCleanup, same
  // pattern as the subscribe-screen poll. The opacity
  // contract is unchanged — every envelope still computed
  // client-side under the publisher's content-key seed.
  createEffect(() => {
    const handle = setInterval(() => {
      void (async (): Promise<void> => {
        await refresh();
        const list = subs();
        if (list === null) return;
        const status = statusByPk();
        // Sequence the auto-deliveries so we don't open
        // N parallel sealSubscriberSeedEnvelope WASM
        // calls on a fresh subscriber storm — the WASM
        // module is single-threaded.
        for (const pk of list) {
          const st = status.get(pk);
          if (st === undefined || st === "failed") {
            await deliverTo(pk, "auto");
          }
        }
      })();
    }, SUBSCRIBERS_POLL_INTERVAL_MS);
    onCleanup(() => {
      clearInterval(handle);
    });
  });

  return (
    <section class="lk-card">
      <h2>
        Subscribers{" "}
        <span class="lk-meta lk-regular">
          ({subs()?.length ?? "..."})
        </span>
      </h2>
      <p class="lk-card-desc">
        Each new subscriber receives the channel's key from this device automatically.
      </p>
      <Show when={error() !== null}>
        <div class="lk-alert danger" role="alert">
          {error()}
        </div>
      </Show>
      <Show
        when={subs() !== null && subs()!.length > 0}
        fallback={
          <p class="lk-muted lk-m0">
            <Show when={subs() === null} fallback="No subscribers yet. Share the channel's handle so people can subscribe.">
              Loading...
            </Show>
          </p>
        }
      >
        <ul class="lk-list bordered" aria-label="Subscribers">
          <For each={subs()!}>
            {(pk, i): JSX.Element => {
              const status = (): "delivered" | "auto" | "failed" | undefined =>
                statusByPk().get(pk);
              return (
                <li>
                  <Avatar label={`Subscriber ${i() + 1}`} id={pk} size="sm" />
                  <span class="lk-fill">Subscriber {i() + 1}</span>
                  <Switch fallback={<span class="lk-status-tag">Sending key...</span>}>
                    <Match when={status() === "delivered" || status() === "auto"}>
                      <span class="lk-status-tag ok">Has the key</span>
                    </Match>
                    <Match when={status() === "failed"}>
                      <span class="lk-status-tag bad">Could not send the key</span>
                      <button
                        type="button"
                        class="small"
                        disabled={busyPk() === pk}
                        onClick={(): void => {
                          void deliverTo(pk);
                        }}
                      >
                        {busyPk() === pk ? "Sending..." : "Retry"}
                      </button>
                    </Match>
                  </Switch>
                </li>
              );
            }}
          </For>
        </ul>
      </Show>
    </section>
  );
};

// ============================================================================
// ChannelSubscribe — `/channels/:id/subscribe`
//   Subscriber-side view: generate (or restore) an X25519
//   keypair, register it on the channel, poll for a seed
//   envelope, open it, then render decoded posts using the
//   recovered seed.
// ============================================================================

const ChannelSubscribe: Component = () => {
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [bootstrap, setBootstrap] = createSignal<
    "idle" | "registering" | "polling" | "active" | "error"
  >("idle");
  const [bootstrapError, setBootstrapError] = createSignal<string | null>(null);
  const [posts, setPosts] = createSignal<ReadonlyArray<ChannelPostView>>([]);
  const [decodedByCursor, setDecodedByCursor] = createSignal<
    ReadonlyMap<number, PostDecode>
  >(new Map());

  // Persisted keypair lookup. If none exists on first
  // mount, we generate one and store it.
  async function ensureKeypair(): Promise<{
    readonly secret: Uint8Array;
    readonly public: Uint8Array;
  }> {
    const existing = channelsState.subscriberKeysByChannel().get(params.id);
    if (existing !== undefined) {
      return existing;
    }
    const fresh = await freshSubscriberKeys();
    channelsActions.setSubscriberKeys(params.id, fresh);
    return fresh;
  }

  /** Drive the registration + catch-up dance.
   *  Idempotent — the server treats a re-register of the same
   *  pubkey as a no-op, and re-running the catch-up just
   *  re-opens whatever the retained set currently is. */
  async function bootstrap_(): Promise<void> {
    setBootstrap("registering");
    setBootstrapError(null);
    try {
      const kp = await ensureKeypair();
      const pubHex = bytesToHex(kp.public);
      await subscribe(params.id, pubHex);

      setBootstrap("polling");
      // First-attempt catch-up in case the publisher has
      // already fanned out. If it recovers at least one epoch's
      // seed we can read; otherwise the polling effect below
      // keeps trying.
      const opened = await catchUpSeeds(pubHex);
      if (opened > 0) {
        setBootstrap("active");
      }
    } catch (err) {
      if (apiError.is(err)) {
        setBootstrapError(channelErrorText(err));
      } else {
        setBootstrapError(String(err));
      }
      setBootstrap("error");
    }
  }

  /**
   * ADR-0031 catch-up: fetch the server's retained seed-envelope
   * set (up to N=4 most-recent epochs), open EACH under its own
   * epoch into the per-channel `epoch -> seed` map, and detect a
   * re-sync. Returns the number of epoch seeds now recovered for
   * this channel (0 => nothing to read yet, keep polling).
   *
   * Re-sync (offline past retention): if the map has no seed for
   * the server's CURRENT epoch after opening the retained set, we
   * re-register (idempotent) so the publisher reseals the current
   * seed on its next fanout. Posts older than the retained window
   * stay unreadable (older-key placeholder) — an availability
   * cost, never a wrong-seed decrypt (invariant #4).
   */
  async function catchUpSeeds(pubHex: string): Promise<number> {
    const kp = channelsState.subscriberKeysByChannel().get(params.id);
    if (kp === undefined) return 0;

    // Track the server's current epoch (for re-sync detection and
    // so the epoch shown in the UI stays live).
    const epochView = await fetchEpoch(params.id);
    channelsActions.setEpoch(params.id, epochView.epoch);

    // Retained set (plural) — the authoritative, epoch-tagged
    // source. Each entry is opened under ITS epoch; one bad
    // envelope does not abort the whole catch-up.
    const retained = await fetchEnvelopes(params.id, pubHex);
    for (const { epoch, envelope } of retained) {
      try {
        const seed = await openSubscriberSeedEnvelope(
          kp.secret,
          params.id,
          epoch,
          envelope,
        );
        channelsActions.setSubscribedSeedForEpoch(params.id, epoch, seed);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(`seed envelope open failed for epoch ${epoch}`, err);
      }
    }

    const map = channelsState.subscribedSeedsByChannel().get(params.id);
    if (map === undefined || !map.has(epochView.epoch)) {
      // Missing the current epoch's seed — re-register so the
      // next rotation fanout reseals us. Idempotent; a failure
      // here is non-fatal (we still read whatever we recovered).
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

  // Polling loop — runs while `bootstrap === "polling"`.
  // 3-second cadence; cancels itself once at least one epoch's
  // seed is recovered. A future sprint adds server-push (long-
  // poll or SSE) so this becomes event-driven.
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
          // Transient network errors: keep polling. A real
          // permanent failure (404 unknown-channel etc.) is
          // surfaced by the registration step.
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
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("subscribe post-load error", err);
      }
    })();
  });

  // Decode-by-epoch — re-runs whenever the post list OR the
  // recovered seed map changes, so a seed that arrives later
  // (e.g. a re-sync reseal) upgrades an older-key placeholder to
  // plaintext without a reload. Each post is opened under the
  // seed for ITS epoch (ADR-0031); a post whose epoch has no seed
  // renders as an unreadable/older-key placeholder — never
  // force-decrypted with a wrong-epoch seed (invariant #4).
  createEffect(() => {
    const ps = posts();
    const seedMap = channelsState.subscribedSeedsByChannel().get(params.id);
    if (ps.length === 0) {
      setDecodedByCursor(new Map());
      return;
    }
    void Promise.all(
      ps.map(async (p) => {
        const res = await decodeAndDecryptPost(
          params.id,
          p.payloadB64,
          (epoch) => seedMap?.get(epoch),
        );
        return [p.cursor, toPostDecode(res)] as const;
      }),
    ).then((entries) => {
      setDecodedByCursor(new Map(entries));
    });
  });

  const channelName = (): string =>
    channelsState.channels().find((c) => c.id === params.id)?.localName ?? "Channel";

  return (
    <div class="lk-page-pane wide lk-stack">
      <button
        type="button"
        class="ghost small lk-narrow-only lk-self-start"
        onClick={(): void => navigate(paths.channels)}
      >
        <Icon name="back" size={16} />
        Channels
      </button>
      <div class="lk-page-head row">
        <Avatar label={channelName()} id={params.id} size="lg" />
        <div>
          <h1 class="lk-ellipsis">{channelName()}</h1>
          <p>Subscribed on this device</p>
        </div>
      </div>

      <Switch>
        <Match when={bootstrap() === "idle" || bootstrap() === "registering"}>
          <div class="lk-alert info" role="status">
            Subscribing...
          </div>
        </Match>
        <Match when={bootstrap() === "polling"}>
          <div class="lk-alert info" role="status">
            Subscribed. Posts appear once the publisher lets this device in; that usually
            takes a moment.
          </div>
        </Match>
        <Match when={bootstrap() === "error"}>
          <div class="lk-alert danger" role="alert">
            {bootstrapError() ?? "Could not subscribe. Try again later."}
          </div>
        </Match>
      </Switch>

      <h2 class="lk-m0">Posts</h2>
      <Show
        when={bootstrap() === "active" && posts().length > 0}
        fallback={
          <p class="lk-muted">
            <Show when={bootstrap() === "active"} fallback={bootstrap() === "error" ? "" : "Loading..."}>
              No posts yet.
            </Show>
          </p>
        }
      >
        <ul class="lk-list">
          <For each={posts()}>
            {(p): JSX.Element => <PostCard post={p} decoded={decodedByCursor().get(p.cursor)} />}
          </For>
        </ul>
      </Show>
    </div>
  );
};

// ============================================================================
// Module exports
// ============================================================================

export { NewChannel, ChannelView, ChannelSubscribe };
