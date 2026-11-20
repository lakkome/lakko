// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * People in the search field (`docs/ux/who-can-find-me.md` section 6.1,
 * "Searching: one field"), at the top of Chats and of Contacts: the people
 * this device already knows, matched as the person types, and - from three
 * characters, after half a second without typing, with nothing to press -
 * what the network finds by username and by name, in the same list under
 * **People** (`auto-search.ts`).
 *
 * A row shows avatar, name and a verified `@username`, and never WHY the
 * person was found. Its action: a contact shows **Open**; someone who lets
 * this account find them offers **Add contact**, which sends the request at
 * once with the key that found them and no introduction text, then reads
 * **Request sent**; anyone else shows their name only. A kept grant counts
 * only once the Delivery Service said it still reaches the person
 * (`reach-verdicts.ts`).
 *
 * In Contacts (`scope: "contacts"`) only people are matched: the contacts
 * list above already shows the contacts, so they are not repeated here, and
 * no channel handle is asked. A person the list above shows is never repeated
 * here, in either field.
 *
 * Every decision is in the plain modules beside this file; this renders them.
 */
import { accountSuspended } from "../auth/suspension-state";
import { ConversationCard, ConversationRow, foundIdHex, isMemberOf, useLineageIfMounted } from "./ConversationCard";
import type { FoundConversation } from "./people-search";
import { createEffect, createMemo, createSignal, For, on, onCleanup, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64UrlEncodeNoPad, base64Decode } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { contactBackState, type ContactBackState } from "../chat/info-navigation";
import { ContactsClient } from "../contacts/client";
import { loadAccepted, subscribeAcceptedChanged } from "../contacts/local-store";
import { loadSentRequests, recordSentRequest, sentRequestsVersion } from "../contacts/sent-requests";
import { t } from "../l10n";
import { useProfiles } from "../profile/ProfileContext";
import { reachSession } from "../reach/reach-service";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { loadWasm, type Wasm } from "../wasm";
import { AutoSearch, networkQueryOf, SearchMemory, type NetworkQuery, type NetworkView } from "./auto-search";
import {
  asContacts,
  canAddContact,
  localPeople,
  mergePeople,
  namesFormerContact,
  peopleRows,
  type ContactSet,
  type LocalCandidate,
  type PersonResult,
} from "./people";
import { searchPeople } from "./people-search";
import { candidatesMatching, ReachVerdicts } from "./reach-verdicts";
import {
  heldStateAfter,
  mayAskAgain,
  outgoingOf,
  rowSendState,
  rowView,
  withoutRowLines,
  type OutgoingRequests,
  type RowSendState,
} from "./row-send";
import { REQUEST_FAILED, REQUEST_SENT, searchStateMessage, showsNoMatchLine } from "./search-state";
import { sendFoundRequest, withSharedGroupFallback, type SendOutcome } from "./send-request";
import { sharedGroupsFor } from "../contacts/shared-groups";
import { ownPersonKeyB64 } from "../chat/person-key";
import {
  canonicalUsernameOf,
  localCandidates,
  normalizerOf,
  peopleSearchDeps,
  reachCheckWith,
  sendDeps,
} from "./wiring";
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";
import { pendingContacts } from "../contacts/pending-contacts";
import { isSuspendedKeyB64 } from "../moderation/suspension-marks";
import { withoutSuspended } from "../moderation/suspension-rules";

/** Where the field is: Chats matches conversations too, Contacts only people. */
export type PeopleSearchScope = "chats" | "contacts";

/** The answers of this session, shared by both fields (keys carry the
 *  account and the scope). */
const sessionMemory = new SearchMemory();

/** Each found person's row state for this session, by account and person
 *  key, so a send under way survives the list being redrawn as the person
 *  types, and moving between Chats and Contacts. "Request sent" is not here:
 *  it comes from the Sent list. */
const [rowStates, setRowStates] = createSignal<ReadonlyMap<string, RowSendState>>(new Map());
const rowKeyOf = (keyHex: string): string => `${reachSession()?.accountIdB64 ?? ""}\u0000${keyHex}`;
const rowStateOf = (keyHex: string): RowSendState => rowStates().get(rowKeyOf(keyHex)) ?? "idle";
function setRowState(keyHex: string, state: RowSendState): void {
  setRowStates((cur) => new Map(cur).set(rowKeyOf(keyHex), state));
}

export function PeopleSearch(props: {
  /** What was typed in the search field. */
  readonly query: () => string;
  readonly scope: PeopleSearchScope;
  /** How many conversations the local filter matched (Chats only). */
  readonly conversationMatches?: () => number;
  /** The account keys (base64) of everyone in this device's groups. */
  readonly groupPeopleKeys: () => readonly string[];
  /** Open a channel's subscribe page (Chats only). */
  readonly onOpenChannel?: (channelIdHex: string, handle: string, name?: string, subscribe?: boolean) => void;
  /** The people a list row above shows: the matching contacts and people
   *  added through their link. They leave People whatever their reach. */
  readonly listedB64?: () => ReadonlySet<string>;
  /** The people whose one-to-one is among the matching conversations (Chats).
   *  A contact among them is not listed twice; anyone else keeps the way to
   *  add them. */
  readonly standingAboveB64?: () => ReadonlySet<string>;
  /** The line that nothing matches; `null` for none. Chats' line when absent. */
  readonly noMatchLine?: () => string | null;
  /** The route of the one-to-one with a person, or `null` (Chats). */
  readonly chatHrefOf?: (personKeyB64: string) => string | null;
  /** A channel's name when this device knows one, `null` otherwise. */
  readonly channelNameOf?: (channelIdHex: string) => string | null;
  /** The ids of the conversation rows standing above (Chats): a group's
   *  conversation id, a channel's hex id. A found group or channel among them
   *  is not listed again (one conversation, one row). */
  readonly standingConversationIds?: () => ReadonlySet<string>;
  /** ADR-0087 — open a group this device is in (a found row's `Open`). */
  readonly onOpenConversation?: (groupIdB64: string) => void;
}): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const lineage = useLineageIfMounted();

  const [wasm, setWasm] = createSignal<Wasm | null>(null);
  void loadWasm().then(setWasm);

  /** `docs/ux/conversation-info.md` section 3.1 — a contact opened from the
   *  Chats field covers that screen, so its back control returns here. From
   *  the Contacts field nothing is covered: the list stays beside it. */
  const contactOpenState = (): ContactBackState | undefined =>
    props.scope === "chats"
      ? contactBackState(`${globalThis.location.pathname}${globalThis.location.search}`)
      : undefined;

  const ownKeyHex = (): string | null => reachSession()?.accountKeyHex ?? null;
  const inContacts = props.scope === "contacts";

  // The contact list as it stands now: a person accepted while the text is in
  // the field is a contact at once, before their request leaves the Sent list.
  const [contactsChanged, setContactsChanged] = createSignal(0);
  onCleanup(subscribeAcceptedChanged(() => setContactsChanged((n) => n + 1)));
  const contactSet = createMemo((): ContactSet => {
    contactsChanged();
    const contacts = loadAccepted();
    return {
      keysB64: new Set(contacts.map((c) => c.signaturePubkeyB64)),
      accountIdsHex: new Set(contacts.flatMap((c) => (c.accountIdHex === undefined ? [] : [c.accountIdHex]))),
    };
  });

  /** The people this device knows, as they stand now. */
  const candidates = createMemo((): LocalCandidate[] => {
    if (wasm() === null || props.query().trim().length === 0) return [];
    profiles.version();
    contactsChanged();
    return localCandidates(profiles, props.groupPeopleKeys());
  });

  // ---- whether a kept grant still reaches them ---------------------------
  const [verdictsChanged, setVerdictsChanged] = createSignal(0);
  const verdicts = new ReachVerdicts({
    reachCheck: (keyB64, wireB64) => reachCheckWith(base64Decode(keyB64), wireB64),
    onChange: () => setVerdictsChanged((n) => n + 1),
  });
  // Runs on the text and the candidates, never on a verdict: a check answers
  // for one text, so every change of it asks again, and never in a loop.
  createEffect(() => {
    const w = wasm();
    const held = candidates();
    const text = props.query();
    if (w === null) return;
    if (text.trim().length === 0) {
      verdicts.clear();
      return;
    }
    verdicts.ask(candidatesMatching(held, text, normalizerOf(w), ownKeyHex()), text);
  });

  // A row's refused or failed line lasts until the text changes. Leaving the
  // field is a change of it: the field is unmounted when it is emptied.
  createEffect(on(() => props.query(), () => setRowStates(withoutRowLines), { defer: true }));
  onCleanup(() => setRowStates(withoutRowLines));

  /** The Sent list, which a row's "Request sent" reads. */
  const outgoing = createMemo((): OutgoingRequests => {
    sentRequestsVersion();
    const account = reachSession()?.accountIdB64;
    return outgoingOf(account === undefined ? [] : loadSentRequests(account), pendingContacts());
  });

  /** The local half, as the person types. */
  const local = createMemo((): PersonResult[] => {
    const w = wasm();
    const q = props.query();
    if (w === null || q.trim().length === 0) return [];
    verdictsChanged();
    return localPeople(verdicts.apply(candidates()), q, normalizerOf(w), ownKeyHex());
  });

  // ---- the network half: asked by itself as the person types ------------
  const [network, setNetwork] = createSignal<NetworkView>({ outcome: null, searching: false });
  const auto = new AutoSearch({
    memory: sessionMemory,
    onChange: setNetwork,
    run: async (query) => {
      const w = wasm();
      if (w === null) return { people: [], channel: null, state: { kind: "failed" } };
      const deps = peopleSearchDeps({
        wasm: w,
        profiles,
        sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
      });
      // Contacts matches only people: no channel handle is asked there.
      if (inContacts) {
        const {
          lookupChannel: _channel,
          openConversationRecord: _open,
          conversationRecordOfLookupKey: _byName,
          conversationRecordsOfTermLookupKey: _byTerm,
          ...peopleOnly
        } = deps;
        return searchPeople(query, peopleOnly);
      }
      return searchPeople(query, deps);
    },
  });
  onCleanup(() => auto.dispose());

  const target = (): NetworkQuery | null => {
    const w = wasm();
    if (w === null) return null;
    const asked = networkQueryOf(props.query(), {
      canonicalUsername: (typed) => canonicalUsernameOf(w, typed),
      queryTerm: (q) => w.nameQueryTerm(q) ?? null,
      normalize: normalizerOf(w),
    });
    if (asked === null) return null;
    const account = reachSession()?.accountIdB64 ?? "";
    return { query: asked.query, key: `${account}\u0000${props.scope}\u0000${asked.key}` };
  };
  createEffect(on(target, (next) => auto.input(next)));
  // A contact removed with the text standing: an answer that calls them one
  // is false now, and nothing else would ask again.
  createEffect(
    on(
      contactSet,
      (contacts) => auto.recheck((outcome) => namesFormerContact(outcome.people, contacts)),
      { defer: true },
    ),
  );

  /** Everyone found, a person who became a contact since read as one. */
  const merged = (): PersonResult[] =>
    asContacts(mergePeople([local(), network().outcome?.people ?? []], ownKeyHex()), contactSet());
  const people = (): PersonResult[] => {
    const all = merged();
    // ADR-0089 step 4, UX 3 - search lists no suspended account. Rows show
    // at once; one drops out when the log confirms its suspension.
    return withoutSuspended(
      peopleRows(all, props.listedB64?.() ?? new Set(), props.standingAboveB64?.() ?? new Set()),
      isSuspendedKeyB64,
    );
  };

  const channel = () => (inContacts ? null : (network().outcome?.channel ?? null));
  /** ADR-0087 section 5 — public groups and channels, under People. */
  const conversations = (): readonly FoundConversation[] => {
    if (inContacts) return [];
    const found = network().outcome?.conversations ?? [];
    const standing = props.standingConversationIds?.();
    if (standing === undefined || standing.size === 0) return found;
    return found.filter((f) => !standsAbove(f, standing));
  };
  /** Whether a found group or channel is already a row above: a channel by
   *  its hex id, a group by its own id or the conversation its lineage
   *  belongs to. */
  const standsAbove = (f: FoundConversation, standing: ReadonlySet<string>): boolean => {
    if (f.kind === "channel") return standing.has(foundIdHex(f));
    if (standing.has(f.idB64)) return true;
    const view = lineage?.resolve(f.idB64);
    return view !== undefined && standing.has(view.conversationIdB64);
  };
  const [card, setCard] = createSignal<FoundConversation | null>(null);
  const searching = (): boolean => network().searching;
  /** The line at the end of the one list (`docs/ux/who-can-find-me.md`
   *  section 6.1, "One list"): a network failure or an empty token pool.
   *  "Not found" is never said here: the empty-list line says it, and only
   *  when nothing stands in the list. None while a new question is out. */
  const message = (): string | null => {
    if (searching()) return null;
    const outcome = network().outcome;
    if (outcome === null || outcome.state.kind === "not-found") return null;
    return searchStateMessage(outcome.state);
  };

  /** The line that nothing matches, when nothing stands in the field. */
  const noMatchLine = (): string | null => {
    const nothing = showsNoMatchLine({
      rowsAbove: (props.conversationMatches?.() ?? 0) + (props.listedB64?.().size ?? 0),
      peopleRows: people().length,
      channel: channel() !== null || conversations().length > 0,
      searchOut: searching() || network().waiting === true,
    });
    if (!nothing || message() !== null) return null;
    return props.noMatchLine === undefined ? t("search.nothing_found") : props.noMatchLine();
  };

  return (
    <>
      <Show when={noMatchLine()}>{(line) => <p class="lk-pane-nomatch">{line()}</p>}</Show>
      {/* One list, no headings (UX 6.1 "One list"): people, then groups and
          channels, then the channel a handle names; the spinner and the
          failure line at the end. Each row's second line says what it is. */}
      <For each={people()}>
          {(p) => (
            <PersonRow
              person={p}
              wasm={wasm}
              chats={chats}
              outgoing={outgoing}
              onReachRefused={(keyHex) => verdicts.refused(keyHex)}
              chatHref={props.chatHrefOf?.(p.keyB64) ?? null}
              openState={contactOpenState}
            />
          )}
        </For>
        <For each={conversations()}>
          {(c) => (
            <ConversationRow
              found={c}
              onOpen={(found) => {
                if (isMemberOf(found, chats, lineage)) {
                  if (found.kind === "channel") props.onOpenChannel?.(foundIdHex(found), found.handle, found.name);
                  else props.onOpenConversation?.(found.idB64);
                  return;
                }
                setCard(found);
              }}
            />
          )}
        </For>
      <ConversationCard
        found={card()}
        onBack={() => setCard(null)}
        onOpenConversation={(id) => {
          setCard(null);
          props.onOpenConversation?.(id);
        }}
        onOpenChannel={(found, subscribe) => {
          setCard(null);
          props.onOpenChannel?.(foundIdHex(found), found.handle, found.name, subscribe);
        }}
      />
      {/* The exact-handle row, unless the same channel is already among the
          groups and channels found (that row carries what subscribing needs). */}
      <Show when={channel() !== null && !conversations().some((f) => f.kind === "channel" && foundIdHex(f) === channel()?.idHex) ? channel() : null}>
        {(c) => (
          <button
            type="button"
            class="lk-row"
            onClick={() => props.onOpenChannel?.(c().idHex, c().handle)}
          >
            <Avatar label={props.channelNameOf?.(c().idHex) ?? c().handle} id={c().idHex} />
            <span class="lk-row-body">
              <span class="lk-row-title">{props.channelNameOf?.(c().idHex) ?? `@${c().handle}`}</span>
              <span class="lk-row-sub" dir="auto">
                {t("search.people.channel_result", { handle: `@${c().handle}` })}
              </span>
            </span>
          </button>
        )}
      </Show>
      <Show when={searching()}>
        <div style={{ display: "flex", "justify-content": "center", padding: "12px" }}>
          <span class="lakko-spinner" role="status" aria-label={t("search.people.searching_a11y")} />
        </div>
      </Show>
      <Show when={message()}>
        {(line) => (
          <p class="lk-pane-nomatch" role="status">
            {line()}
          </p>
        )}
      </Show>
    </>
  );
}

function PersonRow(props: {
  readonly person: PersonResult;
  readonly wasm: () => Wasm | null;
  readonly chats: ReturnType<typeof useChats>;
  /** Who the Sent list holds. */
  readonly outgoing: () => OutgoingRequests;
  /** A send was refused by the reach check. */
  readonly onReachRefused: (keyHex: string) => void;
  /** Their conversation, which Open opens (UX 6.1); `null` with none yet. */
  readonly chatHref: string | null;
  /** The history state the contact screen is opened with (its way back). */
  readonly openState: () => ContactBackState | undefined;
}): JSX.Element {
  const auth = useAuth();
  const profiles = useProfiles();

  // A contact has nothing to send: whatever this row said before they became
  // one is over.
  const state = (): RowSendState =>
    props.person.reach === "contact"
      ? "idle"
      : rowSendState(props.person, rowStateOf(props.person.keyHex), props.outgoing());
  const view = () =>
    rowView(state(), canAddContact(props.person), mayAskAgain(props.person, props.outgoing()));
  /** A refusal takes the grant away, and with it what the row may show; the
   *  row keeps the username its card still has while it says so. The next
   *  text asks again, and a person who cannot be reached is a name alone. */
  const username = (): string | null =>
    props.person.username ??
    (state() === "refused"
      ? (profiles.resolve(props.person.keyB64).username?.replace(/^@+/, "") ?? null)
      : null);
  const contactHref = (): string => paths.contact(base64UrlEncodeNoPad(base64Decode(props.person.keyB64)));

  /** Add contact: the request goes out at once - no dialog, no text. */
  async function send(): Promise<void> {
    const person = props.person;
    const w = props.wasm();
    if (w === null || rowStateOf(person.keyHex) === "sending") return;
    setRowState(person.keyHex, "sending");
    const client = new ContactsClient(undefined, undefined, {
      identityProvider: () => props.chats.identity(),
      accountIdentityProvider: () => props.chats.accountIdentity(),
      onPersist: durableSnapshotHook(props.chats, () => sessionAccountOf(auth.state())),
      ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
      onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
    });
    let outcome: SendOutcome;
    try {
      outcome = await sendFoundRequest(
        person,
        sendDeps({
          wasm: w,
          client,
          identity: () => auth.identitySecrets(),
          peerLabel: person.name,
        }),
      );
    } catch {
      outcome = { kind: "failed", message: REQUEST_FAILED };
    }
    // Who-can-find-me 6.4 - the inbox refused: the request goes inside a
    // group both people are in, if there is one.
    const accountIdHex = person.accountIdHex;
    const fallback =
      accountIdHex === null
        ? { outcome, groupIdB64: null }
        : await withSharedGroupFallback(outcome, {
            sharedGroups: () =>
              sharedGroupsFor({
                groups: props.chats.groups(),
                accountIdB64: reachSession()?.accountIdB64 ?? null,
                ownKeyB64: ownPersonKeyB64(props.chats.identity()),
                personKeysB64: [person.keyB64],
              }),
            sendInGroup: async (groupIdB64) => {
              const entry = props.chats.get(groupIdB64);
              if (entry === undefined) throw new Error("the shared group is gone");
              await client.sendContactRequestInGroup(
                auth.identitySecrets(),
                hexToBytes(accountIdHex),
                base64Decode(person.keyB64),
                entry.group,
              );
            },
            errorMessage: () => () => t("contacts.detail.request_failed"),
          });
    outcome = fallback.outcome;
    if (outcome.kind === "sent") {
      const account = reachSession()?.accountIdB64 ?? null;
      if (account !== null && person.accountIdHex !== null) {
        recordSentRequest(account, {
          recipientUsername: person.username ?? "",
          recipientName: person.name,
          recipientAccountIdHex: person.accountIdHex,
          sentAtMs: Date.now(),
          recipientKeyB64: person.keyB64,
          ...(fallback.groupIdB64 === null ? {} : { throughGroupIdB64: fallback.groupIdB64 }),
        });
      }
    }
    if (outcome.kind === "refused" && outcome.byReachCheck === true) props.onReachRefused(person.keyHex);
    setRowState(person.keyHex, heldStateAfter(outcome));
  }

  return (
    <div>
      <div class="lk-row">
        <Avatar label={props.person.name} id={props.person.keyB64} personKey={props.person.keyB64} />
        <div class="lk-row-body">
          <div class="lk-row-line">
            <span class="lk-row-title">
              <span class="lk-ellipsis">{props.person.name}</span>
            </span>
          </div>
          <Show when={username()}>
            {(u) => (
              <div class="lk-row-line">
                <span class="lk-row-sub">@{u()}</span>
              </div>
            )}
          </Show>
        </div>
        <Show when={props.person.reach === "contact"}>
          <Show
            when={props.chatHref}
            fallback={
              // No conversation yet: its Message creates one.
              <A href={contactHref()} state={props.openState()} class="lk-pane-lookup" style={{ width: "auto" }}>
                {t("common.open")}
              </A>
            }
          >
            {(href) => (
              <A href={href()} class="lk-pane-lookup" style={{ width: "auto" }}>
                {t("common.open")}
              </A>
            )}
          </Show>
        </Show>
        {/* ADR-0089 step 3: a suspended account adds no contact. */}
        <Show when={view().add !== "hidden" && !accountSuspended()}>
          <button
            type="button"
            class="lk-pane-lookup"
            style={{ width: "auto" }}
            disabled={view().add === "disabled"}
            aria-busy={view().add === "disabled"}
            onClick={() => void send()}
          >
            {view().add === "disabled" ? t("common.sending") : t("search.people.add_contact")}
          </button>
        </Show>
        <Show when={view().sent}>
          <span class="lk-row-sub" role="status">
            {REQUEST_SENT()}
          </span>
        </Show>
        {/* A decline is silent: nothing else says the first one is over. */}
        <Show when={view().askAgain}>
          <button type="button" class="lk-pane-lookup" style={{ width: "auto" }} onClick={() => void send()}>
            {t("contacts.search.ask_again")}
          </button>
        </Show>
      </div>
      <Show when={view().message}>
        {(n) => (
          <div class={n().error ? "form-error" : "form-info"} role="status" style={{ margin: "0 12px 8px" }}>
            {n().text()}
          </div>
        )}
      </Show>
    </div>
  );
}

/** Lower-case unseparated hex to bytes (the `accountIdHex` shape). */
function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
