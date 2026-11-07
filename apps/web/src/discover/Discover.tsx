// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Discover (docs/ux/feed.md section 6, ADR-0059): the Discover preset of the
 * Feed and the source of suggestions other feeds mix in.
 *
 * {@link createDiscoverModel} holds the state (per account, sealed on this
 * device, `discover-store.ts`) and the network: the once-a-day shortlist
 * refresh in rounds of eight buckets padded with the month's decoys
 * (`decoys.ts`), the explicit search (one `directory-search` token; the UI
 * says it is sent), the listing fetch of a group-shared card when it is
 * opened, and the handle check before subscribing (`cards.verifyDecision`).
 * {@link DiscoverView} draws it; {@link DiscoverCardView} is one card, also
 * used where a feed mixes suggestions in.
 */

import { initialsOf } from "../ui/Avatar";
import { createMemo, createSignal, For, onMount, Show, type Accessor, type JSX } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";

import { cardsWithoutHidden } from "../feed/feed-marks";
import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { apiError, fetchBucket, fetchListing, searchDirectory, type DirectoryItem } from "../channels/api";
import { lookupPublicChannel } from "../channels/channel-lookup";
import { subscribeToChannel } from "../channels/channel-access";
import { bytesToHex } from "../channels/composer";
import { paths } from "../routes/paths";
import { channelsState } from "../state/channels-state";
import { anonTokenQuotaExhausted, isAnonTokenNetworkFailure } from "../tokens/anon-token-pool";
import { LOOKUP_ALLOWANCE_USED, SEARCH_ALLOWANCE_USED } from "../tokens/token-copy";
import { loadWasm, type Wasm } from "../wasm";
import {
  cardsFromItems,
  rankCards,
  reasonLine,
  cardRelation,
  type CardRelation,
  verifyDecision,
  type CardContext,
  type DecodedListing,
  type DirectoryCard,
} from "./cards";
import { currentDecoys, pickedKey, planBucketRounds, refreshDue } from "./decoys";
import {
  bucketKey,
  defaultDiscoverPrefs,
  loadDiscoverPrefs,
  MAX_NOT_INTERESTED,
  saveDiscoverPrefs,
  withRefreshOutcome,
  type CachedBucket,
  type DiscoverPrefs,
} from "./discover-store";
import { appLanguage, languageLabel, languageOptions } from "./languages";
import { topicLabel } from "./topic-names";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { aggregateShared, type SharedChannelsRow } from "./shared-channels";
import { screenAnchor } from "../ui/screen-anchor";
import { LakkoPicker } from "../ui/LakkoPicker";

// ----- Copy (UX spec sections 6 and 7) ---------------------------------------

/** Each entry renders in the current language when called, so copy on
 *  screen follows a language switch (ADR-0061). */
export const COPY = {
  pickTitle: (): string => t("discover.pick.title"),
  pickBody: (): string => t("discover.pick.body"),
  mixLabel: (): string => t("discover.mix.label"),
  unavailable: (): string => t("discover.unavailable"),
  noResults: (): string => t("discover.search.no_results"),
  gone: (): string => t("discover.gone"),
  rateLimited: SEARCH_ALLOWANCE_USED,
  searchTooShort: (): string => t("discover.search.too_short"),
  nothingYet: (): string => t("discover.nothing_yet"),
} as const;

// ----- Model -------------------------------------------------------------------

type ListingDecoder = Pick<Wasm, "decodeChannelListing">;

function base64UrlToBytes(b64: string): Uint8Array {
  const std = b64.replace(/-/g, "+").replace(/_/g, "/").padEnd(b64.length + ((4 - (b64.length % 4)) % 4), "=");
  const bin = atob(std);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

/** Decode one listing with the shared codec; `null` when it refuses. */
export function decodeListingB64(w: ListingDecoder, listingB64: string): DecodedListing | null {
  try {
    const r = w.decodeChannelListing(base64UrlToBytes(listingB64));
    try {
      return {
        channelIdHex: bytesToHex(r.channelId),
        handle: r.handle,
        name: r.name,
        description: r.description,
        topic: r.topic,
        language: r.language,
      };
    } finally {
      r.free();
    }
  } catch {
    return null;
  }
}

export interface DiscoverDeps {
  /** The groups this device is in, for the group-shared source. */
  readonly liveGroups: Accessor<ReadonlySet<string>>;
  readonly groupTitle: (groupIdB64: string) => string;
  /** A contact's display name for a member key, `null` for a non-contact. */
  readonly contactName: (memberKeyB64: string) => string | null;
  readonly ownKeyB64: Accessor<string | null>;
  readonly sharedRow: Accessor<SharedChannelsRow>;
  /** Conversations hidden from feeds on this device (`docs/ux/feed.md`
   *  section 5): left out of the suggestions before they are ranked. */
  readonly hiddenFromFeeds?: Accessor<ReadonlySet<string>>;
}

export type SearchState =
  | { readonly kind: "idle" }
  | { readonly kind: "busy" }
  | { readonly kind: "done"; readonly query: string; readonly cards: readonly DirectoryCard[] }
  | { readonly kind: "error"; readonly message: () => string };

export interface DiscoverModel {
  readonly prefs: Accessor<DiscoverPrefs>;
  readonly loaded: Accessor<boolean>;
  readonly topics: Accessor<ReadonlyArray<{ readonly id: number; readonly name: string }>>;
  readonly language: Accessor<string>;
  /** The suggestions, ranked on this device. */
  readonly cards: Accessor<readonly DirectoryCard[]>;
  readonly refreshing: Accessor<boolean>;
  readonly unavailable: Accessor<boolean>;
  readonly search: Accessor<SearchState>;
  readonly verified: Accessor<ReadonlySet<string>>;
  readonly notice: Accessor<string | null>;
  topicName(id: number): string;
  savePicks(topics: readonly number[], language: string): Promise<void>;
  dismissIntro(): Promise<void>;
  setMixRandom(on: boolean): Promise<void>;
  notInterested(channelIdHex: string): Promise<void>;
  hideTopic(topic: number): Promise<void>;
  refresh(force?: boolean): Promise<void>;
  runSearch(query: string): Promise<void>;
  clearSearch(): void;
  /** Fetch the public listing of a group-shared card (description, topic). */
  openCard(card: DirectoryCard): Promise<void>;
  /** Verify the handle, then go to the subscribe screen. */
  subscribe(card: DirectoryCard): Promise<void>;
  /** Whether the card's channel is this device's own, followed, or neither. */
  relation(channelIdHex: string): CardRelation;
  /** Open a channel this device already holds. */
  openChannel(channelIdHex: string): void;
}

export function createDiscoverModel(deps: DiscoverDeps): DiscoverModel {
  const auth = useAuth();
  const navigate = useNavigate();
  const location = useLocation();
  const [prefs, setPrefs] = createSignal<DiscoverPrefs>(defaultDiscoverPrefs());
  const [loaded, setLoaded] = createSignal(false);
  const [wasm, setWasm] = createSignal<Wasm | null>(null);
  const [refreshing, setRefreshing] = createSignal(false);
  const [unavailable, setUnavailable] = createSignal(false);
  const [search, setSearch] = createSignal<SearchState>({ kind: "idle" });
  const [verified, setVerified] = createSignal<ReadonlySet<string>>(new Set());
  const [gone, setGone] = createSignal<ReadonlySet<string>>(new Set());
  const [details, setDetails] = createSignal<ReadonlyMap<string, DirectoryItem>>(new Map());
  const [notice, setNotice] = createLiveText();

  const account = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  const topics = createMemo(() => {
    const w = wasm();
    if (w === null) return [];
    return [...w.channelTopicIds()].map((id) => ({ id, name: topicLabel(id) }));
  });
  const topicName = (id: number): string => topicLabel(id);
  const language = (): string => prefs().language ?? appLanguage(navigator.language);

  async function persist(next: DiscoverPrefs): Promise<void> {
    setPrefs(next);
    const a = account();
    if (a === null) return;
    const key = await getDeviceKey(a);
    if (key === null) return;
    await saveDiscoverPrefs(a, key, next).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[discover] saving failed", err instanceof Error ? err.name : "unknown");
    });
  }

  onMount(() => {
    void (async (): Promise<void> => {
      setWasm(await loadWasm());
      const a = account();
      if (a !== null) {
        const key = await getDeviceKey(a);
        if (key !== null) setPrefs(await loadDiscoverPrefs(a, key).catch(() => defaultDiscoverPrefs()));
      }
      setLoaded(true);
    })();
  });

  /** A forced refresh (new picks) asked while one was running. */
  let forcedAgain = false;

  async function refresh(force = false): Promise<void> {
    const w = wasm();
    const p = prefs();
    if (refreshing()) {
      // The run in flight was planned from the picks it started with; picks
      // saved since need a run of their own, which follows it.
      if (force) forcedAgain = true;
      return;
    }
    if (w === null || p.topics.length === 0) return;
    // Whose preferences these are. `persist` writes under the account that is
    // signed in when it runs.
    const startedFor = account();
    const lang = language();
    const now = Date.now();
    const key = pickedKey(p.topics, lang);
    if (!force && !refreshDue(p.lastRefreshMs, p.lastFetchedKey, key, now)) return;
    setRefreshing(true);
    try {
      const { draw } = currentDecoys(p.decoys, topics().map((t) => t.id), lang, now, Math.random);
      const rounds = planBucketRounds(p.topics, draw, p.mixRandom, Math.random);
      const picked = new Set(p.topics);
      const buckets: Record<string, CachedBucket> = {};
      let failures = 0;
      let asked = 0;
      for (const round of rounds) {
        const results = await Promise.allSettled(round.map((t) => fetchBucket(t, lang)));
        results.forEach((r, i) => {
          const topic = round[i] as number;
          asked += 1;
          if (r.status === "rejected") {
            failures += 1;
            return;
          }
          // A decoy's answer is discarded: only picked buckets are kept.
          if (picked.has(topic)) buckets[bucketKey(topic, lang)] = { fetchedAtMs: now, items: r.value };
        });
      }
      if (account() !== startedFor) return;
      if (asked > 0 && failures === asked) {
        setUnavailable(true);
        await persist(withRefreshOutcome(prefs(), { decoys: draw }));
        return;
      }
      setUnavailable(false);
      // Keep a previously cached picked bucket this round failed to refresh.
      for (const t of picked) {
        const k = bucketKey(t, lang);
        const old = p.buckets[k];
        if (buckets[k] === undefined && old !== undefined) buckets[k] = old;
      }
      // Into the preferences as they are now, not as they were when the
      // requests went out (`withRefreshOutcome`).
      await persist(
        withRefreshOutcome(prefs(), {
          decoys: draw,
          fetched: { buckets, lastRefreshMs: now, lastFetchedKey: key },
        }),
      );
    } finally {
      setRefreshing(false);
      if (forcedAgain) {
        forcedAgain = false;
        if (account() === startedFor) void refresh(true);
      }
    }
  }

  const ctx = (): CardContext => {
    const p = prefs();
    return {
      pickedTopics: new Set(p.topics),
      language: language(),
      notInterested: new Set(p.notInterested),
      hiddenTopics: new Set(p.hiddenTopics),
      following: new Set(channelsState.channels().map((c) => c.id)),
      gone: gone(),
    };
  };

  const groupCards = (): DirectoryCard[] => {
    const shared = aggregateShared(deps.sharedRow(), deps.ownKeyB64(), deps.liveGroups());
    const extra = details();
    return shared.map((s) => {
      let best: { g: string; keys: readonly string[] } | null = null;
      for (const [g, keys] of s.byGroup) {
        if (best === null || keys.length > best.keys.length) best = { g, keys };
      }
      const names = (best?.keys ?? [])
        .map((k) => deps.contactName(k))
        .filter((n): n is string => n !== null);
      const listing = extra.get(s.channelIdHex);
      const decoded = listing === undefined ? null : decodeListingB64(wasmOrThrow(), listing.listing_b64);
      return {
        channelIdHex: s.channelIdHex,
        handle: s.handle,
        name: s.name,
        description: decoded?.description ?? null,
        topic: decoded?.topic ?? null,
        language: decoded?.language ?? null,
        band: listing?.subscribers ?? null,
        source: "group",
        serverRank: 1000,
        followers:
          best === null
            ? null
            : { groupIdB64: best.g, groupTitle: deps.groupTitle(best.g), count: best.keys.length, contactNames: names },
      } satisfies DirectoryCard;
    });
  };

  function wasmOrThrow(): ListingDecoder {
    const w = wasm();
    if (w === null) throw new Error("wasm not loaded");
    return w;
  }

  const cards = createMemo((): readonly DirectoryCard[] => {
    const w = wasm();
    if (w === null) return [];
    const p = prefs();
    const lang = language();
    const fromBuckets: DirectoryCard[] = [];
    for (const t of p.topics) {
      const b = p.buckets[bucketKey(t, lang)];
      if (b !== undefined) fromBuckets.push(...cardsFromItems(b.items, "bucket", (x) => decodeListingB64(w, x)));
    }
    const hidden = deps.hiddenFromFeeds?.() ?? new Set<string>();
    return rankCards(cardsWithoutHidden([...fromBuckets, ...groupCards()], hidden), ctx());
  });

  /** Bumped by every search and by clearing it: only the newest request may
   *  put its answer on screen. */
  let searchSeq = 0;

  async function runSearch(query: string): Promise<void> {
    const q = query.trim();
    const mine = ++searchSeq;
    if (q.length < 2) {
      setSearch({ kind: "error", message: COPY.searchTooShort });
      return;
    }
    setSearch({ kind: "busy" });
    try {
      const items = await searchDirectory(q.slice(0, 64));
      // Cleared, or searched again, while this one was out.
      if (mine !== searchSeq) return;
      const w = wasmOrThrow();
      const found = rankCards(cardsFromItems(items, "search", (x) => decodeListingB64(w, x)), {
        ...ctx(),
        following: new Set(),
        pickedTopics: new Set(),
      });
      setSearch({ kind: "done", query: q, cards: found });
    } catch (err) {
      if (mine !== searchSeq) return;
      setSearch({ kind: "error", message: () => searchErrorText(err) });
    }
  }

  async function openCard(card: DirectoryCard): Promise<void> {
    if (card.description !== null) return;
    try {
      const item = await fetchListing(card.channelIdHex);
      if (item === null) {
        setGone(new Set([...gone(), card.channelIdHex]));
        setNotice(COPY.gone);
        return;
      }
      const next = new Map(details());
      next.set(card.channelIdHex, item);
      setDetails(next);
    } catch {
      setNotice(COPY.unavailable);
    }
  }

  async function subscribe(card: DirectoryCard): Promise<void> {
    setNotice(null);
    // The handle check is a network round trip; a person who went elsewhere
    // meanwhile is not sent to the channel (`ui/screen-anchor.ts`).
    const here = screenAnchor(location);
    // ADR-0087 sections 6 and 16 — the handle's record names the channel and
    // carries the read key; a listing whose handle answers another id (or
    // none) is gone.
    let found: Awaited<ReturnType<typeof lookupPublicChannel>>;
    try {
      found = await lookupPublicChannel(card.handle);
    } catch (err) {
      if (anonTokenQuotaExhausted(err) !== null) setNotice(LOOKUP_ALLOWANCE_USED);
      else setNotice(COPY.unavailable);
      return;
    }
    if (verifyDecision(card.channelIdHex, found?.idHex ?? null) === "gone" || found === null) {
      setGone(new Set([...gone(), card.channelIdHex]));
      setNotice(COPY.gone);
      return;
    }
    setVerified(new Set([...verified(), card.channelIdHex]));
    try {
      await subscribeToChannel(found.idHex, { kind: "public", readKey: found.readKey }, {
        name: found.name,
        description: found.description,
        handle: found.handle,
      });
    } catch {
      setNotice(COPY.unavailable);
      return;
    }
    if (!here()) return;
    navigate(paths.channel(found.idHex));
  }

  return {
    prefs,
    loaded,
    topics,
    language,
    cards,
    refreshing,
    unavailable,
    search,
    verified,
    notice,
    topicName,
    async savePicks(t, lang) {
      await persist({ ...prefs(), topics: [...new Set(t)], language: lang, introDismissed: true });
      await refresh(true);
    },
    async dismissIntro() {
      await persist({ ...prefs(), introDismissed: true });
    },
    async setMixRandom(on) {
      await persist({ ...prefs(), mixRandom: on });
    },
    async notInterested(id) {
      const list = [...prefs().notInterested.filter((x) => x !== id), id].slice(-MAX_NOT_INTERESTED);
      await persist({ ...prefs(), notInterested: list });
    },
    async hideTopic(topic) {
      await persist({ ...prefs(), hiddenTopics: [...new Set([...prefs().hiddenTopics, topic])] });
    },
    refresh,
    runSearch,
    clearSearch: () => {
      searchSeq += 1;
      setSearch({ kind: "idle" });
    },
    openCard,
    subscribe,
    relation: (id) =>
      cardRelation(
        id,
        new Set(channelsState.keysByChannel().keys()),
        new Set(channelsState.channels().map((c) => c.id)),
      ),
    openChannel: (id) => navigate(paths.channel(id)),
  };
}

/** The line for a failed search (UX spec section 7). */
export function searchErrorText(err: unknown): string {
  if (anonTokenQuotaExhausted(err) !== null) return COPY.rateLimited();
  if (apiError.is(err) && err.status === 429) return COPY.rateLimited();
  if (isAnonTokenNetworkFailure(err)) return COPY.unavailable();
  return COPY.unavailable();
}

// ----- Views ---------------------------------------------------------------------

/** One suggestion card. */
export function DiscoverCardView(props: { readonly card: DirectoryCard; readonly model: DiscoverModel }): JSX.Element {
  const [busy, setBusy] = createSignal(false);
  const c = (): DirectoryCard => props.card;
  const relation = (): CardRelation => props.model.relation(c().channelIdHex);
  return (
    <article class="lk-feed-item lk-discover-card" aria-label={t("discover.card.a11y", { name: c().name })}>
      <div class="lk-discover-card-body">
        <div class="lk-discover-card-head">
          <span class="lk-discover-avatar" aria-hidden="true">
            {initialsOf(c().name, "#")}
          </span>
          <div class="lk-discover-card-title">
            <strong>{c().name}</strong>
            {/* The handle is the owner's (or a member's) claim until the
                lookup before subscribing checks it: plain text, never a
                link (ADR-0059 section 3). */}
            <span class="lk-discover-handle">@{c().handle}</span>
          </div>
        </div>
        <Show
          when={c().description !== null}
          fallback={
            <button type="button" class="ghost lk-discover-more" onClick={() => void props.model.openCard(c())}>
              {t("discover.card.show_description")}
            </button>
          }
        >
          <Show when={c().description !== ""}>
            <p class="lk-discover-description">{c().description}</p>
          </Show>
        </Show>
        <div class="lk-discover-facts">
          <Show when={c().topic !== null}>
            <span>{props.model.topicName(c().topic as number)}</span>
          </Show>
          <Show when={c().language !== null && c().language !== ""}>
            <span>{languageLabel(c().language as string)}</span>
          </Show>
          <Show when={c().band !== null}>
            <span>{t("discover.card.subscribers", { band: c().band as string })}</span>
          </Show>
        </div>
        <div class="lk-feed-why">
          {relation() === "own"
            ? t("channels.row.own")
            : relation() === "following"
              ? t("discover.card.following")
              : reasonLine(c(), (id) => props.model.topicName(id))}
        </div>
      </div>
      <Show
        when={relation() === "none"}
        fallback={
          <div class="lk-discover-actions">
            <button type="button" class="primary" onClick={() => props.model.openChannel(c().channelIdHex)}>
              {t("common.open")}
            </button>
          </div>
        }
      >
      <div class="lk-discover-actions">
        <button
          type="button"
          class="primary"
          disabled={busy()}
          onClick={() => {
            setBusy(true);
            void props.model.subscribe(c()).finally(() => setBusy(false));
          }}
        >
          {t("channels.subscribe.action")}
        </button>
        <button type="button" class="ghost" onClick={() => void props.model.notInterested(c().channelIdHex)}>
          {t("discover.card.not_interested")}
        </button>
        <Show when={c().topic !== null}>
          <button type="button" class="ghost" onClick={() => void props.model.hideTopic(c().topic as number)}>
            {t("discover.card.hide_topic")}
          </button>
        </Show>
      </div>
      </Show>
    </article>
  );
}

/** The pick-topics card (first open, and "Edit topics"). */
function PickTopics(props: { readonly model: DiscoverModel; readonly onDone: () => void; readonly first: boolean }): JSX.Element {
  const m = props.model;
  const [chosen, setChosen] = createSignal<ReadonlySet<number>>(new Set(m.prefs().topics));
  const [lang, setLang] = createSignal(m.language());
  const toggle = (id: number): void => {
    const next = new Set(chosen());
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setChosen(next);
  };
  return (
    <section class="lk-card lk-discover-pick" aria-label={t("discover.pick.a11y")}>
      <h2>{COPY.pickTitle()}</h2>
      <p class="lk-card-desc">{COPY.pickBody()}</p>
      <div class="lk-discover-chips">
        <For each={m.topics()}>
          {(topic) => (
            <button
              type="button"
              class="lk-feed-chip"
              aria-pressed={chosen().has(topic.id)}
              aria-selected={chosen().has(topic.id)}
              onClick={() => toggle(topic.id)}
            >
              {topic.name}
            </button>
          )}
        </For>
      </div>
      <div class="lk-picker-field">
        <span id="lk-discover-pick-language">{t("discover.language")}</span>
        <LakkoPicker
          labelledBy="lk-discover-pick-language"
          value={lang()}
          options={languageOptions(lang()).map((code) => ({ value: code, label: languageLabel(code) }))}
          onChange={(code): void => {
            setLang(code);
          }}
        />
      </div>
      <div class="lk-actions lk-row-end">
        <button
          type="button"
          class="ghost"
          onClick={() => {
            if (props.first) void m.dismissIntro();
            props.onDone();
          }}
        >
          {props.first ? t("common.not_now") : t("common.cancel")}
        </button>
        <button
          type="button"
          class="primary"
          onClick={() => {
            void m.savePicks([...chosen()], lang());
            props.onDone();
          }}
        >
          {t("common.save")}
        </button>
      </div>
    </section>
  );
}

/** The Discover preset of the Feed. */
export function DiscoverView(props: { readonly model: DiscoverModel }): JSX.Element {
  const m = props.model;
  const [editing, setEditing] = createSignal(false);
  const [query, setQuery] = createSignal("");
  onMount(() => {
    void m.refresh();
  });
  const showIntro = (): boolean =>
    m.loaded() && (editing() || (m.prefs().topics.length === 0 && !m.prefs().introDismissed));

  return (
    <div class="lk-discover">
      <form
        class="lk-discover-search"
        role="search"
        onSubmit={(e) => {
          e.preventDefault();
          void m.runSearch(query());
        }}
      >
        <input
          type="search"
          placeholder={t("discover.search.placeholder")}
          aria-label={t("discover.search.placeholder")}
          value={query()}
          maxLength={64}
          disabled={m.unavailable()}
          onInput={(e) => {
            setQuery(e.currentTarget.value);
            if (e.currentTarget.value === "") m.clearSearch();
          }}
        />
        <button type="submit" class="ghost" disabled={m.unavailable() || m.search().kind === "busy"}>
          {t("discover.search.action")}
        </button>
      </form>
      <Show when={m.unavailable()}>
        <p class="lk-feed-hint">{COPY.unavailable()}</p>
      </Show>

      <Show when={m.notice()}>
        {(msg) => (
          <div class="lk-feed-banner" role="status">
            <span>{msg()}</span>
          </div>
        )}
      </Show>

      <Show
        when={m.search().kind === "idle"}
        fallback={
          <Show when={m.search()} keyed>
            {(s) => (
              <Show
                when={s.kind === "done" ? s : null}
                fallback={
                  <p class="lk-feed-hint" role="status">
                    {s.kind === "busy" ? t("discover.search.searching") : s.kind === "error" ? s.message() : ""}
                  </p>
                }
              >
                {(done) => (
                  <Show when={done().cards.length > 0} fallback={<div class="lk-empty">{COPY.noResults()}</div>}>
                    <For each={done().cards}>{(c) => <DiscoverCardView card={c} model={m} />}</For>
                  </Show>
                )}
              </Show>
            )}
          </Show>
        }
      >
        <Show when={showIntro()}>
          <PickTopics model={m} first={!editing()} onDone={() => setEditing(false)} />
        </Show>
        <Show when={!showIntro()}>
          <details class="lk-discover-menu">
            <summary>{t("discover.settings")}</summary>
            <button type="button" class="ghost" onClick={() => setEditing(true)}>
              {t("discover.edit_topics")}
            </button>
            <label class="lk-row">
              <input
                type="checkbox"
                role="switch"
                checked={m.prefs().mixRandom}
                onChange={(e) => void m.setMixRandom(e.currentTarget.checked)}
              />
              <span>{COPY.mixLabel()}</span>
            </label>
          </details>
        </Show>
        <Show when={m.refreshing() && m.cards().length === 0}>
          <div class="lk-feed-skeleton" aria-busy="true" aria-label={t("common.loading")}>
            <div />
            <div />
          </div>
        </Show>
        <Show
          when={m.cards().length > 0}
          fallback={
            <Show when={!m.refreshing() && !showIntro()}>
              <div class="lk-empty">{m.unavailable() ? COPY.unavailable() : COPY.nothingYet()}</div>
            </Show>
          }
        >
          <For each={m.cards()}>{(c) => <DiscoverCardView card={c} model={m} />}</For>
        </Show>
      </Show>
    </div>
  );
}
