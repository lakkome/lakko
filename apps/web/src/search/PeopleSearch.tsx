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
 * **Request sent**; anyone else shows their name only.
 *
 * In Contacts (`scope: "contacts"`) only people are matched: the contacts
 * list above already shows the contacts, so they are not repeated here, and
 * no channel handle is asked.
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
import { recordSentRequest } from "../contacts/sent-requests";
import { t } from "../l10n";
import { useProfiles } from "../profile/ProfileContext";
import { reachSession } from "../reach/reach-service";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { loadWasm, type Wasm } from "../wasm";
import { AutoSearch, networkQueryOf, SearchMemory, type NetworkQuery, type NetworkView } from "./auto-search";
import { canAddContact, localPeople, mergePeople, type PersonResult } from "./people";
import { searchPeople } from "./people-search";
import { rowStateAfter, rowView, type RowSendState } from "./row-send";
import { REQUEST_FAILED, REQUEST_SENT, searchStateMessage } from "./search-state";
import { sendFoundRequest, type SendOutcome } from "./send-request";
import { canonicalUsernameOf, localCandidates, normalizerOf, peopleSearchDeps, sendDeps } from "./wiring";
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";

/** Where the field is: Chats matches conversations too, Contacts only people. */
export type PeopleSearchScope = "chats" | "contacts";

/** The answers of this session, shared by both fields (keys carry the
 *  account and the scope). */
const sessionMemory = new SearchMemory();

/** Each found person's row state for this session, by account and person
 *  key, so "Request sent" survives the list being redrawn as the person
 *  types, and moving between Chats and Contacts. */
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
  /** A request went out (Contacts refreshes its Sent list). */
  readonly onSent?: () => void;
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

  /** The local half, as the person types. */
  const local = createMemo((): PersonResult[] => {
    const w = wasm();
    const q = props.query();
    if (w === null || q.trim().length === 0) return [];
    profiles.version();
    return localPeople(localCandidates(profiles, props.groupPeopleKeys()), q, normalizerOf(w), ownKeyHex());
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

  const people = (): PersonResult[] => {
    const found = network().outcome?.people ?? [];
    const merged = mergePeople([local(), found], ownKeyHex());
    // The contacts list above already shows the contacts.
    return inContacts ? merged.filter((p) => p.reach !== "contact") : merged;
  };

  const channel = () => (inContacts ? null : (network().outcome?.channel ?? null));
  const searching = (): boolean => network().searching;
  /** The line under People; none while a new question is out. */
  const message = (): string | null => {
    if (searching()) return null;
    const outcome = network().outcome;
    return outcome === null ? null : searchStateMessage(outcome.state);
  };
  const showsPeople = (): boolean =>
    people().length > 0 || channel() !== null || searching() || message() !== null;

  const nothingLocal = (): boolean =>
    (props.conversationMatches?.() ?? 0) === 0 && local().length === 0 && channel() === null;

  return (
    <>
      <Show when={!inContacts && nothingLocal() && network().outcome?.state.kind !== "found"}>
        <p class="lk-pane-nomatch">{t("chats.search.no_results")}</p>
      </Show>
      <Show when={showsPeople()}>
        <div class="lk-section-label" style={{ display: "flex", "align-items": "center", gap: "8px" }}>
          <span>{t("search.people.heading")}</span>
          <Show when={searching()}>
            <span class="lakko-spinner" role="status" aria-label={t("search.people.searching_a11y")} />
          </Show>
        </div>
        <For each={people()}>{(p) => <PersonRow person={p} wasm={wasm} chats={chats} onSent={props.onSent} openState={contactOpenState} />}</For>
        <Show when={channel()}>
          {(c) => (
            <button
              type="button"
              class="lk-pane-lookup"
              onClick={() => props.onOpenChannel?.(c().idHex, c().handle)}
            >
              <Icon name="channels" size={16} />
              {t("search.people.channel_result", { handle: `@${c().handle}` })}
            </button>
          )}
        </Show>
        <Show when={message()}>
          {(line) => (
            <p class="lk-pane-nomatch" role="status">
              {line()}
            </p>
          )}
        </Show>
      </Show>
    </>
  );
}

function PersonRow(props: {
  readonly person: PersonResult;
  readonly wasm: () => Wasm | null;
  readonly chats: ReturnType<typeof useChats>;
  readonly onSent?: (() => void) | undefined;
  /** The history state the contact screen is opened with (its way back). */
  readonly openState: () => ContactBackState | undefined;
}): JSX.Element {
  const auth = useAuth();
  const profiles = useProfiles();

  const view = () => rowView(rowStateOf(props.person.keyHex), canAddContact(props.person));
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
      // The Sent list shows `@username`; a person found by name alone has
      // none to show there, so only the row says it went.
      if (account !== null && person.username !== null && person.accountIdHex !== null) {
        recordSentRequest(account, {
          recipientUsername: person.username,
          recipientAccountIdHex: person.accountIdHex,
          sentAtMs: Date.now(),
          recipientKeyB64: person.keyB64,
        });
      }
      props.onSent?.();
    }
    setRowState(person.keyHex, rowStateAfter(outcome));
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
          <Show when={props.person.username}>
            {(u) => (
              <div class="lk-row-line">
                <span class="lk-row-sub">@{u()}</span>
              </div>
            )}
          </Show>
        </div>
        <Show when={props.person.reach === "contact"}>
          <A href={contactHref()} state={props.openState()} class="lk-pane-lookup" style={{ width: "auto" }}>
            {t("common.open")}
          </A>
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
