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
import { Icon } from "../ui/icons";
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
import { peopleLineOf, REQUEST_FAILED, REQUEST_SENT, showsNoMatchLine } from "./search-state";
import { sendFoundRequest, type SendOutcome } from "./send-request";
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
  readonly onOpenChannel?: (channelIdHex: string, handle: string) => void;
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
}): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();

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
        const { lookupChannel: _channel, ...peopleOnly } = deps;
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
    return peopleRows(all, props.listedB64?.() ?? new Set(), props.standingAboveB64?.() ?? new Set());
  };

  const channel = () => (inContacts ? null : (network().outcome?.channel ?? null));
  const searching = (): boolean => network().searching;
  /** The line under People; none while a new question is out. */
  const message = (): string | null => {
    if (searching()) return null;
    const outcome = network().outcome;
    // Counted before a contact leaves for the list above: "not found" would
    // contradict the person standing there.
    return outcome === null ? null : peopleLineOf(outcome.state, merged().length);
  };
  const showsPeople = (): boolean => people().length > 0 || searching() || message() !== null;

  /** The line that nothing matches, when nothing stands in the field. */
  const noMatchLine = (): string | null => {
    const nothing = showsNoMatchLine({
      rowsAbove: (props.conversationMatches?.() ?? 0) + (props.listedB64?.().size ?? 0),
      peopleRows: people().length,
      channel: channel() !== null,
      searchOut: searching() || network().waiting === true,
    });
    if (!nothing) return null;
    return props.noMatchLine === undefined ? t("chats.search.no_results") : props.noMatchLine();
  };

  return (
    <>
      <Show when={noMatchLine()}>{(line) => <p class="lk-pane-nomatch">{line()}</p>}</Show>
      <Show when={showsPeople()}>
        <div class="lk-section-label" style={{ display: "flex", "align-items": "center", gap: "8px" }}>
          <span>{t("search.people.heading")}</span>
          <Show when={searching()}>
            <span class="lakko-spinner" role="status" aria-label={t("search.people.searching_a11y")} />
          </Show>
        </div>
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
        <Show when={message()}>
          {(line) => (
            <p class="lk-pane-nomatch" role="status">
              {line()}
            </p>
          )}
        </Show>
      </Show>
      <Show when={channel()}>
        {(c) => (
          <>
            <div class="lk-section-label">{t("nav.channels")}</div>
            <button
              type="button"
              class="lk-pane-lookup"
              onClick={() => props.onOpenChannel?.(c().idHex, c().handle)}
            >
              <Icon name="channels" size={16} />
              {props.channelNameOf?.(c().idHex) ?? `@${c().handle}`}
            </button>
          </>
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
    if (outcome.kind === "sent") {
      const account = reachSession()?.accountIdB64 ?? null;
      if (account !== null && person.accountIdHex !== null) {
        recordSentRequest(account, {
          recipientUsername: person.username ?? "",
          recipientName: person.name,
          recipientAccountIdHex: person.accountIdHex,
          sentAtMs: Date.now(),
          recipientKeyB64: person.keyB64,
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
        <Show when={view().add !== "hidden"}>
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
