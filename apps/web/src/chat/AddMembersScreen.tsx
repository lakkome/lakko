// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Add members screen (`docs/ux/conversation-info.md` section 2.4), pushed
 * over a group's info.
 *
 * Top bar: back icon, the title, and **Add** (**Add {count}** with two or
 * more picks), disabled with none. Under it a search field; the picked people
 * as removable chips; the contacts with a checkbox; and, from three
 * characters, what the network finds under People - the same search the
 * Chats and Contacts field runs (`search/people-search.ts` through
 * `search/auto-search.ts`), with its People lines word for word, and a
 * checkbox in place of Add contact.
 *
 * Every decision is in `chat/add-members.ts`; this renders it. The add
 * itself is the conversation's (`routes/Chat.tsx`), which receives the picks.
 */
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { t } from "../l10n";
import { useProfiles } from "../profile/ProfileContext";
import { reachSession } from "../reach/reach-service";
import { AutoSearch, networkQueryOf, SearchMemory, type NetworkView } from "../search/auto-search";
import { matchesLoosely, type PersonResult } from "../search/people";
import { searchPeople } from "../search/people-search";
import { searchStateMessage } from "../search/search-state";
import { canonicalUsernameOf, normalizerOf, peopleSearchDeps } from "../search/wiring";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { loadWasm, type Wasm } from "../wasm";
import {
  contactRows,
  livePicks,
  personRows,
  pickOfContact,
  pickOfPerson,
  submitLabel,
  togglePick,
  type AddPick,
  type ContactCandidate,
  type Excluded,
} from "./add-members";

/** The answers of this session's Add members searches. */
const memory = new SearchMemory();

export function AddMembersScreen(props: {
  /** The account's contacts, in list order. */
  readonly contacts: () => readonly ContactCandidate[];
  /** Who is out of both lists right now. */
  readonly excluded: () => Excluded;
  /** Back to the group's info, adding nobody. */
  readonly onBack: () => void;
  /** Add these people; the screen is closed by the caller. */
  readonly onAdd: (picks: readonly AddPick[]) => void;
}): JSX.Element {
  const auth = useAuth();
  const profiles = useProfiles();
  const [wasm, setWasm] = createSignal<Wasm | null>(null);
  void loadWasm().then(setWasm);

  const [query, setQuery] = createSignal("");
  const [picks, setPicks] = createSignal<readonly AddPick[]>([]);
  let field: HTMLInputElement | undefined;
  onMount(() => field?.focus());

  const matches = (text: string, q: string): boolean => {
    const w = wasm();
    if (w === null) return text.toLowerCase().includes(q.toLowerCase());
    return matchesLoosely(text, q, normalizerOf(w));
  };

  const rows = createMemo(() => contactRows(props.contacts(), query(), props.excluded(), matches));
  const contactKeys = createMemo(() => new Set(props.contacts().map((c) => c.keyB64)));

  // A person who joined, or whose add started elsewhere, while this screen is
  // open drops out of the picks rather than being added twice.
  createEffect(
    on(props.excluded, (excluded) => {
      const live = livePicks(picks(), excluded);
      if (live.length !== picks().length) setPicks(live);
    }),
  );
  const picked = (keyB64: string): boolean => picks().some((p) => p.keyB64 === keyB64);
  const toggle = (pick: AddPick | null): void => {
    if (pick !== null) setPicks(togglePick(picks(), pick));
  };

  // ---- the network half: the search field's own, asked as the person types
  const [network, setNetwork] = createSignal<NetworkView>({ outcome: null, searching: false });
  const auto = new AutoSearch({
    memory,
    onChange: setNetwork,
    run: async (q) => {
      const w = wasm();
      if (w === null) return { people: [], channel: null, state: { kind: "failed" } };
      // People only: a channel cannot be added to a group.
      const { lookupChannel: _channel, ...peopleOnly } = peopleSearchDeps({
        wasm: w,
        profiles,
        sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
      });
      return searchPeople(q, peopleOnly);
    },
  });
  onCleanup(() => auto.dispose());
  createEffect(
    on(
      () => {
        const w = wasm();
        if (w === null) return null;
        const asked = networkQueryOf(query(), {
          canonicalUsername: (typed) => canonicalUsernameOf(w, typed),
          queryTerm: (q) => w.nameQueryTerm(q) ?? null,
          normalize: normalizerOf(w),
        });
        if (asked === null) return null;
        const account = reachSession()?.accountIdB64 ?? "";
        return { query: asked.query, key: `${account}\u0000add-members\u0000${asked.key}` };
      },
      (next) => auto.input(next),
    ),
  );
  const people = createMemo(() =>
    personRows((network().outcome?.people ?? []) as readonly PersonResult[], contactKeys(), props.excluded()),
  );
  const searching = (): boolean => network().searching;
  const peopleLine = (): string | null => {
    if (searching()) return null;
    const outcome = network().outcome;
    return outcome === null ? null : searchStateMessage(outcome.state);
  };
  const showsPeople = (): boolean => people().length > 0 || searching() || peopleLine() !== null;

  const label = (): string => {
    const l = submitLabel(picks().length);
    return l.kind === "count" ? t("chat.members.add.submit_count", { count: l.count }) : t("chat.members.add.submit");
  };

  return (
    <section class="lk-info-screen" aria-labelledby="lk-add-members-title">
      <header class="lk-chat-head lk-info-head">
        <button
          type="button"
          class="lk-icon-button"
          aria-label={t("common.back")}
          title={t("common.back")}
          onClick={() => props.onBack()}
        >
          <Icon name="chevron-back" size={24} />
        </button>
        <h1 id="lk-add-members-title" class="lk-info-title">
          {t("chat.members.add.title")}
        </h1>
        <button
          type="button"
          class="primary small"
          disabled={picks().length === 0}
          onClick={() => props.onAdd(picks())}
        >
          {label()}
        </button>
      </header>
      <div class="lk-info-scroll">
        <div class="lk-search" style={{ margin: "0 0 4px" }}>
          <Icon name="search" size={20} />
          <input
            ref={field}
            type="search"
            autocomplete="off"
            placeholder={t("chat.members.add.search_placeholder")}
            aria-label={t("chat.members.add.search_placeholder")}
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
          />
        </div>
        <Show when={picks().length > 0}>
          <ul class="lk-pick-chips">
            <For each={picks()}>
              {(p) => (
                <li class="lk-pick-chip">
                  <Avatar label={p.label} id={p.keyB64} size="sm" personKey={p.keyB64} />
                  <span class="lk-ellipsis">{p.label}</span>
                  <button
                    type="button"
                    class="lk-icon-button"
                    aria-label={t("chat.members.remove.confirm_named", { name: p.label })}
                    title={t("chat.members.remove.confirm_named", { name: p.label })}
                    onClick={() => toggle(p)}
                  >
                    <Icon name="xmark" size={14} />
                  </button>
                </li>
              )}
            </For>
          </ul>
        </Show>
        <For each={rows()}>
          {(row) => (
            <label class="lk-row pick" style={{ opacity: row.pickable ? 1 : 0.55 }}>
              <input
                type="checkbox"
                checked={picked(row.contact.keyB64)}
                disabled={!row.pickable}
                onChange={() => toggle(pickOfContact(row.contact))}
              />
              <Avatar label={row.contact.name} id={row.contact.keyB64} personKey={row.contact.keyB64} />
              <div class="lk-row-body">
                <span class="lk-row-title">
                  <span class="lk-ellipsis">{row.contact.name}</span>
                </span>
                <Show
                  when={row.pickable}
                  fallback={<span class="lk-row-sub">{t("chats.new_group.accept_again")}</span>}
                >
                  <Show when={row.contact.username}>{(u) => <span class="lk-row-sub">{u()}</span>}</Show>
                </Show>
              </div>
            </label>
          )}
        </For>
        <Show when={showsPeople()}>
          <div class="lk-section-label" style={{ display: "flex", "align-items": "center", gap: "8px", padding: "12px 12px 4px" }}>
            <span>{t("search.people.heading")}</span>
            <Show when={searching()}>
              <span class="lakko-spinner" role="status" aria-label={t("search.people.searching_a11y")} />
            </Show>
          </div>
          <For each={people()}>
            {(row) => (
              <label class="lk-row pick" style={{ opacity: row.pickable ? 1 : 0.55 }}>
                <Show when={row.pickable} fallback={<span style={{ width: "18px", "flex-shrink": 0 }} />}>
                  <input
                    type="checkbox"
                    checked={picked(row.person.keyB64)}
                    onChange={() => toggle(pickOfPerson(row.person))}
                  />
                </Show>
                <Avatar label={row.person.name} id={row.person.keyB64} personKey={row.person.keyB64} />
                <div class="lk-row-body">
                  <span class="lk-row-title">
                    <span class="lk-ellipsis">{row.person.name}</span>
                  </span>
                  <Show when={row.person.username}>{(u) => <span class="lk-row-sub">@{u()}</span>}</Show>
                </div>
              </label>
            )}
          </For>
          <Show when={peopleLine()}>
            {(line) => (
              <p class="lk-pane-nomatch" role="status">
                {line()}
              </p>
            )}
          </Show>
        </Show>
      </div>
    </section>
  );
}
