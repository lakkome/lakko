// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings > Privacy > Who can find me (`docs/ux/who-can-find-me.md`
 * sections 3, 4, 8 and 9).
 *
 * `WhoCanFindMeSection` is the screen: one choice of three, two switches under
 * Some people, one choice of two under each switch, and a disclosure into each
 * list. `WhoCanFindMeList` is the list of section 4 and
 * `WhoCanFindMeContactsList` the one of section 4.1. Every tap runs
 * a `lakko_ui_rules` transition (`visibility-actions.ts`); what is on screen
 * for a setting, and the copy, is `who-can-find-me.ts`. Nothing here decides
 * a rule.
 */
import { createMemo, createResource, createSignal, For, Show, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";

import { t } from "../l10n";
import { tRich } from "../l10n/rich";
import { reachApplyStatus, reachSession } from "../reach/reach-service";
import { visibilitySetting } from "../reach/visibility-store";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { loadWasm } from "../wasm";
import { accountIdsHexOf } from "./contact-visibility";
import { useFindableContacts } from "./findable-contacts";
import { useFindableScopes } from "./findable-scopes";
import * as actions from "./visibility-actions";
import {
  chosenCount,
  chosenKeysOf,
  contactList,
  contactsOfContactsHelper,
  COPY,
  groupsHelper,
  opensListOnChoose,
  scopeList,
  showsMembersSwitch,
  showsListSearch,
  statusLine,
  viewOf,
  visibleRows,
  type ContactEntry,
  type FormName,
  type ModeName,
  type ScopeEntry,
  type VisibilityView,
} from "./who-can-find-me";

/**
 * UX section 8: after Contacts of my contacts is turned on, the widening
 * travels through the contacts' devices. Whether it has reached them is not
 * known on web (no acknowledgement is counted), so the helper says so for
 * the rest of this page session after the person turns it on here.
 */
const [cocSpreading, setCocSpreading] = createSignal(false);

/** The setting as the screen reads it, `null` while none is loaded. */
function useView(): () => VisibilityView | null {
  return createMemo(() => {
    const s = visibilitySetting();
    return s === null ? null : viewOf(s);
  });
}

/** The keys of the chosen list, freeing the wasm wrappers it reads. */
function useChosenKeys(): () => Set<string> {
  return createMemo(() => {
    const s = visibilitySetting();
    if (s === null) return new Set<string>();
    const chosen = s.chosen;
    try {
      return chosenKeysOf(chosen);
    } finally {
      for (const c of chosen) c.free();
    }
  });
}

/** The account ids of the chosen contacts, as hex. */
function useChosenContacts(): () => Set<string> {
  return createMemo(() => {
    const s = visibilitySetting();
    return new Set(s === null ? [] : accountIdsHexOf(s.chosenContacts));
  });
}

function run(action: () => Promise<unknown>): void {
  void action().catch((err: unknown) => {
    // A refused transition (too many chosen) leaves the setting as it was.
    // eslint-disable-next-line no-console
    console.warn("[privacy] visibility change refused", err);
  });
}

/** The page head with the apply state (UX sections 8 and 9). */
function Head(): JSX.Element {
  const status = (): ReturnType<typeof statusLine> => statusLine(reachApplyStatus());
  return (
    <div class="lk-page-head">
      <div class="lk-privacy-title">
        <h1>{COPY.title()}</h1>
        <Show when={status().spinner}>
          <span class="lakko-spinner" role="status" aria-label={t("who_can_find_me.applying_a11y")} />
        </Show>
      </div>
      <p>
        {t("common.sentence_pair", { first: COPY.lede(), second: COPY.ledeLink() })}
      </p>
      <Show when={status().message}>
        {(message) => (
          <div class="lk-privacy-status" role="status" aria-live="polite">
            <span>{message()}</span>
            <Show when={status().retry}>
              <button type="button" class="ghost small" onClick={() => reachSession()?.requestSync()}>
                {COPY.retry()}
              </button>
            </Show>
          </div>
        )}
      </Show>
    </div>
  );
}

function ModeRow(props: {
  readonly mode: ModeName;
  readonly label: string;
  readonly helper?: string;
  readonly current: ModeName;
}): JSX.Element {
  return (
    <label class="lk-card-row lk-privacy-choice">
      <input
        type="radio"
        name="lk-visibility-mode"
        checked={props.current === props.mode}
        onChange={() => run(() => actions.chooseMode(props.mode))}
      />
      <span class="lk-card-row-body">
        <span class="lk-card-row-title">{props.label}</span>
        <Show when={props.helper}>{(h) => <span class="lk-card-row-sub">{h()}</span>}</Show>
      </span>
    </label>
  );
}

export function WhoCanFindMeSection(): JSX.Element {
  const navigate = useNavigate();
  const view = useView();
  const chosenKeys = useChosenKeys();
  const scopes = useFindableScopes();
  const chosenContacts = useChosenContacts();
  const contacts = useFindableContacts();

  const chosenAmongScopes = (): number => {
    const keys = chosenKeys();
    return chosenCount(scopes(), (e) => keys.has(e.key));
  };
  const chosenAmongContacts = (): number =>
    contacts().filter((c) => chosenContacts().has(c.accountIdHex)).length;

  function onContactsOfContacts(on: boolean): void {
    run(async () => {
      const was = view()?.contactsOfContacts ?? false;
      const next = await actions.setContactsOfContacts(on);
      if (next !== null && on && !was) setCocSpreading(true);
      if (!on) setCocSpreading(false);
    });
  }

  function onForm(form: FormName): void {
    const before = chosenAmongScopes();
    run(async () => {
      const next = await actions.setGroupsForm(form);
      if (next !== null && form === "chosen" && opensListOnChoose(before)) {
        navigate(paths.settingsSection("privacy-groups"));
      }
    });
  }

  function onContactsForm(form: FormName): void {
    const before = chosenAmongContacts();
    run(async () => {
      const next = await actions.setContactsForm(form);
      if (next !== null && form === "chosen" && opensListOnChoose(before)) {
        navigate(paths.settingsSection("privacy-contacts"));
      }
    });
  }

  return (
    <>
      <Head />
      <Show when={view()} fallback={<p class="lk-muted">{t("common.loading")}</p>}>
        {(v) => {
          const rows = (): ReturnType<typeof visibleRows> => visibleRows(v());
          return (
            <section class="lk-card lk-privacy" aria-label={COPY.title()}>
              <div role="radiogroup" aria-label={COPY.title()} class="lk-privacy-group">
                <ModeRow mode="nobody" label={COPY.nobody()} helper={COPY.nobodyHelper()} current={v().mode} />
                <ModeRow mode="some" label={COPY.some()} current={v().mode} />
                <Show when={rows().parts}>
                  <div class="lk-privacy-nested">
                    <label class="lk-card-row lk-privacy-choice">
                      <input
                        type="checkbox"
                        checked={v().contactsOfContacts}
                        onChange={(e) => onContactsOfContacts(e.currentTarget.checked)}
                      />
                      <span class="lk-card-row-body">
                        <span class="lk-card-row-title">{COPY.contactsOfContacts()}</span>
                        <span class="lk-card-row-sub">
                          {contactsOfContactsHelper(v(), chosenAmongContacts(), contacts().length, cocSpreading())}
                        </span>
                      </span>
                    </label>
                    <Show when={rows().contactsForm}>
                      <div
                        class="lk-privacy-nested"
                        role="radiogroup"
                        aria-label={COPY.contactsOfContacts()}
                      >
                        <label class="lk-card-row lk-privacy-choice">
                          <input
                            type="radio"
                            name="lk-visibility-contacts-form"
                            checked={v().contactsForm === "all"}
                            onChange={() => onContactsForm("all")}
                          />
                          <span class="lk-card-row-body">
                            <span class="lk-card-row-title">{COPY.contactsAll()}</span>
                          </span>
                        </label>
                        <div class="lk-card-row lk-privacy-choice">
                          <label class="lk-privacy-choice-label">
                            <input
                              type="radio"
                              name="lk-visibility-contacts-form"
                              checked={v().contactsForm === "chosen"}
                              onChange={() => onContactsForm("chosen")}
                            />
                            <span class="lk-card-row-body">
                              <span class="lk-card-row-title">{COPY.contactsChosen()}</span>
                            </span>
                          </label>
                          <Show when={rows().contactsDisclosure}>
                            <A
                              href={paths.settingsSection("privacy-contacts")}
                              class="lk-card-row-action lk-privacy-disclosure"
                              aria-label={COPY.contactsChosen()}
                            >
                              <Icon name="chevron" size={18} />
                            </A>
                          </Show>
                        </div>
                      </div>
                    </Show>
                    <label class="lk-card-row lk-privacy-choice">
                      <input
                        type="checkbox"
                        checked={v().groups !== null}
                        onChange={(e) => {
                          const on = e.currentTarget.checked;
                          run(() => actions.setGroupsVisible(on));
                        }}
                      />
                      <span class="lk-card-row-body">
                        <span class="lk-card-row-title">{COPY.groups()}</span>
                        <Show when={v().groups !== null}>
                          <span class="lk-card-row-sub">
                            {groupsHelper(v(), chosenAmongScopes(), scopes().length)}
                          </span>
                        </Show>
                      </span>
                    </label>
                    <Show when={rows().groupsForm}>
                      <div
                        class="lk-privacy-nested"
                        role="radiogroup"
                        aria-label={COPY.groups()}
                      >
                        <label class="lk-card-row lk-privacy-choice">
                          <input
                            type="radio"
                            name="lk-visibility-form"
                            checked={v().groups === "all"}
                            onChange={() => onForm("all")}
                          />
                          <span class="lk-card-row-body">
                            <span class="lk-card-row-title">{COPY.all()}</span>
                          </span>
                        </label>
                        <div class="lk-card-row lk-privacy-choice">
                          <label class="lk-privacy-choice-label">
                            <input
                              type="radio"
                              name="lk-visibility-form"
                              checked={v().groups === "chosen"}
                              onChange={() => onForm("chosen")}
                            />
                            <span class="lk-card-row-body">
                              <span class="lk-card-row-title">{COPY.chosen()}</span>
                            </span>
                          </label>
                          <Show when={rows().listDisclosure}>
                            <A
                              href={paths.settingsSection("privacy-groups")}
                              class="lk-card-row-action lk-privacy-disclosure"
                              aria-label={COPY.chosen()}
                            >
                              <Icon name="chevron" size={18} />
                            </A>
                          </Show>
                        </div>
                      </div>
                    </Show>
                  </div>
                </Show>
                <ModeRow
                  mode="everyone"
                  label={COPY.everyone()}
                  helper={COPY.everyoneHelper()}
                  current={v().mode}
                />
              </div>
            </section>
          );
        }}
      </Show>
    </>
  );
}

/** One row of the list: avatar, name, switch. */
function ScopeRow(props: { readonly entry: ScopeEntry; readonly on: boolean }): JSX.Element {
  return (
    <div class="lk-card-row">
      <Avatar label={props.entry.name} id={props.entry.key} size="md" />
      <div class="lk-card-row-body">
        <span class="lk-card-row-title">{props.entry.name}</span>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={props.on}
        aria-label={props.entry.name}
        class="lk-card-row-action"
        classList={{ primary: props.on }}
        onClick={() => run(() => actions.setScopeChosen(props.entry, !props.on))}
      >
        {props.on ? t("common.on") : t("common.off")}
      </button>
    </div>
  );
}

/** The list of groups and channels (UX section 4). */
export function WhoCanFindMeList(): JSX.Element {
  const chosenKeys = useChosenKeys();
  const scopes = useFindableScopes();
  const [query, setQuery] = createSignal("");
  const list = createMemo(() => scopeList(scopes(), query()));
  const isOn = (e: ScopeEntry): boolean => chosenKeys().has(e.key);

  return (
    <>
      <A
        href={paths.settingsSection("privacy")}
        class="ghost small lk-wide-only"
        style={{ "align-self": "flex-start" }}
      >
        <Icon name="back" size={16} />
        {COPY.title()}
      </A>
      <div class="lk-page-head">
        <h1>{COPY.chosen()}</h1>
        <p>{groupsHelper({ groups: "chosen" }, chosenCount(scopes(), isOn), scopes().length)}</p>
      </div>
      <Show
        when={scopes().length > 0}
        fallback={<p class="lk-muted">{COPY.listEmpty()}</p>}
      >
        <Show when={showsListSearch(scopes().length)}>
          <label class="lk-search" style={{ margin: "0" }}>
            <Icon name="search" size={16} />
            <input
              type="search"
              placeholder={COPY.listSearch()}
              aria-label={COPY.listSearch()}
              value={query()}
              onInput={(e) => setQuery(e.currentTarget.value)}
            />
          </label>
        </Show>
        <Show when={list().groups.length > 0}>
          <section class="lk-card" aria-label={COPY.listGroups()}>
            <h2>{COPY.listGroups()}</h2>
            <For each={list().groups}>{(e) => <ScopeRow entry={e} on={isOn(e)} />}</For>
          </section>
        </Show>
        <Show when={list().channels.length > 0}>
          <section class="lk-card" aria-label={COPY.listChannels()}>
            <h2>{COPY.listChannels()}</h2>
            <For each={list().channels}>{(e) => <ScopeRow entry={e} on={isOn(e)} />}</For>
          </section>
        </Show>
        <Show when={list().groups.length === 0 && list().channels.length === 0}>
          <p class="lk-muted">{COPY.listNoMatch()}</p>
        </Show>
      </Show>
    </>
  );
}

/** One row of the list of contacts: avatar, name, switch. */
function ContactRow(props: { readonly entry: ContactEntry; readonly on: boolean }): JSX.Element {
  return (
    <div class="lk-card-row">
      <Avatar label={props.entry.name} id={props.entry.keyB64} personKey={props.entry.keyB64} size="md" />
      <div class="lk-card-row-body">
        <span class="lk-card-row-title">{props.entry.name}</span>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={props.on}
        aria-label={props.entry.name}
        class="lk-card-row-action"
        classList={{ primary: props.on }}
        onClick={() => run(() => actions.setContactChosen(props.entry.accountIdHex, !props.on))}
      >
        {props.on ? t("common.on") : t("common.off")}
      </button>
    </div>
  );
}

/** The list of contacts (UX section 4.1). */
export function WhoCanFindMeContactsList(): JSX.Element {
  const chosen = useChosenContacts();
  const contacts = useFindableContacts();
  const [query, setQuery] = createSignal("");
  const list = createMemo(() => contactList(contacts(), query()));
  const isOn = (e: ContactEntry): boolean => chosen().has(e.accountIdHex);

  return (
    <>
      <A
        href={paths.settingsSection("privacy")}
        class="ghost small lk-wide-only"
        style={{ "align-self": "flex-start" }}
      >
        <Icon name="back" size={16} />
        {COPY.title()}
      </A>
      <div class="lk-page-head">
        <h1>{COPY.contactsChosen()}</h1>
        <p>
          {contactsOfContactsHelper(
            { contactsOfContacts: true, contactsForm: "chosen" },
            contacts().filter(isOn).length,
            contacts().length,
            false,
          )}
        </p>
      </div>
      <Show when={contacts().length > 0} fallback={<p class="lk-muted">{COPY.contactsListEmpty()}</p>}>
        <Show when={showsListSearch(contacts().length)}>
          <label class="lk-search" style={{ margin: "0" }}>
            <Icon name="search" size={16} />
            <input
              type="search"
              placeholder={COPY.contactsListSearch()}
              aria-label={COPY.contactsListSearch()}
              value={query()}
              onInput={(e) => setQuery(e.currentTarget.value)}
            />
          </label>
        </Show>
        <Show when={list().length > 0} fallback={<p class="lk-muted">{COPY.listNoMatch()}</p>}>
          <section class="lk-card" aria-label={COPY.contactsChosen()}>
            <For each={list()}>{(e) => <ContactRow entry={e} on={isOn(e)} />}</For>
          </section>
        </Show>
      </Show>
    </>
  );
}

/**
 * UX section 5: `Members can find me` in group info and channel info. Present
 * only while Members of my groups and channels is on. Under Only the ones I
 * choose it is that scope's switch from the list; under All it shows on, and
 * turning it off excludes this one scope (the setting becomes Only the ones I
 * choose with every other group and channel on) and says so once, in place.
 */
export function MembersCanFindMeRow(props: {
  readonly kind: "group" | "channel";
  /** The scope id as raw bytes: a group's MLS group id, a channel's id. */
  readonly id: Uint8Array;
}): JSX.Element {
  const view = useView();
  const scopes = useFindableScopes();
  const [wasm] = createResource(loadWasm);
  const [excluded, setExcluded] = createSignal(false);

  const on = (): boolean => {
    visibilitySetting();
    return actions.allowsScope(wasm(), { kind: props.kind, id: props.id });
  };

  const shown = (): boolean => {
    const v = view();
    return v !== null && showsMembersSwitch(v);
  };

  function toggle(): void {
    const v = view();
    if (v === null) return;
    const entry = { kind: props.kind, id: props.id };
    if (v.groups === "all") {
      if (!on()) return;
      run(async () => {
        const next = await actions.excludeScopeUnderAll(scopes(), entry);
        if (next !== null) setExcluded(true);
      });
      return;
    }
    run(() => actions.setScopeChosen(entry, !on()));
  }

  return (
    <Show when={shown()}>
      <section class="lk-card">
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="search" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{COPY.membersCanFindMe()}</span>
            <Show when={excluded()}>
              <span class="lk-card-row-sub" role="status">
                {tRich("who_can_find_me.members_excluded_notice", {
                  privacy: <A href={paths.settingsSection("privacy")}>{t("settings.section.privacy")}</A>,
                })}
              </span>
            </Show>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={on()}
            aria-label={COPY.membersCanFindMe()}
            class="lk-card-row-action"
            classList={{ primary: on() }}
            onClick={toggle}
          >
            {on() ? t("common.on") : t("common.off")}
          </button>
        </div>
      </section>
    </Show>
  );
}
