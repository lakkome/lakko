// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Feed destination (`docs/ux/feed.md`, ADR-0058).
 *
 * Messages from the person's groups and posts from their channels, ranked on
 * this device by the one rule engine in the core. What is here: the preset
 * switcher, the items with their reactions and folded replies, opening an
 * item in its chat or channel, the "show new" pill, and the empty and
 * first-run states. Channel posts come from the channel-post log that
 * `ChannelPostSync` fills in the foreground.
 *
 * Seen (ADR-0058 section 5): a group message fully on screen for
 * `FEED_SEEN_DWELL_MS` joins its group's seen set; when that makes the set
 * contiguous further than before, the group gets a read receipt and the
 * other devices a read cursor at the new watermark, never past a message
 * the person did not see. Channel posts are not marked: channels send no
 * receipt, and the channel-post log has no seen set yet.
 *
 * The person's own feeds (UX spec section 4.1): "New feed" opens the
 * editor, a saved feed sits in the switcher after the presets with Edit and
 * Share beside it, and sharing gives a link that carries the rule in its
 * fragment plus a `.lakkofeed` file; both leave out specific conversations.
 * Saved feeds live on this device (`feed/feed-store.ts`).
 *
 * Discover (ADR-0059, UX spec section 6) is the fourth chip: channel
 * suggestions instead of items (`discover/Discover.tsx`). A saved feed whose
 * rule asks for suggestions ("rarely" / "sometimes") mixes the same cards
 * in between its items (`discover/cards.ts` `mixSuggestions`).
 *
 * Not yet here: the "New" divider, unpinning a preset, and the item menu.
 */

import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { composeReadReceiptEnvelope } from "../chat/attachments";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
import { ChatClient } from "../chat/client";
import { readIsActive } from "../chat/membership-ended";
import { conversationKindOf } from "../chat/conversation-kind";
import { conversationTitle } from "../chat/conversation-title";
import { groupTitle } from "../chat/group-title";
import { useGroupNames } from "../chat/GroupNameContext";
import { formatListTime } from "../chat/list-time";
import { loadMessageLog, type PersistedGroup } from "../chat/message-log-store";
import { subscribeMessageLogChanged } from "../chat/message-log-events";
import { ownPersonKeyB64, peopleCountOf, personKeysOf } from "../chat/person-key";
import { loadAccepted, subscribeAcceptedChanged } from "../contacts/local-store";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
import { channelTitle } from "../channels/channel-row";
import { channelsState } from "../state/channels-state";
import type { ChannelPostSlot } from "../feed/channel-post-log";
import { loadChannelPostLog, subscribeChannelPostLogChanged } from "../feed/channel-post-log-store";
import { attachmentLine, FEED_PRESETS, repliesLabel, whyLine, type FeedPresetKey } from "../feed/feed-copy";
import { buildFeedInput, reactionSummary, type FeedCandidateRef, type FeedSource } from "../feed/feed-model";
import { rankFeedInput, type FeedCore, type RankedItem } from "../feed/feed-ranking";
import { FEED_SEEN_DWELL_MS, isSeenInSlot, readCursorBytes } from "../feed/feed-seen";
import {
  blankDraft,
  decodeDraft,
  encodeDraft,
  exportDraft,
  ruleFromDraft,
  shareFileName,
  shareLink,
  type FeedDraft,
  type FeedDraftCore,
} from "../feed/feed-draft";
import { FeedEditor, type PreviewLine } from "../feed/FeedEditor";
import {
  loadFeedState,
  newFeedId,
  removeFeed,
  saveFeedState,
  upsertFeed,
  type FeedState,
} from "../feed/feed-store";
import { recordFeedSeen } from "../feed/feed-seen-writer";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { Icon } from "../ui/icons";
import { mixSuggestions } from "../discover/cards";
import { createDiscoverModel, DiscoverCardView, DiscoverView } from "../discover/Discover";
import { useSharedChannels } from "../discover/SharedChannelsContext";
import { loadWasm } from "../wasm";
import { paths } from "./paths";

const PRESET_STORAGE_KEY = "lakko.feed.preset";
const BANNER_STORAGE_KEY = "lakko.feed.device-banner-dismissed";
/** Within this many pixels of the top, new items are shown at once;
 *  further down they wait behind the pill so the list does not move under
 *  the reader. */
const TOP_SLACK_PX = 40;

function readStorage(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStorage(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // A convenience only; the feed works without it.
  }
}

/** The selected feed: a preset key, Discover (ADR-0059), or `custom:<id>`
 *  for a saved feed. */
type Selection = FeedPresetKey | "discover" | `custom:${string}`;

/** Where a new saved feed asks to be selected (the import route sets it). */
export const FEED_SELECTION_STORAGE_KEY = PRESET_STORAGE_KEY;

function initialSelection(): Selection {
  const stored = readStorage(PRESET_STORAGE_KEY);
  if (stored !== null && stored.startsWith("custom:")) return stored as Selection;
  if (stored === "discover") return "discover";
  return FEED_PRESETS.some((p) => p.key === stored) ? (stored as FeedPresetKey) : "latest";
}

/** A saved feed as the switcher holds it. */
interface SavedView {
  readonly id: string;
  readonly draft: FeedDraft;
}

/** How many items the editor's preview shows. */
const PREVIEW_ITEMS = 8;

function itemKey(item: RankedItem): string {
  return `${item.ref.source.kind}:${item.ref.source.idHex}:${item.ref.message.messageId}`;
}

export default function Feed(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const sync = useDeviceGroupSync();
  const navigate = useNavigate();

  const [core, setCore] = createSignal<(FeedCore & FeedDraftCore) | null>(null);
  onMount(() => {
    void loadWasm().then((w) => setCore(() => w));
  });

  const [selection, setSelection] = createSignal<Selection>(initialSelection());
  const choose = (key: Selection): void => {
    setSelection(key);
    writeStorage(PRESET_STORAGE_KEY, key);
  };

  // ----- Saved feeds (UX spec section 4.1) ---------------------------------

  const [saved, setSaved] = createSignal<readonly SavedView[]>([]);
  let savedState: FeedState = { feeds: [] };
  async function reloadSaved(): Promise<void> {
    const account = auth.state();
    const w = await loadWasm();
    if (account.kind !== "session") return;
    const deviceKey = await getDeviceKey(account.account.accountId);
    if (deviceKey === null) return;
    savedState = await loadFeedState(account.account.accountId, deviceKey);
    const views: SavedView[] = [];
    for (const f of savedState.feeds) {
      try {
        views.push({ id: f.id, draft: decodeDraft(w, f.rule) });
      } catch {
        // A stored rule the core refuses is not shown.
      }
    }
    setSaved(views);
    const sel = selection();
    if (sel.startsWith("custom:") && !views.some((v) => `custom:${v.id}` === sel)) choose("latest");
  }
  onMount(() => {
    void reloadSaved().catch(() => undefined);
  });

  async function persistSaved(next: FeedState): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session") return;
    const deviceKey = await getDeviceKey(s.account.accountId);
    if (deviceKey === null) throw new Error("This device can't save feeds right now.");
    await saveFeedState(s.account.accountId, deviceKey, next);
    savedState = next;
    await reloadSaved();
  }

  const selectedSaved = (): SavedView | null => {
    const sel = selection();
    if (!sel.startsWith("custom:")) return null;
    return saved().find((v) => `custom:${v.id}` === sel) ?? null;
  };

  /** What the "why" line reads the item under: a saved feed that weighs
   *  reactions reads like Popular, anything else like Latest. */
  const whyPreset = (): FeedPresetKey => {
    const sel = selection();
    if (sel === "discover") return "latest";
    if (!sel.startsWith("custom:")) return sel as FeedPresetKey;
    return (selectedSaved()?.draft.weights.reactions ?? 0) > 0 ? "popular" : "latest";
  };

  type Editing = { readonly mode: "new" } | { readonly mode: "edit"; readonly id: string };
  const [editing, setEditing] = createSignal<Editing | null>(null);
  const [editError, setEditError] = createSignal<string | null>(null);
  const [sharing, setSharing] = createSignal<SavedView | null>(null);
  const [copied, setCopied] = createSignal(false);

  async function saveDraft(draft: FeedDraft): Promise<void> {
    const c = core();
    const e = editing();
    if (c === null || e === null) return;
    setEditError(null);
    try {
      const id = e.mode === "edit" ? e.id : newFeedId();
      await persistSaved(upsertFeed(savedState, { id, rule: encodeDraft(c, draft) }));
      setEditing(null);
      choose(`custom:${id}`);
    } catch (err) {
      setEditError(err instanceof Error ? err.message : "Couldn't save the feed. Try again.");
    }
  }

  async function deleteSaved(id: string): Promise<void> {
    setEditError(null);
    try {
      await persistSaved(removeFeed(savedState, id));
      setEditing(null);
      choose("latest");
    } catch (err) {
      setEditError(err instanceof Error ? err.message : "Couldn't delete the feed. Try again.");
    }
  }

  const [bannerDismissed, setBannerDismissed] = createSignal(
    readStorage(BANNER_STORAGE_KEY) === "1",
  );

  // The message log, reloaded on every write (the open chat, the
  // background receive, another tab), as the conversation list does.
  const [log, setLog] = createSignal<{ readonly [hex: string]: PersistedGroup } | null>(null);
  async function reloadLog(): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session") return;
    const deviceKey = await getDeviceKey(s.account.accountId);
    if (deviceKey === null) return;
    const payload = await loadMessageLog(s.account.accountId, deviceKey);
    setLog(payload?.groups ?? {});
  }
  onMount(() => {
    void reloadLog();
    const unsubscribe = subscribeMessageLogChanged((account) => {
      const s = auth.state();
      if (s.kind === "session" && s.account.accountId === account) void reloadLog();
    });
    onCleanup(unsubscribe);
  });

  // The channel-post log, reloaded whenever the foreground sync files new
  // posts into it.
  const [channelPosts, setChannelPosts] = createSignal<{
    readonly [hex: string]: ChannelPostSlot;
  } | null>(null);
  async function reloadChannelPosts(): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session") return;
    const deviceKey = await getDeviceKey(s.account.accountId);
    if (deviceKey === null) return;
    const log = await loadChannelPostLog(s.account.accountId, deviceKey);
    setChannelPosts(log?.channels ?? {});
  }
  onMount(() => {
    void reloadChannelPosts().catch(() => setChannelPosts({}));
    const unsubscribe = subscribeChannelPostLogChanged((account) => {
      const s = auth.state();
      if (s.kind === "session" && s.account.accountId === account) void reloadChannelPosts();
    });
    onCleanup(unsubscribe);
  });

  const [contactVersion, setContactVersion] = createSignal(0);
  onMount(() => {
    const offContacts = subscribeAcceptedChanged(() => setContactVersion((v) => v + 1));
    onCleanup(offContacts);
  });

  // Recency moves with the clock; a minute is fine-grained enough for a
  // ranking whose shortest half-life is six hours.
  const [clock, setClock] = createSignal(Date.now());
  onMount(() => {
    const t = setInterval(() => setClock(Date.now()), 60_000);
    onCleanup(() => clearInterval(t));
  });

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  /** Groups only: a one-to-one is never in the feed (UX spec section 3). */
  const isGroup = (g: ActiveGroup): boolean => conversationKindOf(g.group) === "group";

  /** The group's name exactly as the conversation list shows it. */
  const titleOf = (g: ActiveGroup): string =>
    conversationTitle({
      kind: "group",
      memberKeysB64: personKeysOf(g.group),
      ownKeyB64: ownPersonKeyB64(chats.identity()),
      fallbackLabel: g.label,
      sharedName: groupNames.nameOf(g.groupIdB64),
      contactNameById: undefined,
      contactNameByMember: undefined,
      nameOf: (k) => profiles.resolve(k).name,
      groupTitle,
    });

  const feedSources = createMemo((): FeedSource[] => {
    const flags = sync.flags();
    const groups = chats
      .groups()
      .filter(isGroup)
      .map((g): FeedSource => {
        const hex = syncGroupIdHex(g.group.groupId);
        return {
          kind: "group",
          idHex: hex,
          slug: base64UrlEncodeNoPad(g.group.groupId),
          title: titleOf(g),
          muted: flags[hex]?.muted === true,
          audience: peopleCountOf(g.group),
        };
      });
    const shared = channelsState.metaByChannel();
    const channels = channelsState.channels().map(
      (c): FeedSource => ({
        kind: "channel",
        idHex: c.id,
        slug: c.id,
        title: channelTitle(shared.get(c.id)?.name, c.localName, c.localHandle),
        muted: flags[c.id]?.muted === true,
        audience: 0,
      }),
    );
    return [...groups, ...channels];
  });

  // ----- Discover (ADR-0059) -----------------------------------------------

  const sharedChannels = useSharedChannels();
  const discover = createDiscoverModel({
    liveGroups: () => new Set(chats.groups().filter(isGroup).map((g) => g.groupIdB64)),
    groupTitle: (groupIdB64) => {
      const g = chats.get(groupIdB64);
      return g === undefined ? "a group" : titleOf(g);
    },
    contactName: (key) => {
      contactVersion();
      if (!loadAccepted().some((a) => a.signaturePubkeyB64 === key)) return null;
      return memberNameOf(profiles.resolve(key));
    },
    ownKeyB64: () => ownPersonKeyB64(chats.identity()),
    sharedRow: () => sharedChannels.row(),
  });

  /** How often the selected feed mixes suggestions in: its rule's field for a
   *  saved feed; the presets never do (their rules say never). */
  const suggestionRate = (): "never" | "rarely" | "sometimes" => selectedSaved()?.draft.suggestions ?? "never";
  createEffect(() => {
    if (suggestionRate() !== "never" && discover.loaded()) void discover.refresh();
  });

  const input = createMemo(() => {
    const c = core();
    const l = log();
    const posts = channelPosts();
    if (c === null || l === null || posts === null) return null;
    contactVersion();
    const s = auth.state();
    const username = s.kind === "session" && s.account.username !== "" ? s.account.username : null;
    return buildFeedInput({
      sources: feedSources(),
      log: l,
      channelPosts: posts,
      ownChannels: new Set(channelsState.keysByChannel().keys()),
      ownUsername: username,
      contactKeysB64: new Set(loadAccepted().map((a) => a.signaturePubkeyB64)),
      mentions: (text, name) => c.mentionsUsername(text, name),
      nowMs: clock(),
    });
  });

  const ranked = createMemo((): RankedItem[] | null => {
    const c = core();
    const i = input();
    if (c === null || i === null) return null;
    const sel = selection();
    if (sel === "discover") return [];
    const custom = selectedSaved();
    let rule;
    if (custom !== null) {
      rule = ruleFromDraft(c, custom.draft);
    } else if (sel.startsWith("custom:")) {
      return null;
    } else {
      const presetEnum = {
        latest: c.FeedPreset.Latest,
        popular: c.FeedPreset.Popular,
        "for-me": c.FeedPreset.ForMe,
      }[sel as FeedPresetKey];
      rule = c.FeedRule.preset(presetEnum);
    }
    try {
      return rankFeedInput(c, rule, i, clock());
    } finally {
      rule.free();
    }
  });

  // What is drawn. New results replace it at once near the top or on a
  // preset change; further down they wait behind "Show N new".
  let listEl: HTMLDivElement | undefined;
  const [shown, setShown] = createSignal<RankedItem[] | null>(null);
  const [pending, setPending] = createSignal<RankedItem[] | null>(null);
  /** Item index -> the card shown after it, for a feed that mixes cards in. */
  const cardAfter = createMemo((): ReadonlyMap<number, number> => {
    const rate = suggestionRate();
    const items = shown();
    const out = new Map<number, number>();
    if (rate === "never" || items === null) return out;
    let last = -1;
    for (const slot of mixSuggestions(items.length, discover.cards().length, rate)) {
      if (slot.kind === "item") last = slot.index;
      else out.set(last, slot.index);
    }
    return out;
  });
  let shownPreset: Selection | null = null;
  const nearTop = (): boolean => (listEl?.scrollTop ?? 0) <= TOP_SLACK_PX;
  createEffect(() => {
    const next = ranked();
    const p = selection();
    if (next === null) return;
    if (shown() === null || shownPreset !== p || nearTop()) {
      shownPreset = p;
      setShown(next);
      setPending(null);
      return;
    }
    setPending(next);
  });
  const newCount = createMemo(() => {
    const next = pending();
    const current = shown();
    if (next === null || current === null) return 0;
    const seen = new Set(current.map(itemKey));
    return next.filter((i) => !seen.has(itemKey(i))).length;
  });
  const showPending = (): void => {
    const next = pending();
    if (next !== null) setShown(next);
    setPending(null);
    listEl?.scrollTo({ top: 0 });
  };
  const onScroll = (): void => {
    if (nearTop() && pending() !== null) {
      setShown(pending());
      setPending(null);
    }
  };

  const authorOf = (ref: FeedCandidateRef): string => {
    if (ref.source.kind === "channel") return ref.source.title;
    if (ref.message.direction === "outgoing") return "You";
    const key = ref.message.authorKeyB64;
    return key === null || key === undefined ? "Member" : memberNameOf(profiles.resolve(key));
  };

  const open = (ref: FeedCandidateRef): void => {
    if (ref.source.kind === "channel") {
      navigate(paths.channel(ref.source.slug));
      return;
    }
    navigate(`${paths.groupChat(ref.source.slug)}?at=${ref.message.messageId}`);
  };

  const [expanded, setExpanded] = createSignal<ReadonlySet<string>>(new Set());
  const toggleReplies = (key: string): void => {
    const next = new Set(expanded());
    if (next.has(key)) next.delete(key);
    else next.add(key);
    setExpanded(next);
  };

  // ----- Seen (ADR-0058 section 5) -----------------------------------------

  // Publishes the read receipts only. Its echoes are shared with every other
  // ChatClient on the page.
  const receiptPublisher = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const account = accountId();
      if (account !== null) await chats.persistSnapshot(account);
    },
  });

  /** Seen ids waiting to be filed, per group log key. */
  const pendingSeen = new Map<string, Set<string>>();
  let flushTimer: ReturnType<typeof setTimeout> | null = null;
  const FLUSH_DELAY_MS = 800;

  const wantsSeen = (ref: FeedCandidateRef): boolean =>
    ref.source.kind === "group" &&
    ref.message.direction === "incoming" &&
    !isSeenInSlot(log()?.[ref.source.idHex], ref.message);

  async function flushSeen(): Promise<void> {
    flushTimer = null;
    const account = accountId();
    const c = core();
    if (account === null || c === null || pendingSeen.size === 0) return;
    const batch = new Map<string, string[]>();
    for (const [hex, ids] of pendingSeen) batch.set(hex, [...ids]);
    pendingSeen.clear();
    const deviceKey = await getDeviceKey(account);
    if (deviceKey === null) return;
    const advanced = await recordFeedSeen(account, deviceKey, batch, (ids, seen) =>
      c.contiguousSeen(ids, seen),
    );
    for (const [hex, upTo] of advanced) {
      const entry = chats.groups().find((g) => syncGroupIdHex(g.group.groupId) === hex);
      if (entry === undefined || readIsActive(entry.group) === false) continue;
      const secrets = auth.identitySecrets();
      if (secrets !== null && chats.identity() !== null) {
        // A read receipt wakes nobody (ADR-0024), so it rides the control path.
        void composeReadReceiptEnvelope(Number(upTo))
          .then((bytes) => receiptPublisher.broadcastControl(secrets, entry.group, bytes))
          .catch((err: unknown) => {
            // eslint-disable-next-line no-console
            console.warn("[feed] read receipt failed", err);
          });
      }
      void sync
        .setReadCursor(entry.group.groupId, readCursorBytes(upTo), BigInt(Date.now()))
        .catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("[feed] read cursor failed", err);
        });
    }
  }

  function queueSeen(ref: FeedCandidateRef): void {
    if (!wantsSeen(ref)) return;
    const ids = pendingSeen.get(ref.source.idHex) ?? new Set<string>();
    ids.add(ref.message.messageId);
    pendingSeen.set(ref.source.idHex, ids);
    if (flushTimer === null) {
      flushTimer = setTimeout(() => {
        void flushSeen().catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("[feed] marking seen failed", err);
        });
      }, FLUSH_DELAY_MS);
    }
  }

  /** Per watched element: its ref and the running dwell timer. */
  const watched = new Map<Element, { ref: FeedCandidateRef; timer: ReturnType<typeof setTimeout> | null }>();
  let observer: IntersectionObserver | null = null;

  /** Fully on screen, or filling the screen when taller than it. */
  const fullyShown = (e: IntersectionObserverEntry): boolean =>
    e.intersectionRatio >= 0.99 ||
    (e.rootBounds !== null && e.intersectionRect.height >= e.rootBounds.height * 0.9);

  function ensureObserver(): IntersectionObserver | null {
    if (observer !== null || typeof IntersectionObserver === "undefined") return observer;
    observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          const w = watched.get(e.target);
          if (w === undefined) continue;
          if (fullyShown(e)) {
            if (w.timer === null) {
              w.timer = setTimeout(() => {
                w.timer = null;
                if (document.visibilityState === "visible") queueSeen(w.ref);
              }, FEED_SEEN_DWELL_MS);
            }
          } else if (w.timer !== null) {
            clearTimeout(w.timer);
            w.timer = null;
          }
        }
      },
      { threshold: [0, 0.5, 0.99, 1] },
    );
    return observer;
  }

  /** Watch one item's element for the dwell; stops when it leaves the DOM. */
  function watchSeen(el: Element, ref: FeedCandidateRef): void {
    if (!wantsSeen(ref)) return;
    const o = ensureObserver();
    if (o === null) return;
    watched.set(el, { ref, timer: null });
    o.observe(el);
    onCleanup(() => {
      const w = watched.get(el);
      if (w?.timer != null) clearTimeout(w.timer);
      watched.delete(el);
      o.unobserve(el);
    });
  }

  onCleanup(() => {
    observer?.disconnect();
    for (const w of watched.values()) if (w.timer !== null) clearTimeout(w.timer);
    watched.clear();
    if (flushTimer !== null) {
      clearTimeout(flushTimer);
      void flushSeen().catch(() => undefined);
    }
  });

  const bodyOf = (ref: FeedCandidateRef): string =>
    ref.message.text.trim() !== "" ? ref.message.text : (attachmentLine(ref.message) ?? "");

  /** The top of the feed a draft would build, for the editor's preview. */
  const previewFor = (draft: FeedDraft): readonly PreviewLine[] | null => {
    const c = core();
    const i = input();
    if (c === null || i === null) return null;
    const rule = ruleFromDraft(c, draft);
    try {
      return rankFeedInput(c, rule, i, clock())
        .slice(0, PREVIEW_ITEMS)
        .map((item) => ({
          key: itemKey(item),
          source: item.ref.source.title,
          text: bodyOf(item.ref),
        }));
    } finally {
      rule.free();
    }
  };

  const editorInitial = (): FeedDraft | null => {
    const c = core();
    const e = editing();
    if (c === null || e === null) return null;
    if (e.mode === "new") return blankDraft(c);
    return saved().find((v) => v.id === e.id)?.draft ?? null;
  };

  /** The shared form of a saved feed: its link and its file bytes. */
  const shareOf = (v: SavedView): { readonly link: string; readonly bytes: Uint8Array } | null => {
    const c = core();
    if (c === null) return null;
    try {
      const bytes = exportDraft(c, v.draft);
      return { link: shareLink(window.location.origin, bytes), bytes };
    } catch {
      return null;
    }
  };

  const download = (v: SavedView, bytes: Uint8Array): void => {
    const url = URL.createObjectURL(new Blob([bytes.slice()], { type: "application/octet-stream" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = shareFileName(v.draft);
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  return (
    <Show
      when={editing() !== null && core() !== null ? editorInitial() : null}
      keyed
      fallback={feedView()}
    >
      {(initial) => (
        <>
          <FeedEditor
            core={core() as FeedCore & FeedDraftCore}
            initial={initial}
            isNew={editing()?.mode === "new"}
            sources={feedSources()}
            preview={previewFor}
            onSave={(d) => void saveDraft(d)}
            onCancel={() => setEditing(null)}
            onDelete={(() => {
              const e = editing();
              return e?.mode === "edit" ? () => void deleteSaved(e.id) : undefined;
            })()}
          />
          <Show when={editError()}>
            {(msg) => (
              <p class="lk-feed-problem lk-feed-editor-error" role="alert">
                {msg()}
              </p>
            )}
          </Show>
        </>
      )}
    </Show>
  );

  function feedView(): JSX.Element {
    return (
    <section class="lk-feed" aria-label="Feed">
      <header class="lk-feed-head">
        <h1>Feed</h1>
        <div class="lk-feed-switcher" role="tablist" aria-label="Feeds">
          <For each={FEED_PRESETS}>
            {(p) => (
              <button
                type="button"
                role="tab"
                class="lk-feed-chip"
                aria-selected={selection() === p.key}
                onClick={() => choose(p.key)}
              >
                {p.label}
              </button>
            )}
          </For>
          <button
            type="button"
            role="tab"
            class="lk-feed-chip"
            aria-selected={selection() === "discover"}
            onClick={() => choose("discover")}
          >
            Discover
          </button>
          <For each={saved()}>
            {(v) => (
              <button
                type="button"
                role="tab"
                class="lk-feed-chip"
                aria-selected={selection() === `custom:${v.id}`}
                onClick={() => choose(`custom:${v.id}`)}
              >
                {v.draft.name}
              </button>
            )}
          </For>
          <button
            type="button"
            class="lk-feed-chip lk-feed-chip-new"
            onClick={() => {
              setEditError(null);
              setEditing({ mode: "new" });
            }}
          >
            New feed
          </button>
        </div>
        <Show when={selectedSaved()}>
          {(v) => (
            <div class="lk-feed-actions">
              <button
                type="button"
                class="ghost"
                onClick={() => {
                  setEditError(null);
                  setEditing({ mode: "edit", id: v().id });
                }}
              >
                Edit
              </button>
              <button
                type="button"
                class="ghost"
                onClick={() => {
                  setCopied(false);
                  setSharing(v());
                }}
              >
                Share
              </button>
            </div>
          )}
        </Show>
      </header>

      <Show when={selection() !== "discover" && !bannerDismissed() && (input()?.candidates.length ?? 0) > 0}>
        <div class="lk-feed-banner" role="note">
          <Icon name="info" size={18} />
          <span>Your feed shows messages this device has received.</span>
          <button
            type="button"
            class="ghost"
            onClick={() => {
              setBannerDismissed(true);
              writeStorage(BANNER_STORAGE_KEY, "1");
            }}
          >
            Got it
          </button>
        </div>
      </Show>

      <Show when={selection() !== "discover" && newCount() > 0}>
        <button type="button" class="lk-feed-new-pill" onClick={showPending}>
          Show {newCount()} new
        </button>
      </Show>

      <div class="lk-feed-list" ref={(el) => (listEl = el)} onScroll={onScroll}>
        <Show when={selection() === "discover"}>
          <DiscoverView model={discover} />
        </Show>
        <Show when={selection() !== "discover"}>
        <Show
          when={shown()}
          fallback={
            <div class="lk-feed-skeleton" aria-busy="true" aria-label="Loading">
              <div />
              <div />
              <div />
            </div>
          }
        >
          {(items) => (
            <Show
              when={items().length > 0}
              fallback={
                <div class="lk-empty">
                  <Show
                    when={feedSources().length > 0}
                    fallback={
                      <>
                        <span>Your feed fills up from your groups and channels.</span>
                        <button type="button" class="primary" onClick={() => choose("discover")}>
                          Discover channels
                        </button>
                      </>
                    }
                  >
                    <Show
                      when={(input()?.candidates.length ?? 0) > 0}
                      fallback={
                        <span>
                          Nothing here yet. New messages from your groups and channels will show up
                          here.
                        </span>
                      }
                    >
                      <span>No messages match this feed.</span>
                      <Show when={selectedSaved()}>
                        {(v) => (
                          <button
                            type="button"
                            class="primary"
                            onClick={() => setEditing({ mode: "edit", id: v().id })}
                          >
                            Edit feed
                          </button>
                        )}
                      </Show>
                    </Show>
                  </Show>
                </div>
              }
            >
              <For each={items()}>
                {(item, index) => {
                  const key = itemKey(item);
                  const why = (): string | null => whyLine(item.ref, whyPreset());
                  // A saved feed that asks for suggestions shows a channel
                  // card after some items (ADR-0059, UX spec 4.1 section 4).
                  const suggestion = (): number | undefined => cardAfter().get(index());
                  return (
                    <>
                    <article class="lk-feed-item" ref={(el) => watchSeen(el, item.ref)}>
                      <button type="button" class="lk-feed-open" onClick={() => open(item.ref)}>
                        <div class="lk-feed-meta">
                          <span class="lk-feed-source">{item.ref.source.title}</span>
                          <span class="lk-feed-time">
                            {formatListTime(item.ref.message.createdAt, new Date(clock()))}
                          </span>
                        </div>
                        {/* A channel speaks as itself: its name is already the source. */}
                        <Show when={item.ref.source.kind === "group"}>
                          <div class="lk-feed-author">{authorOf(item.ref)}</div>
                        </Show>
                        <Show when={item.ref.message.forward?.author}>
                          {(name) => <div class="lk-feed-forwarded">Forwarded from {name()}</div>}
                        </Show>
                        <p class="lk-feed-text">
                          {bodyOf(item.ref)}
                          <Show when={item.ref.message.edited === true}>
                            <span class="lk-feed-edited"> (edited)</span>
                          </Show>
                        </p>
                        <Show when={why()}>{(line) => <div class="lk-feed-why">{line()}</div>}</Show>
                      </button>
                      <Show when={reactionSummary(item.ref.message).length > 0}>
                        <div class="lk-feed-reactions" aria-label="Reactions">
                          <For each={reactionSummary(item.ref.message)}>
                            {(r) => (
                              <span class="lk-feed-reaction">
                                {r.emoji} {r.count}
                              </span>
                            )}
                          </For>
                        </div>
                      </Show>
                      <Show when={item.replies.length > 0}>
                        <button
                          type="button"
                          class="lk-feed-replies-toggle"
                          aria-expanded={expanded().has(key)}
                          onClick={() => toggleReplies(key)}
                        >
                          <span>{repliesLabel(item.replies.length)}</span>
                          <Show when={!expanded().has(key)}>
                            {(() => {
                              const last = item.replies[item.replies.length - 1];
                              return last === undefined ? null : (
                                <span class="lk-feed-reply-preview">
                                  {authorOf(last)}: {bodyOf(last)}
                                </span>
                              );
                            })()}
                          </Show>
                        </button>
                        <Show when={expanded().has(key)}>
                          <ol class="lk-feed-replies">
                            <For each={item.replies}>
                              {(reply) => (
                                <li ref={(el) => watchSeen(el, reply)}>
                                  <button
                                    type="button"
                                    class="lk-feed-reply"
                                    onClick={() => open(reply)}
                                  >
                                    <span class="lk-feed-author">{authorOf(reply)}</span>
                                    <span class="lk-feed-text">{bodyOf(reply)}</span>
                                  </button>
                                </li>
                              )}
                            </For>
                          </ol>
                        </Show>
                      </Show>
                    </article>
                    <Show when={suggestion() !== undefined ? discover.cards()[suggestion() as number] : undefined}>
                      {(card) => <DiscoverCardView card={card()} model={discover} />}
                    </Show>
                    </>
                  );
                }}
              </For>
              <div class="lk-feed-end">You're up to date.</div>
            </Show>
          )}
        </Show>
        </Show>
      </div>

      <Show when={sharing()}>
        {(v) => {
          const shared = shareOf(v());
          return (
            <div class="lk-feed-share-scrim" onClick={() => setSharing(null)}>
              <div
                class="lk-feed-share"
                role="dialog"
                aria-label="Share feed"
                onClick={(e) => e.stopPropagation()}
              >
                <h2>Share "{v().draft.name}"</h2>
                <Show
                  when={shared}
                  fallback={<p class="lk-feed-problem">This feed can't be shared as it is.</p>}
                >
                  {(sh) => (
                    <>
                      <p class="lk-feed-hint">
                        Specific conversations are left out: they mean nothing to anyone else.
                      </p>
                      <input class="lk-feed-share-link" type="text" readOnly value={sh().link} />
                      <div class="lk-feed-share-actions">
                        <button
                          type="button"
                          class="primary"
                          onClick={() => {
                            void navigator.clipboard
                              ?.writeText(sh().link)
                              .then(() => setCopied(true))
                              .catch(() => setCopied(false));
                          }}
                        >
                          {copied() ? "Copied" : "Copy link"}
                        </button>
                        <button type="button" class="ghost" onClick={() => download(v(), sh().bytes)}>
                          Download file
                        </button>
                        <button type="button" class="ghost" onClick={() => setSharing(null)}>
                          Close
                        </button>
                      </div>
                    </>
                  )}
                </Show>
              </div>
            </div>
          );
        }}
      </Show>
    </section>
    );
  }
}
