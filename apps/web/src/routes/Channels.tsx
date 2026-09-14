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
  onCleanup,
  onMount,
  Show,
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
  resolveChannelIdOrHandle,
} from "../channels/channel-discovery";
import { DsClient } from "../ds/client";
import { useAuth } from "../auth/AuthContext";
import { channelsActions, channelsState } from "../state/channels-state";
import { loadWasm, type Wasm } from "../wasm";
import { paths } from "./paths";

// ============================================================================
// Shared handle / policy types for the publisher-tools panels
// ============================================================================

/** The wasm `PublisherGroup` handle `openPublisherGroup` returns. Shared by
 *  the rotation cron AND the co-publisher panel so a `rotate()` on one is
 *  seen by the other — two handles would commit divergent epochs. */
type PublisherGroupHandle = Awaited<ReturnType<typeof openPublisherGroup>>;
/** wasm-bindgen rotation policy — `{ intervalMs: bigint, maxPosts: number }`. */
type ChannelRotationPolicy = ReturnType<Wasm["channelRotationPolicyDefault"]>;

// ============================================================================
// Theme tokens — the design-system variables from `styles.css`, so the
// channels surface follows the light / dark theme with the rest of the app.
// (This used to be a hardcoded dark palette copied from Call.tsx.)
// ============================================================================

const COLOURS = {
  background: "var(--surface-bg)",
  surface: "var(--surface-elevated)",
  surfaceHover: "var(--surface-hover)",
  border: "var(--surface-border)",
  textPrimary: "var(--surface-fg)",
  textSecondary: "var(--surface-muted)",
  accentBlue: "var(--accent)",
  accentFg: "var(--accent-fg)",
  destructive: "var(--danger)",
} as const;

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

/** Italic secondary-colour placeholder bubble (unreadable / older-key). */
function placeholderBubble(text: string): JSX.Element {
  return (
    <div
      style={{
        "font-size": "13px",
        color: COLOURS.textSecondary,
        "font-style": "italic",
      }}
    >
      {text}
    </div>
  );
}

/** Primary-colour plaintext bubble. */
function textBubble(text: string): JSX.Element {
  return (
    <div
      style={{
        "font-size": "14px",
        color: COLOURS.textPrimary,
        "white-space": "pre-wrap",
        "word-break": "break-word",
      }}
    >
      {text}
    </div>
  );
}

/** Render one post body from its decode outcome. `undefined` means the
 *  decode has not completed yet ("Decoding…"). */
function renderPostBody(decoded: PostDecode | undefined): JSX.Element {
  if (decoded === undefined) {
    return placeholderBubble("Decoding…");
  }
  if (decoded.kind === "text") {
    return textBubble(decoded.text);
  }
  if (decoded.kind === "older-key") {
    return placeholderBubble(
      "message from an older key window — no longer available",
    );
  }
  return placeholderBubble(
    "[unreadable — wrong seed, tampered ciphertext, or a different signing key]",
  );
}

// ============================================================================
// ChannelsList — `/channels`
// ============================================================================

const ChannelsList: Component = () => {
  const navigate = useNavigate();

  // G4-5: subscribe entry point. Accepts EITHER a typed `@handle`
  // (resolved to a channel id via OPRF discovery) OR a pasted
  // 32-char hex channel id (used verbatim). Both land on the same
  // subscribe page.
  const [findInput, setFindInput] = createSignal("");
  const [findBusy, setFindBusy] = createSignal(false);
  const [findError, setFindError] = createSignal<string | null>(null);

  async function onFind(): Promise<void> {
    if (findBusy()) return;
    const raw = findInput().trim();
    if (raw.length === 0) return;
    setFindBusy(true);
    setFindError(null);
    try {
      const idHex = await resolveChannelIdOrHandle(raw);
      if (idHex === null) {
        setFindError(
          `No channel found for ${raw.startsWith("@") ? raw : `@${raw}`}.`,
        );
        setFindBusy(false);
        return;
      }
      navigate(paths.channelSubscribe(idHex));
    } catch (err) {
      setFindError(`Lookup failed: ${String(err)}`);
      setFindBusy(false);
    }
  }

  return (
    <div
      style={{
        padding: "24px 32px",
        "max-width": "720px",
        color: COLOURS.textPrimary,
      }}
    >
      <header
        style={{
          display: "flex",
          "justify-content": "space-between",
          "align-items": "center",
          "margin-bottom": "20px",
        }}
      >
        <h1 style={{ margin: "0", "font-size": "22px", "font-weight": "600" }}>
          Channels
        </h1>
        <button
          type="button"
          onClick={(): void => navigate(paths.channelNew)}
          style={{
            "background-color": COLOURS.accentBlue,
            color: COLOURS.accentFg,
            border: "none",
            "border-radius": "8px",
            padding: "8px 14px",
            "font-size": "14px",
            "font-weight": "500",
            cursor: "pointer",
          }}
        >
          + New channel
        </button>
      </header>

      <form
        onSubmit={(e): void => {
          e.preventDefault();
          void onFind();
        }}
        style={{
          display: "flex",
          "flex-direction": "column",
          gap: "8px",
          "margin-bottom": "20px",
        }}
      >
        <div style={{ display: "flex", gap: "8px" }}>
          <input
            type="text"
            value={findInput()}
            placeholder="@handle or channel id"
            autocapitalize="none"
            autocomplete="off"
            spellcheck={false}
            onInput={(e): void => {
              setFindInput(e.currentTarget.value);
            }}
            style={{
              flex: "1",
              padding: "9px 12px",
              "background-color": COLOURS.surface,
              color: COLOURS.textPrimary,
              border: `1px solid ${COLOURS.border}`,
              "border-radius": "8px",
              "font-size": "14px",
            }}
          />
          <button
            type="submit"
            disabled={findBusy() || findInput().trim().length === 0}
            style={{
              "background-color": COLOURS.surface,
              color: COLOURS.textPrimary,
              border: `1px solid ${COLOURS.border}`,
              "border-radius": "8px",
              padding: "9px 14px",
              "font-size": "14px",
              cursor: findBusy() ? "wait" : "pointer",
              "white-space": "nowrap",
            }}
          >
            {findBusy() ? "Finding…" : "Find & subscribe"}
          </button>
        </div>
        <Show when={findError() !== null}>
          <div
            style={{
              "font-size": "13px",
              color: COLOURS.destructive,
            }}
          >
            {findError()}
          </div>
        </Show>
      </form>

      <Show
        when={channelsState.channels().length > 0}
        fallback={
          <p style={{ color: COLOURS.textSecondary, "line-height": "1.6" }}>
            No channels yet. Create one to start broadcasting, or paste an
            invite link to subscribe.
          </p>
        }
      >
        <ul style={{ "list-style": "none", padding: "0", margin: "0" }}>
          <For each={channelsState.channels()}>
            {(c): JSX.Element => (
              <li
                style={{
                  padding: "14px 16px",
                  "background-color": COLOURS.surface,
                  border: `1px solid ${COLOURS.border}`,
                  "border-radius": "12px",
                  "margin-bottom": "8px",
                  cursor: "pointer",
                }}
                onClick={(): void => navigate(paths.channel(c.id))}
              >
                <div
                  style={{ "font-weight": "600", "margin-bottom": "4px" }}
                >
                  {c.localName || "(unnamed)"}
                </div>
                <Show when={c.localDescription !== ""}>
                  <div
                    style={{
                      "font-size": "13px",
                      color: COLOURS.textSecondary,
                      "margin-bottom": "6px",
                    }}
                  >
                    {c.localDescription}
                  </div>
                </Show>
                <div
                  style={{
                    "font-size": "12px",
                    color: COLOURS.textSecondary,
                    "font-family": "monospace",
                  }}
                >
                  {c.id.slice(0, 8)}…{c.id.slice(-8)}
                </div>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
};

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
      setError(
        "Identity not in memory — refresh the page and sign in again to create a channel.",
      );
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
              `The handle @${wantedHandle.replace(/^@/, "")} is already taken — pick another (or leave it blank).`,
            );
          } else {
            setError(`Could not register the handle: ${String(err)}`);
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
        setError(`Server rejected: ${err.code} — ${err.detail}`);
      } else {
        setError(`Network error: ${String(err)}`);
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
      setCoError("Not signed in.");
      return;
    }
    setCoBusy(true);
    setCoError(null);
    setCoStatus("Requesting…");
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
        setCoStatus("Waiting for the channel owner to approve…");
        let keys: Awaited<ReturnType<typeof pollForCoPublishWelcome>> = null;
        for (let i = 0; i < 60 && keys === null; ++i) {
          keys = await pollForCoPublishWelcome(ds, identity, groupId, joinerKey, channelIdHex);
          if (keys === null) await new Promise((r) => setTimeout(r, 3000));
        }
        if (keys === null) {
          setCoError("Timed out waiting for approval — try again once the owner has approved.");
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
          : `Co-publish failed: ${String(err)}`,
      );
    } finally {
      setCoBusy(false);
      setCoStatus(null);
    }
  }

  return (
    <div
      style={{
        padding: "20px",
        "max-width": "520px",
        margin: "0 auto",
        color: COLOURS.textPrimary,
        "min-height": "100vh",
        "background-color": COLOURS.background,
      }}
    >
      <h1 style={{ margin: "0 0 20px 0", "font-size": "22px" }}>
        New channel
      </h1>
      <form
        onSubmit={(e): void => {
          e.preventDefault();
          void onCreate();
        }}
        style={{
          display: "flex",
          "flex-direction": "column",
          gap: "14px",
        }}
      >
        <label style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
          <span style={{ "font-size": "13px", color: COLOURS.textSecondary }}>
            Name (local only — never sent to the server)
          </span>
          <input
            type="text"
            required
            value={name()}
            onInput={(e): void => {
              setName(e.currentTarget.value);
            }}
            style={{
              padding: "9px 12px",
              "background-color": COLOURS.surface,
              color: COLOURS.textPrimary,
              border: `1px solid ${COLOURS.border}`,
              "border-radius": "8px",
              "font-size": "14px",
            }}
          />
        </label>
        <label style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
          <span style={{ "font-size": "13px", color: COLOURS.textSecondary }}>
            Description (optional, local only)
          </span>
          <textarea
            rows={3}
            value={description()}
            onInput={(e): void => {
              setDescription(e.currentTarget.value);
            }}
            style={{
              padding: "9px 12px",
              "background-color": COLOURS.surface,
              color: COLOURS.textPrimary,
              border: `1px solid ${COLOURS.border}`,
              "border-radius": "8px",
              "font-size": "14px",
              "font-family": "inherit",
              resize: "vertical",
            }}
          />
        </label>
        <label style={{ display: "flex", "flex-direction": "column", gap: "6px" }}>
          <span style={{ "font-size": "13px", color: COLOURS.textSecondary }}>
            Public @handle (optional — others can subscribe by it
            instead of the raw id; only its OPRF hash reaches the
            server)
          </span>
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
            style={{
              padding: "9px 12px",
              "background-color": COLOURS.surface,
              color: COLOURS.textPrimary,
              border: `1px solid ${COLOURS.border}`,
              "border-radius": "8px",
              "font-size": "14px",
            }}
          />
        </label>
        <Show when={error() !== null}>
          <div
            style={{
              padding: "10px 12px",
              "background-color": COLOURS.destructive,
              color: COLOURS.accentFg,
              "border-radius": "8px",
              "font-size": "13px",
            }}
          >
            {error()}
          </div>
        </Show>
        <div style={{ display: "flex", gap: "10px", "justify-content": "flex-end" }}>
          <button
            type="button"
            onClick={(): void => navigate(paths.channels)}
            disabled={busy()}
            style={{
              padding: "9px 16px",
              "background-color": "transparent",
              color: COLOURS.textPrimary,
              border: `1px solid ${COLOURS.border}`,
              "border-radius": "8px",
              cursor: busy() ? "wait" : "pointer",
            }}
          >
            Cancel
          </button>
          <button
            type="submit"
            disabled={busy()}
            style={{
              padding: "9px 16px",
              "background-color": COLOURS.accentBlue,
              color: COLOURS.accentFg,
              border: "none",
              "border-radius": "8px",
              cursor: busy() ? "wait" : "pointer",
              "font-weight": "500",
            }}
          >
            {busy() ? "Creating…" : "Create"}
          </button>
        </div>
      </form>

      <div
        style={{
          "margin-top": "28px",
          "padding-top": "20px",
          "border-top": `1px solid ${COLOURS.border}`,
          display: "flex",
          "flex-direction": "column",
          gap: "10px",
        }}
      >
        <h2 style={{ margin: 0, "font-size": "16px" }}>Join as co-publisher</h2>
        <span style={{ "font-size": "13px", color: COLOURS.textSecondary }}>
          Paste a co-publisher link a channel owner shared. You'll request to
          join; once they approve, this device can publish to the channel.
        </span>
        <input
          type="text"
          placeholder="https://…/g/…"
          value={coLink()}
          onInput={(e): void => {
            setCoLink(e.currentTarget.value);
          }}
          style={{
            padding: "9px 12px",
            "background-color": COLOURS.surface,
            color: COLOURS.textPrimary,
            border: `1px solid ${COLOURS.border}`,
            "border-radius": "8px",
            "font-size": "13px",
            "font-family": "monospace",
          }}
        />
        <Show when={coStatus() !== null}>
          <div style={{ "font-size": "13px", color: COLOURS.textSecondary }}>
            {coStatus()}
          </div>
        </Show>
        <Show when={coError() !== null}>
          <div
            style={{
              padding: "10px 12px",
              "background-color": COLOURS.destructive,
              color: COLOURS.accentFg,
              "border-radius": "8px",
              "font-size": "13px",
            }}
          >
            {coError()}
          </div>
        </Show>
        <button
          type="button"
          disabled={coBusy() || coLink().trim().length === 0}
          onClick={(): void => void onJoinAsCoPublisher()}
          style={{
            "align-self": "flex-start",
            padding: "9px 16px",
            "background-color": "transparent",
            color: COLOURS.textPrimary,
            border: `1px solid ${COLOURS.border}`,
            "border-radius": "8px",
            cursor: coBusy() ? "wait" : "pointer",
            "font-size": "14px",
          }}
        >
          {coBusy() ? "Requesting…" : "Request to co-publish"}
        </button>
      </div>
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
      localName: h?.localName ?? "(unknown channel)",
      localDescription: h?.localDescription ?? "",
    };
  };

  async function onPublish(): Promise<void> {
    if (publishBusy()) return;
    const text = composeText().trim();
    if (text.length === 0) {
      setPublishError("Type something before publishing.");
      return;
    }
    const keys = channelsState.keysByChannel().get(params.id);
    if (keys === undefined) {
      setPublishError(
        "This channel has no local publisher keys — you cannot publish to it from this device.",
      );
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
        setPublishError(`Server rejected: ${err.code} — ${err.detail}`);
      } else {
        setPublishError(`Compose / network error: ${String(err)}`);
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
    <div
      style={{
        padding: "24px 32px",
        "max-width": "720px",
        color: COLOURS.textPrimary,
      }}
    >
      <button
        type="button"
        onClick={(): void => navigate(paths.channels)}
        style={{
          background: "transparent",
          color: COLOURS.textSecondary,
          border: "none",
          padding: "0 0 12px 0",
          cursor: "pointer",
          "font-size": "13px",
        }}
      >
        ← All channels
      </button>
      <h1 style={{ margin: "0 0 4px 0", "font-size": "22px" }}>
        {channelMeta().localName}
      </h1>
      <div
        style={{
          "font-size": "12px",
          color: COLOURS.textSecondary,
          "font-family": "monospace",
          "margin-bottom": "12px",
        }}
      >
        {params.id}
      </div>
      <Show when={channelMeta().localDescription !== ""}>
        <p
          style={{
            margin: "0 0 14px 0",
            color: COLOURS.textSecondary,
            "line-height": "1.6",
          }}
        >
          {channelMeta().localDescription}
        </p>
      </Show>
      <div
        style={{
          "font-size": "13px",
          color: COLOURS.textSecondary,
          "margin-bottom": "20px",
        }}
      >
        Server epoch:{" "}
        <strong style={{ color: COLOURS.textPrimary, "font-family": "monospace" }}>
          {data()?.epoch ?? "…"}
        </strong>
      </div>

      <Show
        when={channelsState.keysByChannel().get(params.id) !== undefined}
        fallback={
          <div
            style={{
              "margin-bottom": "20px",
              padding: "12px",
              "background-color": COLOURS.surface,
              color: COLOURS.textSecondary,
              "border-radius": "8px",
              "font-size": "13px",
              "line-height": "1.6",
            }}
          >
            No local publisher keys for this channel — you cannot
            publish to it from this device. To read posts as a
            subscriber instead,{" "}
            <a
              href={paths.channelSubscribe(params.id)}
              style={{ color: COLOURS.accentBlue }}
            >
              open the subscribe page
            </a>
            .
          </div>
        }
      >
        <form
          onSubmit={(e): void => {
            e.preventDefault();
            void onPublish();
          }}
          style={{ "margin-bottom": "20px" }}
        >
          <textarea
            rows={3}
            placeholder="Write a post…"
            value={composeText()}
            onInput={(e): void => {
              setComposeText(e.currentTarget.value);
            }}
            disabled={publishBusy()}
            style={{
              width: "100%",
              padding: "10px 12px",
              "background-color": COLOURS.surface,
              color: COLOURS.textPrimary,
              border: `1px solid ${COLOURS.border}`,
              "border-radius": "8px",
              "font-size": "14px",
              "font-family": "inherit",
              resize: "vertical",
              "box-sizing": "border-box",
            }}
          />
          <div
            style={{
              "margin-top": "10px",
              display: "flex",
              "justify-content": "flex-end",
            }}
          >
            <button
              type="submit"
              disabled={publishBusy() || composeText().trim().length === 0}
              style={{
                "background-color": COLOURS.accentBlue,
                color: COLOURS.accentFg,
                border: "none",
                "border-radius": "8px",
                padding: "9px 16px",
                "font-size": "14px",
                "font-weight": "500",
                cursor: publishBusy() ? "wait" : "pointer",
                opacity: composeText().trim().length === 0 ? "0.6" : "1",
              }}
            >
              {publishBusy() ? "Publishing…" : "Publish"}
            </button>
          </div>
          <Show when={publishError() !== null}>
            <div
              style={{
                "margin-top": "10px",
                padding: "10px 12px",
                "background-color": COLOURS.destructive,
                color: COLOURS.accentFg,
                "border-radius": "8px",
                "font-size": "13px",
              }}
            >
              {publishError()}
            </div>
          </Show>
        </form>

        <SubscribersPanel channelIdHex={params.id} />
        <PublisherToolsPanel channelIdHex={params.id} />
      </Show>

      <h2 style={{ margin: "0 0 12px 0", "font-size": "16px" }}>Posts</h2>
      <Show
        when={data() !== undefined && data()!.posts.length > 0}
        fallback={
          <p style={{ color: COLOURS.textSecondary, "line-height": "1.6" }}>
            <Show
              when={data.loading}
              fallback={"No posts yet. Publish the first one above."}
            >
              Loading…
            </Show>
          </p>
        }
      >
        <ul style={{ "list-style": "none", padding: "0", margin: "0" }}>
          <For each={data()!.posts}>
            {(p): JSX.Element => (
              <li
                style={{
                  padding: "12px",
                  "background-color": COLOURS.surface,
                  "border-radius": "8px",
                  "margin-bottom": "8px",
                }}
              >
                <div
                  style={{
                    display: "flex",
                    "justify-content": "space-between",
                    "font-size": "12px",
                    color: COLOURS.textSecondary,
                    "margin-bottom": "6px",
                  }}
                >
                  <span>cursor {p.cursor}</span>
                  <span>{new Date(p.serverTsMs).toISOString()}</span>
                </div>
                {renderPostBody(decodedByCursor().get(p.cursor))}
              </li>
            )}
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
      setOpenError(
        "Log in to manage this channel's key rotation and co-publishers.",
      );
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
            (e) => `Could not open the channel group: ${String(e)}`,
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
          <div
            style={{
              "margin-bottom": "20px",
              padding: "10px 12px",
              "background-color": COLOURS.surface,
              color: COLOURS.textSecondary,
              "border-radius": "8px",
              "font-size": "13px",
            }}
          >
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
  const [status, setStatus] = createSignal("Loading rotation state…");
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
    const cadence = "auto every 24 h or 1000 posts";
    const state = loadRotationState(props.channelIdHex);
    if (state === null || state.lastRotationMs <= 0n) {
      setStatus(`Not rotated yet · ${cadence}`);
      return;
    }
    setStatus(
      `Last rotated ${relativeAgo(state.lastRotationMs)} · ` +
        `${state.postsSinceRotation} posts since · ${cadence}`,
    );
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
      setError(`Rotate failed: ${String(err)}`);
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
          setError(`Rotation check failed: ${String(err)}`);
        },
      });
    })();
  });

  onCleanup(() => {
    cron?.stop();
    cron = null;
  });

  return (
    <div
      style={{
        "margin-bottom": "20px",
        padding: "12px",
        "background-color": COLOURS.surface,
        "border-radius": "8px",
      }}
    >
      <h2 style={{ margin: "0 0 6px 0", "font-size": "15px" }}>Key rotation</h2>
      <p
        style={{
          margin: "0 0 8px 0",
          "font-size": "12px",
          color: COLOURS.textSecondary,
          "line-height": "1.6",
        }}
      >
        Rotates the channel content key on a schedule (every 24 h or 1000
        posts) and reseals it to every subscriber — backward secrecy for past
        keys.
      </p>
      <div
        style={{
          "font-size": "12px",
          color: COLOURS.textSecondary,
          "font-family": "monospace",
          "margin-bottom": "10px",
        }}
      >
        {status()}
      </div>
      <button
        type="button"
        disabled={busy()}
        onClick={(): void => {
          void onRotateNow();
        }}
        style={{
          "background-color": COLOURS.accentBlue,
          color: COLOURS.accentFg,
          border: "none",
          "border-radius": "6px",
          padding: "6px 12px",
          "font-size": "13px",
          "font-weight": "500",
          cursor: busy() ? "wait" : "pointer",
          opacity: busy() ? "0.6" : "1",
        }}
      >
        {busy() ? "Rotating…" : "Rotate now"}
      </button>
      <Show when={error() !== null}>
        <div
          style={{
            "margin-top": "8px",
            padding: "8px 10px",
            "background-color": COLOURS.destructive,
            color: COLOURS.accentFg,
            "border-radius": "6px",
            "font-size": "12px",
          }}
        >
          {error()}
        </div>
      </Show>
    </div>
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
      setError(`Co-publisher poll failed: ${String(err)}`);
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
      setError(`Create link failed: ${String(err)}`);
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
      setError(coPublisherRefusal(err) ?? `Approve failed: ${String(err)}`);
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
      setError(coPublisherRefusal(err) ?? `Reject failed: ${String(err)}`);
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

  return (
    <div
      style={{
        margin: "0.6rem 0 0 0",
        padding: "0.5rem",
        border: "1px dashed var(--surface-border)",
        "border-radius": "var(--radius-sm)",
        background: "var(--surface-bg)",
      }}
    >
      <h3
        style={{
          "font-size": "0.8rem",
          "text-transform": "uppercase",
          "letter-spacing": "0.04em",
          color: "var(--surface-muted)",
          margin: "0 0 0.4rem 0",
        }}
      >
        Co-publishers
      </h3>
      <button
        type="button"
        disabled={busy()}
        onClick={() => void onCreateLink()}
        style={{
          "font-size": "0.78rem",
          padding: "0.25rem 0.5rem",
          "border-radius": "var(--radius-sm)",
          border: "1px solid var(--surface-border)",
          background: "var(--surface-bg)",
          color: "var(--surface-fg)",
          cursor: "pointer",
        }}
      >
        Create co-publisher link
      </button>
      <Show when={link() !== null}>
        <div
          style={{
            display: "flex",
            gap: "0.3rem",
            "align-items": "center",
            "margin-top": "0.4rem",
          }}
        >
          <input
            type="text"
            readonly
            value={link() ?? ""}
            style={{
              flex: "1",
              "font-size": "0.72rem",
              "font-family": "monospace",
              padding: "0.2rem 0.35rem",
              "border-radius": "var(--radius-sm)",
              border: "1px solid var(--surface-border)",
              background: "var(--surface)",
              color: "var(--surface-fg)",
              "min-width": "0",
            }}
          />
          <button
            type="button"
            onClick={() => {
              const l = link();
              if (l) void navigator.clipboard?.writeText(l);
            }}
            style={{
              "font-size": "0.72rem",
              padding: "0.2rem 0.45rem",
              "border-radius": "var(--radius-sm)",
              border: "1px solid var(--accent)",
              background: "var(--accent)",
              color: "var(--accent-fg)",
              cursor: "pointer",
            }}
          >
            Copy
          </button>
        </div>
      </Show>
      <Show when={pending().length > 0}>
        <ul
          style={{
            "list-style": "none",
            padding: 0,
            margin: "0.4rem 0 0 0",
            display: "flex",
            "flex-direction": "column",
            gap: "0.3rem",
          }}
        >
          <For each={pending()}>
            {(ref) => (
              <li
                style={{
                  display: "flex",
                  "align-items": "center",
                  "justify-content": "space-between",
                  gap: "0.5rem",
                }}
              >
                <span
                  title={hexOf(ref)}
                  style={{
                    "font-family": "monospace",
                    "font-size": "0.75rem",
                    color: "var(--surface-fg)",
                    overflow: "hidden",
                    "text-overflow": "ellipsis",
                    "white-space": "nowrap",
                  }}
                >
                  {hexOf(ref).slice(0, 16)}…
                </span>
                <span style={{ display: "flex", gap: "0.3rem" }}>
                  <button
                    type="button"
                    disabled={busy()}
                    onClick={() => void onApprove(ref)}
                    style={{
                      "font-size": "0.75rem",
                      padding: "0.2rem 0.45rem",
                      "border-radius": "var(--radius-sm)",
                      border: "1px solid var(--accent)",
                      background: "var(--accent)",
                      color: "var(--accent-fg)",
                      cursor: "pointer",
                    }}
                  >
                    Approve
                  </button>
                  <button
                    type="button"
                    disabled={busy()}
                    onClick={() => onReject(ref)}
                    style={{
                      "font-size": "0.75rem",
                      padding: "0.2rem 0.45rem",
                      "border-radius": "var(--radius-sm)",
                      border: "1px solid var(--surface-border)",
                      background: "transparent",
                      color: "var(--surface-fg)",
                      cursor: "pointer",
                    }}
                  >
                    Reject
                  </button>
                </span>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={error() !== null}>
        <p
          style={{
            "font-size": "0.72rem",
            color: "var(--danger, #c0392b)",
            margin: "0.4rem 0 0 0",
          }}
        >
          {error()}
        </p>
      </Show>
    </div>
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
      setError(`List failed: ${String(err)}`);
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
      setError("No publisher keys on this device.");
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
          setError(`Deliver failed: ${err.code} — ${err.detail}`);
        } else {
          setError(`Deliver failed: ${String(err)}`);
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
    <div
      style={{
        "margin-bottom": "20px",
        padding: "12px",
        "background-color": COLOURS.surface,
        "border-radius": "8px",
      }}
    >
      <div
        style={{
          display: "flex",
          "justify-content": "space-between",
          "align-items": "center",
          "margin-bottom": "8px",
        }}
      >
        <h2 style={{ margin: "0", "font-size": "15px" }}>
          Subscribers{" "}
          <span style={{ color: COLOURS.textSecondary, "font-weight": "400" }}>
            ({subs()?.length ?? "…"})
          </span>
        </h2>
        <button
          type="button"
          onClick={(): void => {
            void refresh();
          }}
          style={{
            background: "transparent",
            color: COLOURS.textSecondary,
            border: `1px solid ${COLOURS.border}`,
            "border-radius": "6px",
            padding: "4px 10px",
            "font-size": "12px",
            cursor: "pointer",
          }}
        >
          Refresh
        </button>
      </div>
      <Show when={error() !== null}>
        <div
          style={{
            padding: "8px 10px",
            "background-color": COLOURS.destructive,
            color: COLOURS.accentFg,
            "border-radius": "6px",
            "font-size": "12px",
            "margin-bottom": "8px",
          }}
        >
          {error()}
        </div>
      </Show>
      <Show
        when={subs() !== null && subs()!.length > 0}
        fallback={
          <div
            style={{
              color: COLOURS.textSecondary,
              "font-size": "12px",
              "line-height": "1.6",
            }}
          >
            <Show
              when={subs() === null}
              fallback="No subscribers yet. Anyone with the channel id can open the subscribe page and register their X25519 public key."
            >
              Loading subscribers…
            </Show>
          </div>
        }
      >
        <ul style={{ "list-style": "none", padding: "0", margin: "0" }}>
          <For each={subs()!}>
            {(pk): JSX.Element => {
              const status = (): "delivered" | "auto" | "failed" | undefined =>
                statusByPk().get(pk);
              return (
                <li
                  style={{
                    display: "flex",
                    "align-items": "center",
                    "justify-content": "space-between",
                    padding: "8px 10px",
                    "background-color": COLOURS.background,
                    "border-radius": "6px",
                    "margin-bottom": "6px",
                    "font-family": "monospace",
                    "font-size": "11px",
                    gap: "10px",
                  }}
                >
                  <span
                    style={{
                      color: COLOURS.textSecondary,
                      "word-break": "break-all",
                      flex: "1",
                    }}
                  >
                    {pk}
                  </span>
                  <Show when={status() === "delivered"}>
                    <span
                      style={{
                        color: COLOURS.accentBlue,
                        "font-family": "inherit",
                        "font-size": "11px",
                      }}
                    >
                      delivered
                    </span>
                  </Show>
                  <Show when={status() === "auto"}>
                    <span
                      style={{
                        color: COLOURS.accentBlue,
                        "font-family": "inherit",
                        "font-size": "11px",
                      }}
                      title="Auto-delivered by the 5 s polling loop"
                    >
                      auto-delivered
                    </span>
                  </Show>
                  <Show when={status() === "failed"}>
                    <span
                      style={{
                        color: COLOURS.destructive,
                        "font-family": "inherit",
                        "font-size": "11px",
                      }}
                    >
                      failed
                    </span>
                  </Show>
                  <button
                    type="button"
                    onClick={(): void => {
                      void deliverTo(pk);
                    }}
                    disabled={busyPk() === pk}
                    style={{
                      "background-color": COLOURS.accentBlue,
                      color: COLOURS.accentFg,
                      border: "none",
                      "border-radius": "6px",
                      padding: "4px 10px",
                      "font-size": "11px",
                      "font-family": "inherit",
                      cursor: busyPk() === pk ? "wait" : "pointer",
                    }}
                  >
                    {busyPk() === pk ? "Sealing…" : "Deliver seed"}
                  </button>
                </li>
              );
            }}
          </For>
        </ul>
      </Show>
    </div>
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
        setBootstrapError(`${err.code} — ${err.detail}`);
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

  const myKeypair = (): { readonly public: Uint8Array } | undefined =>
    channelsState.subscriberKeysByChannel().get(params.id);

  return (
    <div
      style={{
        padding: "24px 32px",
        "max-width": "720px",
        color: COLOURS.textPrimary,
      }}
    >
      <button
        type="button"
        onClick={(): void => navigate(paths.channels)}
        style={{
          background: "transparent",
          color: COLOURS.textSecondary,
          border: "none",
          padding: "0 0 12px 0",
          cursor: "pointer",
          "font-size": "13px",
        }}
      >
        ← All channels
      </button>
      <h1 style={{ margin: "0 0 4px 0", "font-size": "22px" }}>Subscribe</h1>
      <div
        style={{
          "font-size": "12px",
          color: COLOURS.textSecondary,
          "font-family": "monospace",
          "margin-bottom": "16px",
        }}
      >
        {params.id}
      </div>

      <div
        style={{
          padding: "12px",
          "background-color": COLOURS.surface,
          "border-radius": "8px",
          "margin-bottom": "16px",
          "font-size": "13px",
          "line-height": "1.6",
          color: COLOURS.textSecondary,
        }}
      >
        <Show when={myKeypair() !== undefined}>
          <div style={{ "margin-bottom": "6px" }}>
            Your subscriber public key (registered on this channel):
          </div>
          <div
            style={{
              "font-family": "monospace",
              "font-size": "11px",
              color: COLOURS.textPrimary,
              "word-break": "break-all",
            }}
          >
            {bytesToHex(myKeypair()!.public)}
          </div>
        </Show>
      </div>

      <div
        style={{
          padding: "12px",
          "background-color": COLOURS.surface,
          "border-radius": "8px",
          "margin-bottom": "20px",
          "font-size": "13px",
          "line-height": "1.6",
        }}
      >
        <Show when={bootstrap() === "idle" || bootstrap() === "registering"}>
          <span style={{ color: COLOURS.textSecondary }}>
            Registering your key on the channel…
          </span>
        </Show>
        <Show when={bootstrap() === "polling"}>
          <span style={{ color: COLOURS.textSecondary }}>
            Registered. Waiting for the publisher to fan out a seed envelope…
            (auto-refreshes every 3 s)
          </span>
        </Show>
        <Show when={bootstrap() === "active"}>
          <span style={{ color: COLOURS.accentBlue }}>
            Seed envelope opened. You can read posts on this channel.
          </span>
        </Show>
        <Show when={bootstrap() === "error"}>
          <span style={{ color: COLOURS.destructive }}>
            {bootstrapError() ?? "Unknown error."}
          </span>
        </Show>
      </div>

      <h2 style={{ margin: "0 0 12px 0", "font-size": "16px" }}>Posts</h2>
      <Show
        when={bootstrap() === "active" && posts().length > 0}
        fallback={
          <p style={{ color: COLOURS.textSecondary, "line-height": "1.6" }}>
            <Show
              when={bootstrap() === "active"}
              fallback={
                bootstrap() === "polling"
                  ? "Waiting for the publisher to deliver your seed."
                  : "Bootstrapping the subscribe flow…"
              }
            >
              No posts yet on this channel.
            </Show>
          </p>
        }
      >
        <ul style={{ "list-style": "none", padding: "0", margin: "0" }}>
          <For each={posts()}>
            {(p): JSX.Element => (
              <li
                style={{
                  padding: "12px",
                  "background-color": COLOURS.surface,
                  "border-radius": "8px",
                  "margin-bottom": "8px",
                }}
              >
                <div
                  style={{
                    display: "flex",
                    "justify-content": "space-between",
                    "font-size": "12px",
                    color: COLOURS.textSecondary,
                    "margin-bottom": "6px",
                  }}
                >
                  <span>cursor {p.cursor}</span>
                  <span>{new Date(p.serverTsMs).toISOString()}</span>
                </div>
                {renderPostBody(decodedByCursor().get(p.cursor))}
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
};

// ============================================================================
// Module exports
// ============================================================================

export { ChannelsList, NewChannel, ChannelView, ChannelSubscribe };
