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
 *                     posts list + publish stub.
 *
 * Real ChannelPost composition + signature verify + AEAD
 * open lands when `lakko-channels` is exposed through
 * `lakko-ffi-wasm` — a follow-up sub-sprint. Until then
 * the publish form sends a stub random body (the server
 * stores opaque bytes and hands them back; the UI shows
 * the post in the list as base64).
 */

import {
  createEffect,
  createResource,
  createSignal,
  For,
  Show,
  type Component,
  type JSX,
} from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";

import {
  apiError,
  createChannel,
  fetchEpoch,
  fetchPosts,
  publishPost,
  type ChannelPostView,
} from "../channels/api";
import {
  composePost,
  freshPublisherKeys,
  utf8Decode,
  utf8Encode,
  verifyAndDecryptPost,
} from "../channels/composer";
import { channelsActions, channelsState } from "../state/channels-state";
import { paths } from "./paths";

// ============================================================================
// Theme tokens — mirror Call.tsx's palette so the channels
// surface feels visually contiguous with the rest of the app.
// ============================================================================

const COLOURS = {
  background: "#0F0F0F",
  surface: "#1A1A1A",
  surfaceHover: "#202020",
  border: "#2A2A2A",
  textPrimary: "#FAFAFA",
  textSecondary: "#A1A1AA",
  accentBlue: "#3B82F6",
  destructive: "#DC2626",
} as const;

// ============================================================================
// ChannelsList — `/channels`
// ============================================================================

const ChannelsList: Component = () => {
  const navigate = useNavigate();
  return (
    <div
      style={{
        padding: "20px",
        "max-width": "720px",
        margin: "0 auto",
        color: COLOURS.textPrimary,
        "min-height": "100vh",
        "background-color": COLOURS.background,
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
            color: "#fff",
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
                  padding: "14px",
                  "background-color": COLOURS.surface,
                  "border-radius": "8px",
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
  const [name, setName] = createSignal("");
  const [description, setDescription] = createSignal("");
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
    setBusy(true);
    setError(null);
    const id = freshChannelIdHex();
    try {
      await createChannel(id);
      // Mint local publisher keys — single-publisher demo
      // affordance until the PublisherGroup MLS exporter
      // wiring lands. Stored in-memory only; a future
      // sub-sprint persists them to the device-group-
      // keyed sqlite cache.
      const keys = await freshPublisherKeys();
      channelsActions.addChannel({
        id,
        localName: name().trim(),
        localDescription: description().trim(),
        addedAtMs: Date.now(),
      });
      channelsActions.setKeys(id, keys);
      navigate(paths.channel(id));
    } catch (err) {
      if (apiError.is(err)) {
        setError(`Server rejected: ${err.code} — ${err.detail}`);
      } else {
        setError(`Network error: ${String(err)}`);
      }
      setBusy(false);
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
        <Show when={error() !== null}>
          <div
            style={{
              padding: "10px 12px",
              "background-color": COLOURS.destructive,
              color: "#fff",
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
              color: "#fff",
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
    ReadonlyMap<number, string | null>
  >(new Map());

  // Decode every post in the latest fetch into plaintext
  // text. Runs whenever the post list or the channel
  // changes. We only have the seed for channels we
  // publish to ourselves — for someone else's channel
  // the subscriber path supplies the seed via HPKE
  // envelope, but that wiring is a follow-up sub-sprint.
  createEffect(() => {
    const d = data();
    const keys = channelsState.keysByChannel().get(params.id);
    if (d === undefined || keys === undefined) {
      setDecodedByCursor(new Map());
      return;
    }
    void Promise.all(
      d.posts.map(async (p) => {
        try {
          const plaintext = await verifyAndDecryptPost(
            keys.seed,
            params.id,
            p.payloadB64,
          );
          return [p.cursor, utf8Decode(plaintext)] as const;
        } catch {
          return [p.cursor, null] as const;
        }
      }),
    ).then((entries) => {
      setDecodedByCursor(new Map(entries));
    });
  });

  return (
    <div
      style={{
        padding: "20px",
        "max-width": "720px",
        margin: "0 auto",
        color: COLOURS.textPrimary,
        "min-height": "100vh",
        "background-color": COLOURS.background,
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
            No local publisher keys for this channel. Subscriber-side
            view + HPKE seed-envelope reception arrive in a follow-up
            sub-sprint.
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
                color: "#fff",
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
                color: "#fff",
                "border-radius": "8px",
                "font-size": "13px",
              }}
            >
              {publishError()}
            </div>
          </Show>
        </form>
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
                <Show
                  when={decodedByCursor().has(p.cursor)}
                  fallback={
                    <div
                      style={{
                        "font-size": "13px",
                        color: COLOURS.textSecondary,
                        "font-style": "italic",
                      }}
                    >
                      Decoding…
                    </div>
                  }
                >
                  {(() => {
                    const decoded = decodedByCursor().get(p.cursor) ?? null;
                    if (decoded === null) {
                      return (
                        <div
                          style={{
                            "font-size": "13px",
                            color: COLOURS.textSecondary,
                            "font-style": "italic",
                          }}
                        >
                          [unreadable — wrong seed, tampered ciphertext, or
                          posted by another publisher with a different
                          signing key]
                        </div>
                      );
                    }
                    return (
                      <div
                        style={{
                          "font-size": "14px",
                          color: COLOURS.textPrimary,
                          "white-space": "pre-wrap",
                          "word-break": "break-word",
                        }}
                      >
                        {decoded}
                      </div>
                    );
                  })()}
                </Show>
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

export { ChannelsList, NewChannel, ChannelView };
