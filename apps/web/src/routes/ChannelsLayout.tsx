// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Channels destination (docs/ux/client-redesign.md section 5.4): the
 * channel list pane on the left with the Chats list vocabulary, the open
 * channel, the create form, the co-publisher join or the subscribe flow on
 * the right. Wraps every `/channels*` route so the list stays mounted; a
 * narrow screen shows one pane at a time.
 *
 * The search field filters the list by name and handle; Enter with no local
 * match looks the text up as an `@handle` (OPRF discovery) and opens the
 * subscribe page, which is how a channel someone else runs is found.
 */
import { anonTokenMessage } from "../tokens/token-copy";
import { createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { useLocation, useNavigate, useParams, type RouteSectionProps } from "@solidjs/router";

import { resolveChannelIdOrHandle } from "../channels/channel-discovery";
import { channelRowMatches, channelRowModel, type ChannelRowModel } from "../channels/channel-row";
import { ChannelRow } from "../channels/ChannelRow";
import { createChannelUnread } from "../feed/channel-unread";
import { knownSubscriberCount } from "../channels/ui-cache";
import type { ChannelHandle } from "../channels/api";
import { channelsState } from "../state/channels-state";
import { Icon } from "../ui/icons";
import { paths } from "./paths";

export default function ChannelsLayout(props: RouteSectionProps): JSX.Element {
  const params = useParams<{ id?: string }>();
  // docs/ux/feed.md section 5 — each row's count of new posts.
  const channelUnread = createChannelUnread();
  const location = useLocation();
  const navigate = useNavigate();

  const open = (): boolean => params.id !== undefined || location.pathname !== paths.channels;

  // The compose menu: New channel, Join as co-publisher.
  const [menuOpen, setMenuOpen] = createSignal(false);
  const go = (to: string): void => {
    setMenuOpen(false);
    navigate(to);
  };

  // The row times relabel themselves as the day turns.
  const [clock, setClock] = createSignal(new Date());
  onMount(() => {
    const t = setInterval(() => setClock(new Date()), 60_000);
    onCleanup(() => clearInterval(t));
  });

  const lastPostMsOf = (id: string): number | null => {
    const posts = channelsState.postsByChannel().get(id);
    if (posts === undefined || posts.length === 0) return null;
    return posts.reduce((max, p) => Math.max(max, p.serverTsMs), 0);
  };

  const isOwner = (id: string): boolean => channelsState.keysByChannel().get(id) !== undefined;

  const rows = createMemo((): ReadonlyArray<{ channel: ChannelHandle; model: ChannelRowModel }> =>
    channelsState.channels().map((c) => ({
      channel: c,
      model: channelRowModel(
        {
          id: c.id,
          sharedName: channelsState.metaByChannel().get(c.id)?.name,
          localName: c.localName,
          localHandle: c.localHandle,
          isOwner: isOwner(c.id),
          subscriberCount: knownSubscriberCount(c.id),
          lastPostMs: lastPostMsOf(c.id),
        },
        clock(),
      ),
    })),
  );

  /** An owner reads and writes on the channel view; a subscriber reads on
   *  the subscribe view, which holds the per-epoch catch-up. */
  const hrefOf = (id: string): string =>
    isOwner(id) ? paths.channel(id) : paths.channelSubscribe(id);

  // Search: filters locally; Enter with no match resolves an @handle.
  const [query, setQuery] = createSignal("");
  const [findBusy, setFindBusy] = createSignal(false);
  const [findError, setFindError] = createSignal<string | null>(null);
  const visible = (): ReadonlyArray<{ channel: ChannelHandle; model: ChannelRowModel }> =>
    rows().filter((r) => channelRowMatches(r.model, query()));

  async function onFind(): Promise<void> {
    if (findBusy()) return;
    const raw = query().trim();
    if (raw.length === 0) return;
    setFindBusy(true);
    setFindError(null);
    try {
      const idHex = await resolveChannelIdOrHandle(raw);
      if (idHex === null) {
        setFindError(`No channel found for ${raw.startsWith("@") ? raw : `@${raw}`}.`);
        return;
      }
      const handle = raw.replace(/^@/, "");
      setQuery("");
      navigate(paths.channelSubscribe(idHex), {
        // A pasted 32-char id carries no handle; only a real handle is kept.
        state: /^[0-9a-f]{32}$/i.test(handle) ? {} : { handle },
      });
    } catch (err) {
      setFindError(
        // ADR-0054 S3: a handle lookup spends an anonymous token.
        anonTokenMessage(err, "Could not look that up. Check your connection and try again.") ??
          `Could not look that up. Check your connection and try again. (${String(err)})`,
      );
    } finally {
      setFindBusy(false);
    }
  }

  return (
    <div class="lk-split" data-open={open() ? "true" : "false"}>
      <aside class="lk-pane" aria-label="Channels">
        <div class="lk-pane-head">
          <h1>Channels</h1>
          <div class="lk-menu-anchor">
            <button
              type="button"
              class="lk-icon-button accent"
              title="New"
              aria-label="New channel or join one"
              aria-haspopup="menu"
              aria-expanded={menuOpen()}
              onClick={(): void => {
                setMenuOpen(!menuOpen());
              }}
            >
              <Icon name="plus" size={18} />
            </button>
            <Show when={menuOpen()}>
              <div
                class="lk-menu-backdrop"
                onClick={(): void => {
                  setMenuOpen(false);
                }}
              />
              <div class="lk-menu" role="menu" aria-label="New">
                <button type="button" role="menuitem" onClick={(): void => go(paths.channelNew)}>
                  <Icon name="channels" size={18} />
                  New channel
                </button>
                <button type="button" role="menuitem" onClick={(): void => go(paths.channelJoin)}>
                  <Icon name="link" size={18} />
                  Join as co-publisher
                </button>
              </div>
            </Show>
          </div>
        </div>
        <form
          class="lk-search lk-pane-search"
          onSubmit={(e): void => {
            e.preventDefault();
            void onFind();
          }}
        >
          <Icon name="search" size={16} />
          <input
            type="search"
            placeholder="Search or find by @handle"
            aria-label="Search channels or find one by handle"
            autocapitalize="none"
            autocomplete="off"
            spellcheck={false}
            value={query()}
            disabled={findBusy()}
            onInput={(e): void => {
              setQuery(e.currentTarget.value);
              setFindError(null);
            }}
          />
        </form>
        <Show when={findError()}>
          {(msg) => (
            <div class="lk-pane-note lk-alert danger" role="alert">
              {msg()}
            </div>
          )}
        </Show>
        <Show
          when={rows().length > 0}
          fallback={
            <div class="lk-empty">
              <Icon name="channels" size={40} strokeWidth={1.4} />
              <strong>No channels yet</strong>
              <button type="button" class="primary" onClick={(): void => navigate(paths.channelNew)}>
                Create a channel
              </button>
            </div>
          }
        >
          <div class="lk-pane-body">
            <Show when={visible().length === 0}>
              <div class="lk-pane-nomatch lk-stack">
                <span>No results</span>
                <Show when={query().trim().length > 0}>
                  <button
                    type="button"
                    class="small lk-self-center"
                    disabled={findBusy()}
                    onClick={(): void => void onFind()}
                  >
                    {findBusy() ? "Looking..." : `Find ${query().trim().startsWith("@") ? query().trim() : `@${query().trim()}`}`}
                  </button>
                </Show>
              </div>
            </Show>
            <For each={visible()}>
              {(r) => (
                <ChannelRow
                  id={r.channel.id}
                  href={hrefOf(r.channel.id)}
                  model={r.model}
                  selected={params.id === r.channel.id}
                  unread={channelUnread()(r.channel.id)}
                />
              )}
            </For>
          </div>
        </Show>
      </aside>
      <div class="lk-main">{props.children}</div>
    </div>
  );
}

/** `/channels` with nothing open: one hint in the main pane. */
export function ChannelsIndex(): JSX.Element {
  return (
    <div class="lk-main-center">
      <span class="lk-hint-pill">Select a channel, or create one</span>
    </div>
  );
}
