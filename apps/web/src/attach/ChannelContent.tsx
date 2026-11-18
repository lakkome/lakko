// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 / UX 7.7 — a content post in a channel: a poll (one vote per
 * account through the anonymous credential, counts per opaque label from the
 * channel service), a checklist (read here; publishers' ticks are not built
 * on web), a location, a contact card, an article.
 */
import { createMemo, createResource, createSignal, Match, onCleanup, Show, Switch, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Decode } from "../auth/account-id";
import { t } from "../l10n";
import { CHANNEL_AUTHOR_B64, type ChannelChecklistOps } from "../channels/channel-timeline";
import { getDeviceKey } from "../auth/key-store";
import { deviceGroupLineageSeed } from "../devices/lifecycle";
import { loadWasm, wasmSync } from "../wasm";
import type { AttachmentRefBytes } from "../chat/attachments";
import type { MediaHub } from "../media/media-hub";
import type { Article, MessageContent, PollBody, SignedChecklist } from "../../wasm/lakko_ffi_wasm";
import { channelSeedFor } from "../channels/channel-access";
import { bytesOfHex, hexOf } from "../channels/feedback-api";
import { postVoteCounts, requestReactionCounts } from "../channels/post-feedback";
import { PollCard } from "./PollCard";
import { ChecklistCard } from "./ChecklistCard";
import { LocationCard } from "./LocationCard";
import { ContactCardBubble } from "./ContactCardBubble";
import { ArticleCard } from "./ArticleCard";
import { channelPollOptionLabels, optionCountsOf, sendChannelVote } from "./channel-poll";
import { ownPollChoice, pollChoicePort } from "./poll-choice-register";
import { checklistViewOf, pollViewOf, type TallyLike } from "./poll-model";

export interface ChannelContentProps {
  readonly channelIdHex: string;
  readonly cursor: number;
  readonly epoch: number | undefined;
  readonly content: MessageContent;
  readonly attachments: readonly AttachmentRefBytes[];
  readonly closed: boolean;
  readonly closedTally?: { readonly voters: number; readonly counts: readonly number[] } | undefined;
  readonly canReact: boolean;
  /** The owner or a publisher: ticks and adds items (UX 8.1). */
  readonly canPublish?: boolean | undefined;
  /** Publishes one post (a checklist op is a post of the channel). */
  readonly publish?: ((plaintext: Uint8Array) => Promise<void>) | undefined;
  readonly checklistOps?: ChannelChecklistOps | undefined;
  readonly hub: MediaHub;
  readonly onOpenMenu: () => void;
  readonly onOpenArticle: (article: Article, attachments: readonly AttachmentRefBytes[]) => void;
}

/** The option labels of a channel poll post, hex, or `null` while the seed
 *  of its epoch (or the core's label function) is not here. */
export function pollOptionLabelsHex(channelIdHex: string, epoch: number | undefined, cursor: number, options: number): string[] | null {
  if (epoch === undefined) return null;
  const seed = channelSeedFor(channelIdHex, epoch);
  if (seed === undefined) return null;
  const w = wasmSync();
  const key = w.ChannelReactionKey.derive(seed, bytesOfHex(channelIdHex), epoch);
  try {
    return channelPollOptionLabels(w, key, cursor, options).map(hexOf);
  } catch {
    return null;
  } finally {
    key.free();
  }
}

export function ChannelContent(props: ChannelContentProps): JSX.Element {
  return (
    <Switch>
      <Match when={props.content.kind === "poll" ? props.content : null}>
        {(c) => <ChannelPoll {...props} body={c().poll.body} />}
      </Match>
      <Match when={props.content.kind === "checklist" ? props.content : null}>
        {(c) => <ChannelChecklist {...props} checklist={c().checklist} />}
      </Match>
      <Match when={props.content.kind === "location" ? props.content : null}>
        {(c) => <LocationCard location={c().location} onOpen={() => props.onOpenMenu()} />}
      </Match>
      <Match when={props.content.kind === "contactCard" ? props.content : null}>
        {(c) => (
          <ContactCardBubble card={c().card} relation="other" busy={false} onAdd={() => undefined} onOpenChat={() => undefined} />
        )}
      </Match>
      <Match when={props.content.kind === "article" ? props.content : null}>
        {(c) => <ChannelArticle {...props} article={c().article} />}
      </Match>
    </Switch>
  );
}

function ChannelPoll(props: ChannelContentProps & { body: PollBody }): JSX.Element {
  const auth = useAuth();
  const [sending, setSending] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  const [pending, setPending] = createSignal<readonly number[] | null>(null);
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => setNow(Date.now()), 30_000);
  onCleanup(() => clearInterval(timer));
  requestReactionCounts(props.channelIdHex, [props.cursor]);

  const labels = createMemo(() => pollOptionLabelsHex(props.channelIdHex, props.epoch, props.cursor, props.body.options.length));
  const tally = (): TallyLike | null => {
    if (props.closedTally !== undefined) {
      return { counts: props.closedTally.counts, voters: props.closedTally.voters, closed: true };
    }
    const l = labels();
    const v = postVoteCounts(props.channelIdHex, props.cursor);
    if (l === null) return { counts: props.body.options.map(() => 0), voters: 0, closed: props.closed };
    return { counts: optionCountsOf(l, v?.labels ?? new Map()), voters: v?.voters ?? 0, closed: props.closed };
  };
  const ownChoice = (): readonly number[] | undefined =>
    pending() ?? ownPollChoice(3, bytesOfHex(props.channelIdHex), props.cursor)?.choices;
  const view = () =>
    pollViewOf({
      body: props.body,
      channel: true,
      verified: true,
      tally: tally(),
      ownChoice: ownChoice(),
      sending: sending(),
      failed: failed(),
      canAct: props.canReact && labels() !== null,
      nowMs: now(),
    });

  async function vote(choices: readonly number[]): Promise<void> {
    const s = auth.state();
    const l = labels();
    if (s.kind !== "session" || l === null) return;
    setSending(true);
    setFailed(false);
    setPending(choices);
    try {
      const account = s.account.accountId;
      const deviceKey = await getDeviceKey(account);
      const lineage = deviceKey === null ? null : await deviceGroupLineageSeed(account, deviceKey);
      if (lineage === null) throw new Error("no lineage seed");
      const w = await loadWasm();
      const identity = w.Identity.fromSeed(lineage);
      let seed32: Uint8Array;
      try {
        seed32 = identity.ed25519Seed;
      } finally {
        identity.free();
      }
      await sendChannelVote({
        accountId: account,
        lineageSeed32: seed32,
        channelIdHex: props.channelIdHex,
        postCursor: props.cursor,
        labels: choices.map((i) => bytesOfHex(l[i] ?? "")),
      });
      seed32.fill(0);
      await pollChoicePort()?.setChoice(3, bytesOfHex(props.channelIdHex), props.cursor, choices, 0);
      requestReactionCounts(props.channelIdHex, [props.cursor]);
    } catch (err) {
      console.warn("[attach] channel vote not sent", err instanceof Error ? err.name : "unknown");
      setFailed(true);
    } finally {
      setSending(false);
      setPending(null);
    }
  }

  return (
    <PollCard
      view={view()}
      onVote={(c) => void vote(c)}
      onRetry={() => {
        const c = ownChoice();
        if (c !== undefined) void vote(c);
      }}
      onViewResults={() => undefined}
    />
  );
}

function ChannelChecklist(props: ChannelContentProps & { checklist: SignedChecklist }): JSX.Element {
  const [error, setError] = createSignal(false);
  const state = createMemo(() => {
    const ops = props.checklistOps ?? { edits: [], adds: [], ticks: [], closes: [] };
    try {
      return wasmSync().foldChecklist(
        props.checklist.body,
        base64Decode(CHANNEL_AUTHOR_B64),
        BigInt(props.cursor),
        ops.edits,
        ops.adds,
        ops.ticks,
        ops.closes,
      );
    } catch {
      return null;
    }
  });
  const view = () => {
    const st = state();
    const body = props.checklist.body;
    return checklistViewOf({
      title: st?.title ?? body.title,
      othersAdd: false,
      othersMark: false,
      items: st?.items ?? body.items.map((i) => ({ item: { kind: "author" as const, id: i.id }, text: i.text, doneBy: null })),
      closed: (st?.closedBy ?? null) !== null || props.closed,
      // UX 8.1 — in a channel only publishers tick and add; subscribers read.
      isAuthor: props.canPublish === true,
      canAct: props.canPublish === true && props.publish !== undefined,
      oneToOne: false,
      ownKeyB64: undefined,
      firstNameOf: () => "",
      showTicker: false,
    });
  };
  const send = async (bytes: Uint8Array): Promise<void> => {
    setError(false);
    try {
      await props.publish?.(bytes);
    } catch {
      setError(true);
    }
  };
  return (
    <>
      <ChecklistCard
        view={view()}
        onToggle={(item, done) => void send(wasmSync().composeChecklistTickEnvelope(BigInt(props.cursor), item, done))}
        onAdd={(text) => void send(wasmSync().composeChecklistAddEnvelope(BigInt(props.cursor), text))}
      />
      <Show when={error()}>
        <div class="lk-alert danger" role="alert">
          {t("channels.publish.error.generic")}
        </div>
      </Show>
    </>
  );
}

function ChannelArticle(props: ChannelContentProps & { article: Article }): JSX.Element {
  const first = (): AttachmentRefBytes | undefined => {
    for (const b of props.article.blocks) if (b.kind === "image") return props.attachments[b.attachmentIndex];
    return undefined;
  };
  const [banner] = createResource(first, async (ref) => {
    try {
      return await props.hub.load(ref);
    } catch {
      return undefined;
    }
  });
  const excerpt = (): string => {
    try {
      return wasmSync().articleExcerpt(props.article);
    } catch {
      return "";
    }
  };
  return (
    <ArticleCard
      article={props.article}
      excerpt={excerpt()}
      bannerUrl={banner() ?? undefined}
      onOpen={() => props.onOpenArticle(props.article, props.attachments)}
    />
  );
}

/** `Close poll` by a publisher (UX 7.7): the close carries the tally its app
 *  reads now. Uses the core's tally encoding when the loaded wasm has it;
 *  without it the close goes out without a tally (readers then show the
 *  live counts, frozen only by the service's own state). */
export async function composeChannelPollClose(
  channelIdHex: string,
  epoch: number | undefined,
  cursor: number,
  body: PollBody,
): Promise<Uint8Array> {
  const w = await loadWasm();
  const labels = pollOptionLabelsHex(channelIdHex, epoch, cursor, body.options.length);
  const counts = postVoteCounts(channelIdHex, cursor);
  if (labels === null) return w.composePollCloseEnvelope(BigInt(cursor));
  const flat = (hexes: readonly string[]): Uint8Array => {
    const out = new Uint8Array(hexes.length * 16);
    hexes.forEach((h, i) => out.set(bytesOfHex(h), i * 16));
    return out;
  };
  const seen = [...(counts?.labels.keys() ?? [])];
  const tally = w.channelPollTallyFromCounts(
    flat(labels),
    flat(seen),
    BigUint64Array.from(seen.map((k) => BigInt(counts?.labels.get(k) ?? 0))),
    BigInt(counts?.voters ?? 0),
  );
  try {
    return w.composePollCloseEnvelope(BigInt(cursor), w.encodeChannelPollTally(tally));
  } finally {
    tally.free();
  }
}

/** The post menu's `Close poll` row is offered to a publisher on an open poll. */
export function channelPollClosable(content: MessageContent | undefined, closed: boolean, canPublish: boolean): boolean {
  if (content?.kind !== "poll" || closed || !canPublish) return false;
  return true;
}
