// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * People in the Chats search field (`docs/ux/who-can-find-me.md` section 6.1,
 * `docs/ux/navigation.md` section 4): the people this device already knows,
 * matched as the person types, then one **Search people** row that asks the
 * network and adds what it finds to the same list.
 *
 * A row shows avatar, name and a verified `@username`, and never WHY the
 * person was found. Its action: a contact opens their page; someone who lets
 * this account find them offers **Add contact**, which asks for an optional
 * introduction and sends the request with the key that found them; anyone
 * else shows their name only.
 *
 * Every decision is in the plain modules beside this file; this renders them.
 */
import { createEffect, createMemo, createSignal, For, on, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64UrlEncodeNoPad, base64Decode } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { ContactsClient } from "../contacts/client";
import { recordSentRequest } from "../contacts/sent-requests";
import { rememberSentIntro } from "../contacts/sent-request-intros";
import { t } from "../l10n";
import { useProfiles } from "../profile/ProfileContext";
import { reachSession } from "../reach/reach-service";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { loadWasm, type Wasm } from "../wasm";
import { canAddContact, localPeople, mergePeople, type PersonResult } from "./people";
import { searchPeople, type PeopleSearchOutcome } from "./people-search";
import { REQUEST_REFUSED, searchStateMessage, type PeopleSearchState } from "./search-state";
import { sendFoundRequest } from "./send-request";
import { localCandidates, normalizerOf, peopleSearchDeps, sendDeps } from "./wiring";

export function PeopleSearch(props: {
  /** What was typed in the search field. */
  readonly query: () => string;
  /** How many conversations the local filter matched. */
  readonly conversationMatches: () => number;
  /** The account keys (base64) of everyone in this device's groups. */
  readonly groupPeopleKeys: () => readonly string[];
  /** Open a channel's subscribe page. */
  readonly onOpenChannel: (channelIdHex: string, handle: string) => void;
}): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();

  const [wasm, setWasm] = createSignal<Wasm | null>(null);
  void loadWasm().then(setWasm);

  const ownKeyHex = (): string | null => reachSession()?.accountKeyHex ?? null;

  /** The local half, as the person types. */
  const local = createMemo((): PersonResult[] => {
    const w = wasm();
    const q = props.query();
    if (w === null || q.trim().length === 0) return [];
    profiles.version();
    return localPeople(localCandidates(profiles, props.groupPeopleKeys()), q, normalizerOf(w), ownKeyHex());
  });

  // The network half belongs to the query it was asked for.
  const [network, setNetwork] = createSignal<{
    readonly query: string;
    readonly outcome: PeopleSearchOutcome | null;
  } | null>(null);
  createEffect(on(props.query, () => setNetwork(null), { defer: true }));

  const state = (): PeopleSearchState => {
    const n = network();
    if (n === null || n.query !== props.query()) return { kind: "idle" };
    return n.outcome === null ? { kind: "searching" } : n.outcome.state;
  };

  const offersSearch = (): boolean => {
    const w = wasm();
    if (w === null) return false;
    const normalised = w.normalizeNameForSearch(props.query());
    return normalised !== undefined && [...normalised].length >= w.nameSearchMinChars();
  };

  const people = (): PersonResult[] => {
    const n = network();
    const found = n !== null && n.query === props.query() && n.outcome !== null ? n.outcome.people : [];
    return mergePeople([local(), found], ownKeyHex());
  };

  const channel = () => {
    const n = network();
    return n !== null && n.query === props.query() ? (n.outcome?.channel ?? null) : null;
  };

  async function search(): Promise<void> {
    const w = wasm();
    const q = props.query();
    if (w === null || state().kind === "searching") return;
    setNetwork({ query: q, outcome: null });
    const outcome = await searchPeople(
      q,
      peopleSearchDeps({
        wasm: w,
        profiles,
        sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
      }),
    );
    setNetwork((cur) => (cur !== null && cur.query === q ? { query: q, outcome } : cur));
  }

  const nothingLocal = (): boolean =>
    props.conversationMatches() === 0 && local().length === 0 && channel() === null;

  return (
    <>
      <Show when={nothingLocal() && state().kind !== "found"}>
        <p class="lk-pane-nomatch">{t("chats.search.no_results")}</p>
      </Show>
      <Show when={people().length > 0}>
        <div class="lk-section-label">{t("search.people.heading")}</div>
        <For each={people()}>{(p) => <PersonRow person={p} wasm={wasm} chats={chats} />}</For>
      </Show>
      <Show when={channel()}>
        {(c) => (
          <button
            type="button"
            class="lk-pane-lookup"
            onClick={() => props.onOpenChannel(c().idHex, c().handle)}
          >
            <Icon name="channels" size={16} />
            {t("search.people.channel_result", { handle: `@${c().handle}` })}
          </button>
        )}
      </Show>
      <Show when={offersSearch()}>
        <button
          type="button"
          class="lk-pane-lookup"
          disabled={state().kind === "searching"}
          aria-busy={state().kind === "searching"}
          onClick={() => void search()}
        >
          <Icon name="search" size={16} />
          <span style={{ flex: "1" }}>{t("search.people.search")}</span>
          <Show
            when={state().kind === "searching"}
            fallback={<Icon name="chevron" size={16} />}
          >
            <span class="lakko-spinner" role="status" aria-label={t("search.people.searching_a11y")} />
          </Show>
        </button>
      </Show>
      <Show when={searchStateMessage(state())}>
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
}): JSX.Element {
  const auth = useAuth();
  const profiles = useProfiles();
  const [adding, setAdding] = createSignal(false);
  const [intro, setIntro] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [note, setNote] = createSignal<{
    readonly error: boolean;
    readonly text: () => string;
  } | null>(null);
  const [sent, setSent] = createSignal(false);

  const contactHref = (): string => paths.contact(base64UrlEncodeNoPad(base64Decode(props.person.keyB64)));

  async function send(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const w = props.wasm();
    if (w === null || busy()) return;
    setBusy(true);
    setNote(null);
    const client = new ContactsClient(undefined, undefined, {
      identityProvider: () => props.chats.identity(),
      accountIdentityProvider: () => props.chats.accountIdentity(),
      onPersist: async () => {
        const s = auth.state();
        if (s.kind !== "session") return;
        await props.chats.persistSnapshot(s.account.accountId);
      },
      ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
      onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
    });
    try {
      const outcome = await sendFoundRequest(
        props.person,
        new TextEncoder().encode(intro()),
        sendDeps({
          wasm: w,
          client,
          identity: () => auth.identitySecrets(),
          peerLabel: props.person.name,
        }),
      );
      switch (outcome.kind) {
        case "sent": {
          const account = reachSession()?.accountIdB64 ?? null;
          // The Sent list shows `@username`; a person found by name alone has
          // none to show there, so only the notice below says it went.
          if (account !== null && props.person.username !== null && props.person.accountIdHex !== null) {
            recordSentRequest(account, {
              recipientUsername: props.person.username,
              recipientAccountIdHex: props.person.accountIdHex,
              sentAtMs: Date.now(),
            });
            void rememberSentIntro(account, props.person.accountIdHex, intro());
          }
          setSent(true);
          setAdding(false);
          const name = props.person.name;
          setNote({ error: false, text: () => t("contacts.send.done", { name }) });
          break;
        }
        case "refused":
          setNote({ error: true, text: REQUEST_REFUSED });
          break;
        case "failed":
          setNote({ error: true, text: outcome.message });
          break;
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <div>
      <div class="lk-row">
        <Avatar label={props.person.name} id={props.person.keyB64} />
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
          <A href={contactHref()} class="lk-pane-lookup" style={{ width: "auto" }}>
            {t("common.open")}
          </A>
        </Show>
        <Show when={canAddContact(props.person) && !sent()}>
          <button
            type="button"
            class="lk-pane-lookup"
            style={{ width: "auto" }}
            aria-expanded={adding()}
            onClick={() => setAdding((v) => !v)}
          >
            {t("search.people.add_contact")}
          </button>
        </Show>
      </div>
      <Show when={adding()}>
        <form class="lk-stack" style={{ gap: "8px", padding: "4px 12px 12px" }} onSubmit={(e) => void send(e)}>
          <label>
            {t("contacts.add.intro_label")}
            <textarea
              rows={2}
              value={intro()}
              onInput={(e) => setIntro(e.currentTarget.value)}
              disabled={busy()}
            />
          </label>
          <div class="lk-actions">
            <button type="button" disabled={busy()} onClick={() => setAdding(false)}>
              {t("common.cancel")}
            </button>
            <button class="primary" type="submit" disabled={busy()}>
              <Icon name="send" size={18} />
              {busy() ? t("common.sending") : t("search.people.send_request")}
            </button>
          </div>
        </form>
      </Show>
      <Show when={note()}>
        {(n) => (
          <div class={n().error ? "form-error" : "form-info"} role="status" style={{ margin: "0 12px 8px" }}>
            {n().text()}
          </div>
        )}
      </Show>
    </div>
  );
}
