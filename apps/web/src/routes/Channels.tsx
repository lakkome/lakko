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
      channelsActions.addChannel({
        id,
        localName: name().trim(),
        localDescription: description().trim(),
        addedAtMs: Date.now(),
      });
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

  async function onPublishStub(): Promise<void> {
    if (publishBusy()) return;
    setPublishBusy(true);
    setPublishError(null);
    // Stub body — until the WASM ChannelPost composer
    // ships, the publish form sends a random 64-byte
    // payload. The server stores it opaque; the post
    // appears in the list as base64. Real
    // sign+encrypt+serialise rides on a follow-up
    // sub-sprint.
    const stub = new Uint8Array(64);
    crypto.getRandomValues(stub);
    try {
      await publishPost(params.id, stub);
      await refetch();
    } catch (err) {
      if (apiError.is(err)) {
        setPublishError(`Server rejected: ${err.code} — ${err.detail}`);
      } else {
        setPublishError(`Network error: ${String(err)}`);
      }
    } finally {
      setPublishBusy(false);
    }
  }

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

      <div style={{ "margin-bottom": "20px" }}>
        <button
          type="button"
          onClick={(): void => {
            void onPublishStub();
          }}
          disabled={publishBusy()}
          style={{
            "background-color": COLOURS.accentBlue,
            color: "#fff",
            border: "none",
            "border-radius": "8px",
            padding: "9px 16px",
            "font-size": "14px",
            "font-weight": "500",
            cursor: publishBusy() ? "wait" : "pointer",
          }}
        >
          {publishBusy() ? "Publishing…" : "Publish (stub: random 64 bytes)"}
        </button>
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
      </div>

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
                <div
                  style={{
                    "font-family": "monospace",
                    "font-size": "12px",
                    "word-break": "break-all",
                    color: COLOURS.textPrimary,
                  }}
                >
                  {p.payloadB64.slice(0, 80)}
                  {p.payloadB64.length > 80 ? "…" : ""}
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
// Module exports
// ============================================================================

export { ChannelsList, NewChannel, ChannelView };
