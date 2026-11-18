// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0092 — the bubble of a content message in a one-to-one or group: a
 * poll, a checklist, a location, a contact card or an article. Folds the
 * stored ops through the core and hands the cards a view; a person's action
 * goes back through the {@link ContentHost} the conversation provides (it
 * holds the group and the send path). The message-menu items of a content
 * message come from {@link contentMenuActions}.
 */
import { createEffect, createMemo, createResource, createSignal, Match, onCleanup, Show, Switch, type JSX } from "solid-js";
import { Portal } from "solid-js/web";

import { base64Decode, base64Encode } from "../auth/account-id";
import { t } from "../l10n";
import { displayLocale } from "../l10n/display-locale";
import { wasmSync } from "../wasm";
import type { IconName } from "../ui/icons";
import { requestConfirm } from "../ui/confirm-dialog";
import type { AttachmentRefBytes } from "../chat/attachments";
import type { MediaHub } from "../media/media-hub";
import type {
  Article,
  ChecklistItemRef,
  ContentState,
  MessageContent,
  SignedChecklist,
  SignedPoll,
  SharedContactCard,
  SharedLocation,
} from "../../wasm/lakko_ffi_wasm";
import { PollCard } from "./PollCard";
import { PollResults } from "./PollResults";
import { ChecklistCard } from "./ChecklistCard";
import { LocationCard } from "./LocationCard";
import { ContactCardBubble } from "./ContactCardBubble";
import { ArticleCard } from "./ArticleCard";
import { contentOf, entryOf, loadContent, updateContent, withEntry, withOp, type ChecklistEntry, type PollEntry } from "./content-store";
import { attachmentRefFromPersisted } from "../chat/persisted-attachment";
import { heldOpOf } from "./receive";
import { castBallot } from "./ballot-cast";
import { enqueueBallot, queuedBallotOf, retryBallot } from "./ballot-queue";
import { ownPollChoice } from "./poll-choice-register";
import { canClosePoll, canRetract, checklistViewOf, firstName, pollResultSections, pollViewOf, type TallyLike } from "./poll-model";
import { formatCoordinates } from "./location-format";
import { checklistRecordsFromSnapshot, checklistStateFromSnapshot, shownTally } from "./snapshot";
import type { ContactCardRelation, PollResultSection } from "./types";

/** What the conversation lends a content bubble. */
export interface ContentHost {
  readonly groupIdB64: string;
  groupId(): Uint8Array;
  /** `true` in a one-to-one. */
  oneToOne(): boolean;
  /** This person is the group's owner or an admin. */
  isAdmin(): boolean;
  /** The conversation takes this person's ops (live membership). */
  canAct(): boolean;
  /** This account's key as the conversation resolves it (base64). */
  ownKeyB64(): string | undefined;
  nameOf(keyB64: string): string;
  readonly hub: MediaHub;
  /** Publish an op quietly (no wake); the DS id and time. */
  sendOp(bytes: Uint8Array): Promise<{ messageId: number; createdAt: string }>;
  /** The 32-byte Ed25519 seed of the key this account's leaf is bound to. */
  voterSeed(): Promise<Uint8Array | null>;
  contactRelation(card: SharedContactCard): ContactCardRelation;
  /** `Add contact`: `null` when the request went out, else the line to show. */
  addContact(card: SharedContactCard, senderName: string): Promise<string | null>;
  openChat(card: SharedContactCard): void;
  openArticle(args: { messageId: number; article: Article; attachments: readonly AttachmentRefBytes[]; authorKeyB64: string | undefined; createdAt: string | undefined; own: boolean }): void;
  /** `Edit checklist` / `Edit` on an article. */
  editContent(messageId: number, content: MessageContent, attachments: readonly AttachmentRefBytes[]): void;
  copyText(text: string): void;
  /** Every account in the conversation now (base64): the restored item's
   *  author is found among them by its signature. */
  rosterKeys(): readonly string[];
  openUrl(url: string): void;
  onError(text: string): void;
}

interface Props {
  readonly host: ContentHost;
  readonly content: MessageContent;
  /** ADR-0092 section 3.11 — the state a history bundle carried. */
  readonly snapshot?: ContentState | undefined;
  readonly messageId: number | undefined;
  readonly attachments: readonly AttachmentRefBytes[];
  readonly own: boolean;
  readonly authorKeyB64: string | undefined;
  readonly createdAt: string | undefined;
  /** Location: a tap opens the message menu (web, UX 9.3). */
  readonly onOpenMenu: () => void;
}

export function ContentBubble(props: Props): JSX.Element {
  // Loading the conversation's store once is what makes `entryOf` reactive.
  void loadContent(props.host.groupIdB64);
  return (
    <Switch>
      <Match when={props.content.kind === "poll" ? props.content : null}>
        {(c) => <PollBubble {...props} content={c()} />}
      </Match>
      <Match when={props.content.kind === "checklist" ? props.content : null}>
        {(c) => <ChecklistBubble {...props} content={c()} />}
      </Match>
      <Match when={props.content.kind === "location" ? props.content : null}>
        {(c) => <LocationCard location={c().location} onOpen={() => props.onOpenMenu()} />}
      </Match>
      <Match when={props.content.kind === "contactCard" ? props.content : null}>
        {(c) => <ContactBubble {...props} card={c().card} />}
      </Match>
      <Match when={props.content.kind === "article" ? props.content : null}>
        {(c) => <ArticleBubble {...props} article={c().article} />}
      </Match>
    </Switch>
  );
}

// ----- poll -----------------------------------------------------------

type PollContent = Extract<MessageContent, { kind: "poll" }>;

/** Fold a poll entry through the core (synchronously; wasm is loaded once a
 *  timeline is on screen). */
export function foldPollEntry(entry: PollEntry, nowMs: number): TallyLike | null {
  return shownTally(foldPollEntryLive(entry, nowMs), entry);
}

function foldPollEntryLive(entry: PollEntry, nowMs: number): TallyLike | null {
  const w = wasmSync();
  const body = entry.signed.body;
  try {
    if (body.anonymous != null) {
      const allowedCloses = entry.closes.filter((c) => c.closer === entry.authorKeyB64 || c.closerIsAdmin);
      const closeId = allowedCloses.length === 0 ? null : BigInt(Math.min(...allowedCloses.map((c) => c.messageId)));
      const counted = entry.ballots
        .filter((b) => b.verified !== undefined && b.verified !== null)
        .map((b) => new w.CountedPollBallot(BigInt(b.messageId), BigInt(b.createdAtMs), base64Decode(b.verified!.tag), Uint8Array.from(b.verified!.choices)));
      const tally = w.foldPollBallots(body.options.length, body.multiple, closeId, body.closeAtMs == null ? null : BigInt(body.closeAtMs), counted);
      try {
        const timeClosed = body.closeAtMs != null && nowMs >= body.closeAtMs;
        return { counts: Array.from(tally.counts), voters: tally.voters, closed: closeId !== null || timeClosed };
      } finally {
        tally.free();
      }
    }
    const tally = w.foldPoll(body, base64Decode(entry.authorKeyB64), BigInt(entry.messageId), [...entry.votes], [...entry.closes]);
    return {
      counts: tally.counts,
      voters: tally.voters,
      closed: w.pollTallyIsClosed(tally, BigInt(nowMs)),
      choicesByVoter: tally.choicesByVoter,
    };
  } catch {
    return null;
  }
}

function PollBubble(props: Props & { content: PollContent }): JSX.Element {
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => setNow(Date.now()), 30_000);
  onCleanup(() => clearInterval(timer));
  const [results, setResults] = createSignal<PollResultSection[] | null>(null);
  const entry = (): PollEntry | undefined => {
    if (props.messageId === undefined) return undefined;
    const e = entryOf(props.host.groupIdB64, props.messageId);
    return e?.kind === "poll" ? e : undefined;
  };
  // ADR-0092 section 3.11 — a poll restored from a history bundle: its
  // bundle state until (and under) live ops; a public poll is filed at once
  // (its author found by signature) so this device can vote; an anonymous one
  // waits for the hand-over's ring.
  const snapshot = (): Extract<ContentState, { kind: "poll" }> | undefined =>
    props.snapshot?.kind === "poll" ? props.snapshot : undefined;
  createEffect(() => {
    const snap = snapshot();
    const id = props.messageId;
    if (snap === undefined || id === undefined) return;
    void seedPollFromSnapshot(props.host, id, props.content.poll, snap);
  });
  const tally = createMemo(() => {
    const e = entry();
    if (e !== undefined) return foldPollEntry(e, now());
    const snap = snapshot();
    return snap === undefined ? null : { counts: snap.counts, voters: snap.voters, closed: snap.closed.kind !== "open" };
  });
  const queued = (): ReturnType<typeof queuedBallotOf> =>
    props.messageId === undefined ? undefined : queuedBallotOf(props.host.groupIdB64, props.messageId);
  const ownChoice = (): readonly number[] | undefined => {
    const q = queued();
    if (q !== undefined) return q.choices;
    const body = props.content.poll.body;
    if (props.messageId === undefined) return undefined;
    if (body.anonymous != null) {
      return ownPollChoice(2, props.host.groupId(), props.messageId)?.choices;
    }
    const own = props.host.ownKeyB64();
    return tally()?.choicesByVoter?.find((v) => v.voter === own)?.choices;
  };
  const view = () =>
    pollViewOf({
      body: props.content.poll.body,
      channel: false,
      verified: entry()?.verified ?? snapshot() !== undefined,
      tally: tally(),
      ownChoice: ownChoice(),
      sending: queued()?.state === "waiting",
      failed: queued()?.state === "failed",
      canAct: props.host.canAct() && props.messageId !== undefined,
      nowMs: now(),
    });
  return (
    <>
      <PollCard
        view={view()}
        onVote={(choices) => {
          const e = entry();
          if (e !== undefined) void votePoll(props.host, e, choices);
        }}
        onRetry={() => {
          if (props.messageId !== undefined) retryBallot(props.host.groupIdB64, props.messageId);
        }}
        onViewResults={() => {
          const tl = tally();
          if (tl === null) return;
          setResults(pollResultSections(props.content.poll.body, tl, (k) => props.host.nameOf(k)));
        }}
      />
      <Show when={results()}>
        {(sections) => (
          <Portal>
            <PollResults sections={sections()} onClose={() => setResults(null)} />
          </Portal>
        )}
      </Show>
    </>
  );
}

/** Vote (or retract with `[]`) in a group poll. */
export async function votePoll(host: ContentHost, entry: PollEntry, choices: readonly number[]): Promise<void> {
  const w = wasmSync();
  try {
    if (entry.signed.body.anonymous != null && entry.ringB64 !== undefined) {
      const seed = await host.voterSeed();
      if (seed === null) throw new Error("no voter seed");
      const recorded = ownPollChoice(2, host.groupId(), entry.messageId);
      const anon = entry.signed.body.anonymous;
      const setVersion = recorded?.setVersion ?? ownSetVersionFromBallots(entry, host.groupId(), seed);
      const cast = await castBallot({
        groupId: host.groupId(),
        pollId: base64Decode(entry.signed.body.id),
        secret: base64Decode(anon.secret),
        ring: base64Decode(entry.ringB64),
        voterSeed: seed,
        choices: Uint8Array.from(choices),
        setVersion,
      });
      enqueueBallot({
        groupIdB64: host.groupIdB64,
        pollMessageId: entry.messageId,
        choices: [...choices],
        rowB64: base64Encode(cast.row),
        setVersion: cast.setVersion,
      });
      return;
    }
    const sent = await host.sendOp(w.composePollVoteEnvelope(BigInt(entry.messageId), Uint8Array.from(choices)));
    const own = host.ownKeyB64();
    if (own === undefined) return;
    const op = heldOpOf(
      { op: "poll-vote", target: entry.messageId, choices },
      sent.messageId,
      Date.parse(sent.createdAt) || Date.now(),
      own,
      host.isAdmin(),
    );
    if (op !== null) await updateContent(host.groupIdB64, (c) => withOp(c, op));
  } catch (err) {
    console.warn("[attach] vote not sent", err instanceof Error ? err.name : "unknown");
    host.onError(t("poll.vote_failed"));
  }
}

/** Where the register has not arrived but the account's own earlier ballot
 *  is readable (found by its tag), that ballot's set version, so a revote
 *  reuses the same anonymity set (ADR-0092 section 3.6). */
function ownSetVersionFromBallots(entry: PollEntry, groupId: Uint8Array, seed: Uint8Array): number | undefined {
  const anon = entry.signed.body.anonymous;
  const verified = entry.ballots.filter((b) => b.verified != null);
  if (anon == null || verified.length === 0) return undefined;
  const w = wasmSync();
  const keys = new w.PollBallotKeys(groupId, base64Decode(entry.signed.body.id), base64Decode(anon.secret));
  const voter = w.PollVoterKey.fromEd25519Seed(seed.slice());
  const list = verified.map((b) => new w.VerifiedPollBallot(base64Decode(b.verified!.tag), Uint8Array.from(b.verified!.choices), b.verified!.version));
  try {
    return w.ownPollBallotSetVersion(keys, voter, list);
  } catch {
    return undefined;
  } finally {
    voter.free();
    keys.free();
  }
}

// ----- checklist ------------------------------------------------------

type ChecklistContent = Extract<MessageContent, { kind: "checklist" }>;

export function foldChecklistEntry(entry: ChecklistEntry): ReturnType<ReturnType<typeof wasmSync>["foldChecklist"]> | null {
  const w = wasmSync();
  try {
    return w.foldChecklist(
      entry.signed.body,
      base64Decode(entry.authorKeyB64),
      BigInt(entry.messageId),
      [...entry.edits],
      [...entry.adds],
      [...entry.ticks],
      [...entry.closes],
    );
  } catch {
    return null;
  }
}

function ChecklistBubble(props: Props & { content: ChecklistContent }): JSX.Element {
  const entry = (): ChecklistEntry | undefined => {
    if (props.messageId === undefined) return undefined;
    const e = entryOf(props.host.groupIdB64, props.messageId);
    return e?.kind === "checklist" ? e : undefined;
  };
  const snapshot = (): Extract<ContentState, { kind: "checklist" }> | undefined =>
    props.snapshot?.kind === "checklist" ? props.snapshot : undefined;
  createEffect(() => {
    const snap = snapshot();
    const id = props.messageId;
    if (snap === undefined || id === undefined) return;
    void seedChecklistFromSnapshot(props.host, id, props.content.checklist, snap);
  });
  const state = createMemo(() => {
    const e = entry();
    if (e !== undefined) return foldChecklistEntry(e);
    const snap = snapshot();
    return snap === undefined ? null : checklistStateFromSnapshot(props.content.checklist.body, snap);
  });
  const view = () => {
    const st = state();
    const body = props.content.checklist.body;
    const own = props.host.ownKeyB64();
    return checklistViewOf({
      title: st?.title ?? body.title,
      othersAdd: st?.othersAdd ?? body.othersAdd,
      othersMark: st?.othersMark ?? body.othersMark,
      items:
        st?.items ??
        body.items.map((i) => ({ item: { kind: "author" as const, id: i.id }, text: i.text, doneBy: null })),
      closed: st?.closedBy != null,
      isAuthor: props.own || (own !== undefined && own === props.authorKeyB64),
      canAct: props.host.canAct() && entry() !== undefined,
      oneToOne: props.host.oneToOne(),
      ownKeyB64: own,
      firstNameOf: (k) => firstName(props.host.nameOf(k)),
    });
  };
  const send = async (bytes: Uint8Array, op: Parameters<typeof heldOpOf>[0]): Promise<void> => {
    try {
      const sent = await props.host.sendOp(bytes);
      const own = props.host.ownKeyB64();
      if (own === undefined) return;
      const held = heldOpOf(op, sent.messageId, Date.parse(sent.createdAt) || Date.now(), own, props.host.isAdmin());
      if (held !== null) await updateContent(props.host.groupIdB64, (c) => withOp(c, held));
    } catch (err) {
      console.warn("[attach] checklist op not sent", err instanceof Error ? err.name : "unknown");
      props.host.onError(t("chat.send.failed"));
    }
  };
  return (
    <ChecklistCard
      view={view()}
      onToggle={(item: ChecklistItemRef, done: boolean) => {
        const id = props.messageId;
        if (id === undefined) return;
        void send(wasmSync().composeChecklistTickEnvelope(BigInt(id), item, done), {
          op: "checklist-tick",
          target: id,
          item,
          done,
        });
      }}
      onAdd={(text: string) => {
        const id = props.messageId;
        if (id === undefined) return;
        void send(wasmSync().composeChecklistAddEnvelope(BigInt(id), text), { op: "checklist-add", target: id, text });
      }}
    />
  );
}

// ----- restored from a history bundle ---------------------------------

/** The account among the roster whose signature the item carries. */
function authorBySignature(host: ContentHost, verify: (key: Uint8Array) => void): string | undefined {
  for (const k of host.rosterKeys()) {
    try {
      verify(base64Decode(k));
      return k;
    } catch {
      // not this one
    }
  }
  return undefined;
}

/** File a restored poll (ADR-0092 section 3.11). An entry already held (a
 *  hand-over came first) only gains the bundle's baseline. */
export async function seedPollFromSnapshot(
  host: ContentHost,
  messageId: number,
  poll: SignedPoll,
  snap: Extract<ContentState, { kind: "poll" }>,
): Promise<void> {
  const baseline = { counts: snap.counts, voters: snap.voters, closed: snap.closed.kind !== "open" };
  // A held entry came from the hand-over (per-voter state): it is complete
  // and the bundle's totals are not used.
  if (entryOf(host.groupIdB64, messageId) !== undefined) return;
  // An anonymous poll needs its ring, which only the hand-over brings.
  if (poll.body.anonymous != null) return;
  const w = wasmSync();
  const author = authorBySignature(host, (k) => w.verifyPoll(poll, host.groupId(), k));
  if (author === undefined) return;
  await updateContent(host.groupIdB64, (c) =>
    withEntry(c, {
      kind: "poll",
      messageId,
      createdAtMs: 0,
      authorKeyB64: author,
      signed: poll,
      verified: true,
      votes: [],
      closes: snap.closed.kind === "byOp" ? [{ messageId: snap.closed.messageId, closer: author, closerIsAdmin: false }] : [],
      ballots: [],
      baseline,
      fromSnapshot: true,
    }),
  );
}

export async function seedChecklistFromSnapshot(
  host: ContentHost,
  messageId: number,
  checklist: SignedChecklist,
  snap: Extract<ContentState, { kind: "checklist" }>,
): Promise<void> {
  if (entryOf(host.groupIdB64, messageId) !== undefined) return;
  const w = wasmSync();
  const author = authorBySignature(host, (k) => w.verifyChecklist(checklist, host.groupId(), k));
  if (author === undefined) return;
  const r = checklistRecordsFromSnapshot(author, messageId, snap);
  await updateContent(host.groupIdB64, (c) =>
    withEntry(c, {
      kind: "checklist",
      messageId,
      createdAtMs: 0,
      authorKeyB64: author,
      signed: checklist,
      verified: true,
      edits: [],
      adds: r.adds,
      ticks: r.ticks,
      closes: r.closes,
      fromSnapshot: true,
    }),
  );
}

// ----- contact card ---------------------------------------------------

function ContactBubble(props: Props & { card: SharedContactCard }): JSX.Element {
  const [busy, setBusy] = createSignal(false);
  const [message, setMessage] = createSignal<string | undefined>(undefined);
  const senderName = (): string =>
    props.own ? t("chat.author.you") : props.authorKeyB64 === undefined ? "" : props.host.nameOf(props.authorKeyB64);
  return (
    <ContactCardBubble
      card={props.card}
      relation={props.host.contactRelation(props.card)}
      busy={busy()}
      message={message()}
      onAdd={() => {
        setBusy(true);
        void props.host
          .addContact(props.card, senderName())
          .then((m) => setMessage(m ?? undefined))
          .finally(() => setBusy(false));
      }}
      onOpenChat={() => props.host.openChat(props.card)}
    />
  );
}

// ----- article --------------------------------------------------------

/** The article a bubble shows: the author's latest edit, else the message. */
export function currentArticle(
  host: ContentHost,
  messageId: number | undefined,
  authorKeyB64: string | undefined,
  own: boolean,
  article: Article,
  attachments: readonly AttachmentRefBytes[],
): { article: Article; attachments: readonly AttachmentRefBytes[] } {
  if (messageId === undefined) return { article, attachments };
  const edit = contentOf(host.groupIdB64)?.articleEdits?.[String(messageId)];
  const author = own ? host.ownKeyB64() : authorKeyB64;
  if (edit === undefined || author === undefined || edit.editor !== author) return { article, attachments };
  return { article: edit.article, attachments: edit.attachments.map((a) => attachmentRefFromPersisted(a).ref) };
}

function ArticleBubble(props: Props & { article: Article }): JSX.Element {
  const current = () =>
    currentArticle(props.host, props.messageId, props.authorKeyB64, props.own, props.article, props.attachments);
  const excerpt = (): string => {
    try {
      return wasmSync().articleExcerpt(current().article);
    } catch {
      return "";
    }
  };
  const firstImage = (): AttachmentRefBytes | undefined => {
    const c = current();
    for (const b of c.article.blocks) {
      if (b.kind === "image") return c.attachments[b.attachmentIndex];
    }
    return undefined;
  };
  const [banner] = createResource(firstImage, async (ref) => {
    try {
      return await props.host.hub.load(ref);
    } catch {
      return undefined;
    }
  });
  return (
    <ArticleCard
      article={current().article}
      excerpt={excerpt()}
      bannerUrl={banner() ?? undefined}
      onOpen={() => {
        if (props.messageId === undefined) return;
        const c = current();
        props.host.openArticle({
          messageId: props.messageId,
          article: c.article,
          attachments: c.attachments,
          authorKeyB64: props.authorKeyB64,
          createdAt: props.createdAt,
          own: props.own,
        });
      }}
    />
  );
}

// ----- the message menu ------------------------------------------------

export interface ContentMenuAction {
  readonly label: string;
  readonly icon: IconName;
  readonly onClick: () => void;
}

/** The content's own rows of the message menu (UX 7.3, 8.2, 9.3, 11.3). */
export function contentMenuActions(args: {
  readonly host: ContentHost;
  readonly content: MessageContent;
  readonly messageId: number | undefined;
  readonly attachments: readonly AttachmentRefBytes[];
  readonly own: boolean;
  readonly authorKeyB64: string | undefined;
}): ContentMenuAction[] {
  const { host, content, messageId } = args;
  const out: ContentMenuAction[] = [];
  if (content.kind === "location") {
    const loc: SharedLocation = content.location;
    out.push({ label: t("location.open_osm"), icon: "globe", onClick: () => host.openUrl(osm(loc)) });
    out.push({
      label: t("location.copy"),
      icon: "copy",
      onClick: () => host.copyText(formatCoordinates(loc.latE7, loc.lonE7, displayLocale())),
    });
    return out;
  }
  if (messageId === undefined || !host.canAct()) return out;
  const isAuthor = args.own || (args.authorKeyB64 !== undefined && args.authorKeyB64 === host.ownKeyB64());
  if (content.kind === "poll") {
    const e = entryOf(host.groupIdB64, messageId);
    if (e?.kind !== "poll") return out;
    const tally = foldPollEntry(e, Date.now());
    const queued = queuedBallotOf(host.groupIdB64, messageId);
    const ownChoice =
      queued?.choices ??
      (e.signed.body.anonymous != null
        ? ownPollChoice(2, host.groupId(), messageId)?.choices
        : tally?.choicesByVoter?.find((v) => v.voter === host.ownKeyB64())?.choices);
    const view = pollViewOf({
      body: e.signed.body,
      channel: false,
      verified: e.verified,
      tally,
      ownChoice,
      sending: queued?.state === "waiting",
      failed: queued?.state === "failed",
      canAct: true,
      nowMs: Date.now(),
    });
    if (canRetract(view)) {
      out.push({ label: t("poll.retract"), icon: "xmark", onClick: () => void votePoll(host, e, []) });
    }
    if (canClosePoll({ isAuthor, isAdmin: host.isAdmin(), group: !host.oneToOne(), closed: view.closed })) {
      out.push({
        label: t("poll.close"),
        icon: "lock",
        onClick: () => void closeContent(host, messageId, "poll"),
      });
    }
    return out;
  }
  if (content.kind === "checklist") {
    const e = entryOf(host.groupIdB64, messageId);
    if (e?.kind !== "checklist" || !isAuthor || e.closes.length > 0) return out;
    out.push({
      label: t("checklist.edit"),
      icon: "pencil",
      onClick: () => host.editContent(messageId, { kind: "checklist", checklist: e.currentSigned ?? e.signed }, args.attachments),
    });
    out.push({ label: t("checklist.close"), icon: "lock", onClick: () => void closeContent(host, messageId, "checklist") });
    return out;
  }
  if (content.kind === "article" && isAuthor) {
    const c = currentArticle(host, messageId, args.authorKeyB64, args.own, content.article, args.attachments);
    out.push({
      label: t("chat.message.edit"),
      icon: "pencil",
      onClick: () => host.editContent(messageId, { kind: "article", article: c.article }, c.attachments),
    });
  }
  return out;
}

function osm(loc: SharedLocation): string {
  const lat = (loc.latE7 / 1e7).toFixed(5);
  const lon = (loc.lonE7 / 1e7).toFixed(5);
  return `https://www.openstreetmap.org/?mlat=${lat}&mlon=${lon}#map=17/${lat}/${lon}`;
}

/** `Close poll` / `Close checklist`: asked first, final. */
async function closeContent(host: ContentHost, messageId: number, kind: "poll" | "checklist"): Promise<void> {
  const ok = await requestConfirm({
    title: kind === "poll" ? t("poll.close_confirm") : t("checklist.close_confirm"),
    body: null,
    confirmLabel: kind === "poll" ? t("poll.close") : t("checklist.close"),
    cancelLabel: t("common.cancel"),
    tone: "danger",
  });
  if (!ok) return;
  try {
    const sent = await host.sendOp(wasmSync().composePollCloseEnvelope(BigInt(messageId)));
    const own = host.ownKeyB64();
    if (own === undefined) return;
    const op = heldOpOf({ op: "poll-close", target: messageId }, sent.messageId, Date.now(), own, host.isAdmin());
    if (op !== null) await updateContent(host.groupIdB64, (c) => withOp(c, op));
  } catch (err) {
    console.warn("[attach] close not sent", err instanceof Error ? err.name : "unknown");
    host.onError(t("chat.send.failed"));
  }
}
