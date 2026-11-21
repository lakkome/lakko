// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 / UX spec §4–§5 — one picker, two tabs: Emoji and GIF.
 *
 * Composer mode: anchored above the composer; an emoji is inserted at the
 * caret and the picker stays open; a GIF is sent immediately and the picker
 * closes. Reaction mode: Emoji tab only; a tap reacts and closes.
 *
 * The emoji data is bundled (`emoji-data.json`) — this component never
 * fetches emoji. GIF search and previews go through Lakko's proxy only
 * (`gif-client.ts`); previews are fetched with the session token and shown
 * from object URLs this component revokes when it closes.
 */

import { createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";

import rawEmoji from "./emoji/emoji-data.json";
import {
  buildDataset,
  displayEmoji,
  loadRecent,
  loadRecentReactions,
  loadTonePrefs,
  pushRecent,
  saveRecent,
  saveRecentReactions,
  saveTonePrefs,
  searchEmoji,
  withTonePref,
  type EmojiEntry,
  type TonePrefs,
} from "./emoji/emoji-index";
import {
  GIF_ATTRIBUTION,
  GIF_NO_RESULTS,
  GIF_RATE_LIMITED,
  GIF_SEARCH_DEBOUNCE_MS,
  GIF_UNAVAILABLE,
  MAX_GIF_PREVIEW_BYTES,
  createDebouncer,
  fetchGifMedia,
  fetchGifPage,
  type GifItem,
} from "./gif-client";
import { gifProviderMissing, setGifProviderMissing } from "./media-prefs";
import { ObjectUrlRegistry } from "./object-url-registry";
import { t } from "../l10n";
import { paintVideoBehind } from "./video-overlay";

/** Whether this page session has asked the proxy if a GIF provider is
 *  configured. Asked once, when the composer picker first opens, so an
 *  unconfigured server's GIF tab is hidden before anyone reaches for it
 *  (UX spec §5) rather than disappearing on the first tap. */
let gifProviderProbed = false;

let datasetCache: ReturnType<typeof buildDataset> | null = null;
function dataset(): ReturnType<typeof buildDataset> {
  datasetCache ??= buildDataset(rawEmoji);
  return datasetCache;
}

/**
 * `docs/ux/reactions-and-emoji.md` section 13 — the `:` suggestions: up to
 * eight emoji matching `word` (the same search as the picker), the person's
 * Recent ones first, each in the tone they chose.
 */
export function emojiSuggestions(word: string, max = 8): string[] {
  const ds = dataset();
  const found = searchEmoji(ds, word, 64);
  if (found.length === 0) return [];
  const tones = loadTonePrefs();
  const recent = loadRecent();
  const glyphs = found.map((e) => displayEmoji(e, tones));
  const rank = (g: string): number => {
    const i = recent.indexOf(g);
    return i < 0 ? recent.length : i;
  };
  return glyphs
    .map((g, i) => ({ g, i }))
    .sort((a, b) => rank(a.g) - rank(b.g) || a.i - b.i)
    .map((x) => x.g)
    .slice(0, max);
}

/** One representative glyph per group for the jump strip. */
const GROUP_ICONS = ["😀", "👋", "🐻", "🍔", "✈️", "⚽", "💡", "❤️", "🏁"];

/** The emoji category names, in the order of the bundled data's groups
 *  (Unicode's own order); an unknown group falls back to the data's name. */
const GROUP_KEYS = [
  "chat.emoji.group.smileys",
  "chat.emoji.group.people",
  "chat.emoji.group.animals",
  "chat.emoji.group.food",
  "chat.emoji.group.travel",
  "chat.emoji.group.activities",
  "chat.emoji.group.objects",
  "chat.emoji.group.symbols",
  "chat.emoji.group.flags",
];
const groupName = (index: number, fallback: string): string => {
  const key = GROUP_KEYS[index];
  return key === undefined ? fallback : t(key);
};

export interface EmojiPickerProps {
  mode: "compose" | "reaction";
  onEmoji: (emoji: string) => void;
  onGif?: (item: GifItem) => void;
  onClose: () => void;
  sessionToken: () => string | null;
  /** Which edge of the anchor the popover lines up with. */
  align?: "left" | "right";
  /** Reaction mode in a conversation that takes only a list
   *  (`docs/ux/reactions-and-emoji.md` section 5): one grid of these, in the
   *  owner's order, the search kept, no categories. */
  allowedOnly?: readonly string[] | undefined;
  /** An emoji drawn dimmed that does nothing (the three-reactions cap, the
   *  kind limit, the hundred-emoji limit of the setting screen). */
  isDisabled?: (emoji: string) => boolean;
  /** One line above the grid (the three-reactions line, the 100 line). */
  notice?: string | undefined;
  /** Stay open after a pick (the allowed-reactions screen adds and stays
   *  open). */
  stayOpen?: boolean;
}

export function EmojiPicker(props: EmojiPickerProps): JSX.Element {
  const [tab, setTab] = createSignal<"emoji" | "gif">("emoji");
  let root: HTMLDivElement | undefined;

  const onKey = (e: KeyboardEvent): void => {
    if (e.key === "Escape") {
      e.stopPropagation();
      props.onClose();
    }
  };
  const onPointerDownOutside = (e: PointerEvent): void => {
    if (root !== undefined && e.target instanceof Node && !root.contains(e.target)) {
      // The toggle button that opened the picker handles its own click.
      if (e.target instanceof Element && e.target.closest("[data-picker-toggle]") !== null) return;
      props.onClose();
    }
  };
  onMount(() => {
    const token = props.sessionToken();
    if (props.mode === "compose" && props.onGif !== undefined && token !== null && !gifProviderProbed) {
      gifProviderProbed = true;
      // Trending carries no search term; only "not configured" hides the tab.
      // Any other answer (including an outage) leaves it for the tab's own
      // Retry state, and lets the next open ask again.
      void fetchGifPage("", 0, token).then((r) => {
        if (r.kind === "not-configured") setGifProviderMissing(true);
        else if (r.kind !== "ok") gifProviderProbed = false;
      });
    }
    globalThis.document?.addEventListener("keydown", onKey, true);
    globalThis.document?.addEventListener("pointerdown", onPointerDownOutside, true);
  });
  onCleanup(() => {
    globalThis.document?.removeEventListener("keydown", onKey, true);
    globalThis.document?.removeEventListener("pointerdown", onPointerDownOutside, true);
  });

  const gifAvailable = (): boolean =>
    props.mode === "compose" && props.onGif !== undefined && !gifProviderMissing();

  return (
    <div
      ref={root}
      class="lakko-picker"
      data-mode={props.mode}
      style={props.align === "right" ? { left: "auto", right: 0 } : undefined}
      role="dialog"
      aria-label={
        props.mode === "reaction"
          ? t("chat.emoji.choose_reaction")
          : gifAvailable()
            ? t("chat.composer.emoji_gifs")
            : t("chat.emoji.tab")
      }
    >
      <Show when={gifAvailable()}>
        <div class="lakko-picker-tabs" role="tablist">
          <button type="button" role="tab" aria-selected={tab() === "emoji"} onClick={() => setTab("emoji")}>
            {t("chat.emoji.tab")}
          </button>
          <button type="button" role="tab" aria-selected={tab() === "gif"} onClick={() => setTab("gif")}>
            {t("chat.media.gif")}
          </button>
        </div>
      </Show>
      <Show when={tab() === "gif" && gifAvailable()} fallback={<EmojiTab {...props} />}>
        <GifTab
          sessionToken={props.sessionToken}
          onPick={(item) => {
            props.onGif?.(item);
            props.onClose();
          }}
          onNotConfigured={() => {
            setGifProviderMissing(true);
            setTab("emoji");
          }}
        />
      </Show>
    </div>
  );
}

// ----- emoji tab -------------------------------------------------------------

function EmojiTab(props: EmojiPickerProps): JSX.Element {
  const ds = dataset();
  const [query, setQuery] = createSignal("");
  const [recent, setRecent] = createSignal<string[]>(loadRecent());
  const [tones, setTones] = createSignal<TonePrefs>(loadTonePrefs());
  const [toneFor, setToneFor] = createSignal<{ entry: EmojiEntry; x: number; y: number } | null>(null);
  const allowedOnly = (): readonly string[] | undefined => props.allowedOnly;
  const results = createMemo(() => {
    const only = allowedOnly();
    const found = searchEmoji(ds, query());
    if (only === undefined) return found;
    const allowed = new Set(only);
    return found.filter((e) => allowed.has(e.emoji) || (e.tones ?? []).some((tn) => allowed.has(tn)));
  });
  const byGroup = createMemo(() => {
    const groups: EmojiEntry[][] = ds.groups.map(() => []);
    for (const e of ds.entries) groups[e.group]?.push(e);
    return groups;
  });
  let body: HTMLDivElement | undefined;
  const headings: (HTMLHeadingElement | undefined)[] = [];

  function choose(emoji: string, entry: EmojiEntry | undefined): void {
    if (props.isDisabled?.(emoji) === true) return;
    const next = pushRecent(recent(), emoji);
    setRecent(next);
    saveRecent(next);
    // Section 5 — the reaction row reads the person's recent REACTIONS.
    if (props.mode === "reaction") saveRecentReactions(pushRecent(loadRecentReactions(), emoji));
    if (entry !== undefined && entry.tones !== undefined) {
      const prefs = withTonePref(tones(), entry, emoji);
      setTones(prefs);
      saveTonePrefs(prefs);
    }
    setToneFor(null);
    props.onEmoji(emoji);
    if (props.mode === "reaction" && props.stayOpen !== true) props.onClose();
  }

  const cell = (entry: EmojiEntry | undefined, glyph: string): JSX.Element => (
    <button
      type="button"
      title={entry?.name}
      aria-label={entry?.name ?? glyph}
      // Keep focus (and the caret) in the message field on web (UX spec §4).
      onMouseDown={(e) => {
        if (props.mode === "compose") e.preventDefault();
      }}
      classList={{ dimmed: props.isDisabled?.(glyph) === true }}
      aria-disabled={props.isDisabled?.(glyph) === true ? "true" : undefined}
      onClick={() => choose(glyph, entry)}
      onContextMenu={(e) => {
        if (entry?.tones === undefined) return;
        e.preventDefault();
        const rect = body?.getBoundingClientRect();
        const target = e.currentTarget.getBoundingClientRect();
        setToneFor({
          entry,
          x: target.left - (rect?.left ?? 0),
          y: target.top - (rect?.top ?? 0) + (body?.scrollTop ?? 0) - 44,
        });
      }}
    >
      {glyph}
    </button>
  );

  return (
    <>
      <div class="lakko-picker-search">
        <input
          type="search"
          placeholder={t("chat.emoji.search")}
          aria-label={t("chat.emoji.search")}
          value={query()}
          onInput={(e) => setQuery(e.currentTarget.value)}
          autofocus={props.mode === "reaction"}
        />
      </div>
      <div ref={body} class="lakko-picker-body" style={{ position: "relative" }}>
        <Show when={props.notice}>{(n) => <p class="lakko-picker-notice" role="status">{n()}</p>}</Show>
        <Show
          when={allowedOnly() === undefined}
          fallback={
            <Show
              when={query().trim().length === 0}
              fallback={
                <Show when={results().length > 0} fallback={<p class="lakko-picker-empty">{t("chat.emoji.no_results")}</p>}>
                  <div class="lakko-emoji-grid">
                    <For each={results()}>{(e) => cell(e, e.emoji)}</For>
                  </div>
                </Show>
              }
            >
              <div class="lakko-emoji-grid">
                <For each={allowedOnly() ?? []}>{(glyph) => cell(ds.byEmoji.get(glyph), glyph)}</For>
              </div>
            </Show>
          }
        >
        <Show
          when={query().trim().length === 0}
          fallback={
            <Show when={results().length > 0} fallback={<p class="lakko-picker-empty">{t("chat.emoji.no_results")}</p>}>
              <div class="lakko-emoji-grid">
                <For each={results()}>{(e) => cell(e, displayEmoji(e, tones()))}</For>
              </div>
            </Show>
          }
        >
          <Show when={recent().length > 0}>
            <h3 class="lakko-picker-heading">{t("chat.emoji.recent")}</h3>
            <div class="lakko-emoji-grid">
              <For each={recent()}>{(glyph) => cell(ds.byEmoji.get(glyph), glyph)}</For>
            </div>
          </Show>
          <For each={ds.groups}>
            {(name, gi) => (
              <section style={{ "content-visibility": "auto", "contain-intrinsic-size": "auto 600px" }}>
                <h3 class="lakko-picker-heading" ref={(el) => (headings[gi()] = el)}>
                  {groupName(gi(), name)}
                </h3>
                <div class="lakko-emoji-grid">
                  <For each={byGroup()[gi()] ?? []}>{(e) => cell(e, displayEmoji(e, tones()))}</For>
                </div>
              </section>
            )}
          </For>
        </Show>
        </Show>
        <Show when={toneFor()}>
          {(tone) => (
            <div
              class="lakko-tones"
              role="menu"
              aria-label={t("chat.emoji.skin_tones", { name: tone().entry.name })}
              style={{ left: `${Math.max(0, Math.min(tone().x, 180))}px`, top: `${Math.max(0, tone().y)}px` }}
            >
              <For each={[tone().entry.emoji, ...(tone().entry.tones ?? [])]}>
                {(glyph) => (
                  <button
                    type="button"
                    role="menuitem"
                    onMouseDown={(e) => {
                      if (props.mode === "compose") e.preventDefault();
                    }}
                    onClick={() => choose(glyph, tone().entry)}
                  >
                    {glyph}
                  </button>
                )}
              </For>
            </div>
          )}
        </Show>
      </div>
      <Show when={query().trim().length === 0 && allowedOnly() === undefined}>
        <nav class="lakko-picker-strip" aria-label={t("chat.emoji.categories")}>
          <For each={ds.groups}>
            {(name, gi) => (
              <button
                type="button"
                title={groupName(gi(), name)}
                aria-label={groupName(gi(), name)}
                onMouseDown={(e) => {
                  if (props.mode === "compose") e.preventDefault();
                }}
                onClick={() => {
                  const h = headings[gi()];
                  if (h !== undefined && body !== undefined) body.scrollTop = h.parentElement?.offsetTop ?? 0;
                }}
              >
                {GROUP_ICONS[gi()] ?? "•"}
              </button>
            )}
          </For>
        </nav>
      </Show>
    </>
  );
}

// ----- GIF tab ---------------------------------------------------------------

type GifStatus = "loading" | "ready" | "empty" | "unavailable" | "rate-limited";

function GifTab(props: {
  sessionToken: () => string | null;
  onPick: (item: GifItem) => void;
  onNotConfigured: () => void;
}): JSX.Element {
  const [query, setQuery] = createSignal("");
  const [items, setItems] = createSignal<GifItem[]>([]);
  const [nextOffset, setNextOffset] = createSignal<number | null>(null);
  const [status, setStatus] = createSignal<GifStatus>("loading");
  const [loadingMore, setLoadingMore] = createSignal(false);
  const [previews, setPreviews] = createSignal<ReadonlyMap<string, { url: string; video: boolean }>>(new Map());
  const registry = new ObjectUrlRegistry();
  const debouncer = createDebouncer(GIF_SEARCH_DEBOUNCE_MS);
  let generation = 0;
  let abort = new AbortController();
  let body: HTMLDivElement | undefined;
  let sentinel: HTMLDivElement | undefined;

  // Preview fetches are bounded so a scroll does not burst past the proxy's
  // per-account media rate.
  const MAX_PREVIEW_FETCHES = 6;
  let activePreviewFetches = 0;
  const previewQueue: GifItem[] = [];
  const requested = new Set<string>();

  function pumpPreviews(): void {
    const token = props.sessionToken();
    if (token === null) return;
    while (activePreviewFetches < MAX_PREVIEW_FETCHES && previewQueue.length > 0) {
      const item = previewQueue.shift();
      if (item === undefined) break;
      activePreviewFetches += 1;
      const gen = generation;
      void fetchGifMedia(item.previewUrl, token, MAX_GIF_PREVIEW_BYTES, undefined, abort.signal)
        .then((r) => {
          if (gen !== generation || r.kind !== "ok") return;
          const url = registry.set(item.id, new Blob([r.value.bytes.slice()], { type: r.value.mime }));
          if (url === null) return;
          const next = new Map(previews());
          next.set(item.id, { url, video: r.value.mime === "video/mp4" });
          setPreviews(next);
        })
        .finally(() => {
          activePreviewFetches -= 1;
          pumpPreviews();
        });
    }
  }

  const tileObserver =
    typeof IntersectionObserver === "undefined"
      ? null
      : new IntersectionObserver(
          (entries) => {
            for (const e of entries) {
              if (!e.isIntersecting) continue;
              const id = (e.target as HTMLElement).dataset.gifId;
              const item = items().find((i) => i.id === id);
              if (item !== undefined && !requested.has(item.id)) {
                requested.add(item.id);
                previewQueue.push(item);
              }
              tileObserver?.unobserve(e.target);
            }
            pumpPreviews();
          },
          { rootMargin: "120px" },
        );

  async function loadPage(reset: boolean): Promise<void> {
    const token = props.sessionToken();
    if (token === null) {
      setStatus("unavailable");
      return;
    }
    if (reset) {
      generation += 1;
      abort.abort();
      abort = new AbortController();
      previewQueue.length = 0;
      requested.clear();
      registry.revokeAll();
      setPreviews(new Map());
      setItems([]);
      setNextOffset(null);
      // A "load more" of the previous query is abandoned by the generation
      // bump above and returns without touching anything, so its busy flag
      // is cleared here; left set, the new results could never load a second
      // page.
      setLoadingMore(false);
      setStatus("loading");
      body?.scrollTo({ top: 0 });
    } else {
      if (loadingMore() || nextOffset() === null) return;
      setLoadingMore(true);
    }
    const gen = generation;
    const r = await fetchGifPage(query(), reset ? 0 : (nextOffset() ?? 0), token, undefined, abort.signal);
    if (gen !== generation) return;
    setLoadingMore(false);
    switch (r.kind) {
      case "ok": {
        const merged = reset ? [...r.value.items] : [...items(), ...r.value.items];
        setItems(merged);
        setNextOffset(r.value.nextOffset);
        setStatus(merged.length === 0 ? "empty" : "ready");
        return;
      }
      case "not-configured":
        props.onNotConfigured();
        return;
      case "rate-limited":
        if (reset) setStatus("rate-limited");
        return;
      default:
        if (reset) setStatus("unavailable");
    }
  }

  onMount(() => {
    void loadPage(true);
    if (typeof IntersectionObserver !== "undefined" && sentinel !== undefined) {
      const more = new IntersectionObserver((entries) => {
        if (entries.some((e) => e.isIntersecting) && status() === "ready") void loadPage(false);
      });
      more.observe(sentinel);
      onCleanup(() => more.disconnect());
    }
  });

  onCleanup(() => {
    generation += 1;
    abort.abort();
    debouncer.cancel();
    tileObserver?.disconnect();
    registry.dispose();
  });

  return (
    <>
      <div class="lakko-picker-search">
        <input
          type="search"
          placeholder={t("chat.gif.search")}
          aria-label={t("chat.gif.search")}
          value={query()}
          maxLength={100}
          onInput={(e) => {
            setQuery(e.currentTarget.value);
            debouncer.schedule(() => void loadPage(true));
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              debouncer.flush();
            }
          }}
        />
      </div>
      <div ref={body} class="lakko-picker-body" aria-busy={status() === "loading"}>
        <Show when={query().trim().length === 0 && status() === "ready"}>
          <h3 class="lakko-picker-heading">{t("chat.gif.trending")}</h3>
        </Show>
        <Show when={status() === "loading"}>
          <div class="lakko-gif-grid" aria-label={t("chat.gif.loading")}>
            <For each={Array.from({ length: 12 })}>{() => <span class="placeholder" />}</For>
          </div>
        </Show>
        <Show when={status() === "empty"}>
          <p class="lakko-picker-empty">{GIF_NO_RESULTS()}</p>
        </Show>
        <Show when={status() === "unavailable" || status() === "rate-limited"}>
          <div class="lakko-picker-empty" role="status">
            <p style={{ margin: "0 0 0.5rem" }}>{status() === "rate-limited" ? GIF_RATE_LIMITED() : GIF_UNAVAILABLE()}</p>
            <button type="button" onClick={() => void loadPage(true)}>
              {t("common.retry")}
            </button>
          </div>
        </Show>
        <Show when={status() === "ready"}>
          <div class="lakko-gif-grid">
            <For each={items()}>
              {(item) => (
                <button
                  type="button"
                  data-gif-id={item.id}
                  aria-label={t("chat.gif.send")}
                  ref={(el) => queueMicrotask(() => tileObserver?.observe(el))}
                  onClick={() => props.onPick(item)}
                >
                  <Show when={previews().get(item.id)}>
                    {(p) =>
                      p().video ? (
                        <video ref={paintVideoBehind} src={p().url} autoplay muted loop playsinline />
                      ) : (
                        <img src={p().url} alt="" draggable={false} />
                      )
                    }
                  </Show>
                </button>
              )}
            </For>
          </div>
        </Show>
        <div ref={sentinel} style={{ height: "1px" }} />
        <Show when={loadingMore()}>
          <p class="lakko-picker-empty" style={{ padding: "0.5rem" }}>
            <span class="lakko-spinner" />
          </p>
        </Show>
      </div>
      <div class="lakko-picker-footer">{GIF_ATTRIBUTION()}</div>
    </>
  );
}
