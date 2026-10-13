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
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
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
          // ADR-0073 phase 2 (UX 16.6) — owned under keys since reset.
          orphanedByKeyReset: channelsState.orphanedByKeyReset().has(c.id),
        },
        clock(),
      ),
    })),
  );

  /** An owner reads and writes on the channel view; a subscriber reads on
   *  the subscribe view, which holds the per-epoch catch-up. */
  const hrefOf = (id: string): string =>
    isOwner(id) || channelsState.orphanedByKeyReset().has(id)
      ? paths.channel(id)
      : paths.channelSubscribe(id);

  // Search: filters locally; Enter with no match resolves an @handle.
  const [query, setQuery] = createSignal("");
  const [findBusy, setFindBusy] = createSignal(false);
  const [findError, setFindError] = createLiveText();
  /** The typed handle with its `@`. */
  const atHandle = (raw: string): string => (raw.startsWith("@") ? raw : `@${raw}`);
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
        const handle = atHandle(raw);
        setFindError(() => t("channels.find.none", { handle }));
        return;
      }
      const handle = raw.replace(/^@/, "");
      setQuery("");
      navigate(paths.channelSubscribe(idHex), {
        // A pasted 32-char id carries no handle; only a real handle is kept.
        state: /^[0-9a-f]{32}$/i.test(handle) ? {} : { handle },
      });
    } catch (err) {
      // ADR-0054 S3: a handle lookup spends an anonymous token.
      if (anonTokenMessage(err, "") === null) console.warn("[channels] handle lookup failed", err);
      setFindError(
        () =>
          anonTokenMessage(err, t("chats.lookup.unreachable")) ?? t("chats.lookup.unreachable"),
      );
    } finally {
      setFindBusy(false);
    }
  }

  return (
    <div class="lk-split" data-open={open() ? "true" : "false"}>
      <aside class="lk-pane" aria-label={t("nav.channels")}>
        <div class="lk-pane-head">
          <h1>{t("nav.channels")}</h1>
          <div class="lk-menu-anchor">
            <button
              type="button"
              class="lk-icon-button accent"
              title={t("chats.compose")}
              aria-label={t("channels.new_menu")}
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
              <div class="lk-menu" role="menu" aria-label={t("chats.compose")}>
                <button type="button" role="menuitem" onClick={(): void => go(paths.channelNew)}>
                  <Icon name="channels" size={18} />
                  {t("channels.new.title")}
                </button>
                <button type="button" role="menuitem" onClick={(): void => go(paths.channelJoin)}>
                  <Icon name="link" size={18} />
                  {t("channels.join.title")}
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
            placeholder={t("channels.search.placeholder")}
            aria-label={t("channels.search.label")}
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
              <strong>{t("channels.empty.title")}</strong>
              <button type="button" class="primary" onClick={(): void => navigate(paths.channelNew)}>
                {t("channels.empty.action")}
              </button>
            </div>
          }
        >
          <div class="lk-pane-body">
            <Show when={visible().length === 0}>
              <div class="lk-pane-nomatch lk-stack">
                <span>{t("chats.search.no_results")}</span>
                <Show when={query().trim().length > 0}>
                  <button
                    type="button"
                    class="small lk-self-center"
                    disabled={findBusy()}
                    onClick={(): void => void onFind()}
                  >
                    {findBusy()
                      ? t("common.looking_up")
                      : t("channels.find.action", { handle: atHandle(query().trim()) })}
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
      <span class="lk-hint-pill">{t("channels.index_hint")}</span>
    </div>
  );
}
