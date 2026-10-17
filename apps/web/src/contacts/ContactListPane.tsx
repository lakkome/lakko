// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Contacts list pane (docs/ux/client-redesign.md section 5.3): the pane
 * head with search and the add menu, then Requests (incoming rows with
 * Accept and Decline; sent requests under a collapsed "Sent" group), then
 * the alphabetical contacts under letter headers. Names come from the one
 * profile resolver and never show a key fingerprint; the rules for grouping,
 * matching and ordering live in `contact-list-model.ts`.
 *
 * Markup only: the route owns the state and every mutation.
 */
import { createSignal, For, Show, type JSX } from "solid-js";

import { base64Encode } from "../auth/account-id";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { Icon, type IconName } from "../ui/icons";
import { t } from "../l10n";
import type { IncomingContactRequest } from "./client";
import {
  groupByLetter,
  matchesContactQuery,
  sortIncomingRequests,
  type SentRequestRecord,
} from "./contact-list-model";
import type { AcceptedContact } from "./local-store";
import { pendingNameOf, type PendingContact } from "./pending-contacts";

/** What the add menu offers. Web has no contact QR flow, so there is no
 *  "Show my QR code" / "Scan QR code" here. */
export type AddMode = "username" | "invite";

/**
 * Contacts is about people. Finding a channel is not here, and neither is
 * finding a group: both are kinds of conversation, so both belong to Chats
 * (`docs/ux/navigation.md` section 4).
 *
 * "Create call link" left too - it was put here only because Calls had been
 * removed. It did not move to Calls either: the form is gone entirely
 * (`docs/ux/call-links.md` sections 1 and 6), and a link is now offered from
 * inside a call that is already happening.
 */
const ADD_ITEMS: ReadonlyArray<{ mode: AddMode; labelKey: string; icon: IconName }> = [
  { mode: "username", labelKey: "contacts.add.by_username", icon: "user" },
  { mode: "invite", labelKey: "contacts.add.share_invite", icon: "link" },
];

export interface ContactListPaneProps {
  readonly filter: () => string;
  readonly onFilter: (value: string) => void;
  readonly onAdd: (mode: AddMode) => void;
  readonly pending: () => readonly IncomingContactRequest[];
  readonly pendingBusy: () => boolean;
  readonly error: () => string | null;
  readonly onAccept: (request: IncomingContactRequest) => void;
  readonly onDecline: (request: IncomingContactRequest) => void;
  readonly sent: () => readonly SentRequestRecord[];
  readonly onDismissSent: (record: SentRequestRecord) => void;
  /** ADR-0079 — inviters added through their link whose app has not
   *  answered yet. Listed among the contacts under their name, with no
   *  badge (UX section 4); a tap opens the chat with them. */
  readonly invited: () => readonly PendingContact[];
  readonly onOpenInvited: (contact: PendingContact) => void;
  readonly contacts: () => readonly AcceptedContact[];
  readonly selectedKey: () => string | null;
  readonly onSelect: (signaturePubkeyB64: string) => void;
}

export function ContactListPane(props: ContactListPaneProps): JSX.Element {
  const profiles = useProfiles();
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [sentOpen, setSentOpen] = createSignal(false);

  const nameOf = (c: AcceptedContact): string =>
    memberNameOf(profiles.resolve(c.signaturePubkeyB64));
  const usernameOf = (c: AcceptedContact): string | null =>
    profiles.resolve(c.signaturePubkeyB64).username;

  /** One row of the alphabetical list: a contact, or an inviter added
   *  through their link (ADR-0079) who is shown exactly like one. */
  type ListRow =
    | { readonly kind: "contact"; readonly c: AcceptedContact; readonly name: string; readonly username: string | null }
    | { readonly kind: "invited"; readonly p: PendingContact; readonly name: string; readonly username: string | null };
  const invitedName = (p: PendingContact): string =>
    pendingNameOf(p) ?? t("invite.contact.page.unnamed");
  const rows = (): ListRow[] => [
    ...props.contacts().map(
      (c): ListRow => ({ kind: "contact", c, name: nameOf(c), username: usernameOf(c) }),
    ),
    ...props.invited().map(
      (p): ListRow => ({
        kind: "invited",
        p,
        name: invitedName(p),
        username: p.username.length > 0 ? p.username : null,
      }),
    ),
  ];
  const matching = (): ListRow[] =>
    rows().filter((r) => matchesContactQuery(props.filter(), r.name, r.username));
  const groups = (): ReturnType<typeof groupByLetter<ListRow>> =>
    groupByLetter(matching(), (r) => r.name);
  const requests = (): IncomingContactRequest[] => sortIncomingRequests(props.pending());
  const nothingAtAll = (): boolean =>
    props.contacts().length === 0 &&
    props.invited().length === 0 &&
    props.pending().length === 0 &&
    props.sent().length === 0;

  function select(key: string): void {
    props.onSelect(key);
  }

  return (
    <aside class="lk-pane" aria-label={t("nav.contacts")}>
      <div class="lk-pane-head">
        <h1>{t("nav.contacts")}</h1>
        <div class="lk-menu-anchor">
          <button
            type="button"
            class="lk-icon-button accent"
            title={t("contacts.list.add")}
            aria-label={t("contacts.list.add")}
            aria-haspopup="menu"
            aria-expanded={menuOpen()}
            onClick={() => setMenuOpen(!menuOpen())}
          >
            <Icon name="plus" size={18} />
          </button>
          <Show when={menuOpen()}>
            <div class="lk-menu-backdrop" onClick={() => setMenuOpen(false)} />
            <div class="lk-menu" role="menu">
              <For each={ADD_ITEMS}>
                {(item) => (
                  <button
                    type="button"
                    role="menuitem"
                    onClick={() => {
                      setMenuOpen(false);
                      props.onAdd(item.mode);
                    }}
                  >
                    <Icon name={item.icon} size={18} />
                    {t(item.labelKey)}
                  </button>
                )}
              </For>
            </div>
          </Show>
        </div>
      </div>
      <div class="lk-search">
        <Icon name="search" size={18} />
        <input
          type="search"
          placeholder={t("chats.search.placeholder")}
          aria-label={t("contacts.list.search")}
          value={props.filter()}
          onInput={(e) => props.onFilter(e.currentTarget.value)}
        />
      </div>
      <div class="lk-pane-body">
        <Show when={props.error()}>
          {(message) => (
            <div class="form-error" style={{ margin: "0 12px 8px" }}>
              {message()}
            </div>
          )}
        </Show>

        {/* Requests arrive on their own (the mailbox long-poll); no refresh. */}
        <Show when={requests().length > 0 || props.sent().length > 0}>
          <div class="lk-section-label">{t("contacts.list.requests")}</div>
          <For each={requests()}>
            {(req) => {
              const r = (): ReturnType<typeof profiles.resolve> => profiles.resolve(req.fromPubkey);
              return (
                <div class="lk-row lk-request-row">
                  <Avatar label={memberNameOf(r())} id={base64Encode(req.fromPubkey)} />
                  <div class="lk-row-body">
                    <span class="lk-row-title">{memberNameOf(r())}</span>
                    <Show when={r().username}>{(u) => <span class="lk-row-sub">{u()}</span>}</Show>
                    {/* ADR-0079 section 3: sent through a link that no
                        longer works. A decides. */}
                    <Show when={req.deadLink === true}>
                      <span class="lk-row-sub">{t("contacts.request.dead_link_note")}</span>
                    </Show>
                    <span class="lk-request-intro">
                      {req.intro !== null && req.intro !== "" ? req.intro : t("contacts.request.no_intro")}
                    </span>
                    <div class="lk-actions" style={{ "margin-top": "6px" }}>
                      <button
                        class="primary small"
                        type="button"
                        disabled={props.pendingBusy()}
                        onClick={() => props.onAccept(req)}
                      >
                        {t("contacts.request.accept")}
                      </button>
                      <button class="small" type="button" onClick={() => props.onDecline(req)}>
                        {t("contacts.request.decline")}
                      </button>
                    </div>
                  </div>
                </div>
              );
            }}
          </For>
          <Show when={props.sent().length > 0}>
            <button
              type="button"
              class="lk-section-toggle"
              aria-expanded={sentOpen()}
              onClick={() => setSentOpen(!sentOpen())}
            >
              <Icon name="chevron" size={14} class={sentOpen() ? "open" : ""} />
              {t("contacts.list.sent", { count: props.sent().length })}
            </button>
            <Show when={sentOpen()}>
              <For each={props.sent()}>
                {(record) => (
                  <div class="lk-row">
                    <Avatar label={record.recipientUsername} id={record.recipientAccountIdHex} />
                    <div class="lk-row-body">
                      <span class="lk-row-title">@{record.recipientUsername}</span>
                      <span class="lk-row-sub">{t("contacts.list.sent_waiting")}</span>
                    </div>
                    <button
                      type="button"
                      class="ghost small"
                      title={t("contacts.list.sent_dismiss")}
                      onClick={() => props.onDismissSent(record)}
                    >
                      {t("common.dismiss")}
                    </button>
                  </div>
                )}
              </For>
            </Show>
          </Show>
        </Show>

        <Show
          when={!nothingAtAll()}
          fallback={
            <div class="lk-empty">
              <Icon name="contacts" size={40} strokeWidth={1.4} />
              <strong>{t("contacts.list.empty.title")}</strong>
              <span>{t("contacts.list.empty.body")}</span>
              <button type="button" class="primary" onClick={() => props.onAdd("username")}>
                {t("contacts.list.empty.action")}
              </button>
            </div>
          }
        >
          <Show
            when={groups().length > 0}
            fallback={
              <Show when={rows().length > 0}>
                <p class="lk-pane-nomatch">{t("contacts.list.no_match")}</p>
              </Show>
            }
          >
            <For each={groups()}>
              {(group) => (
                <>
                  <div class="lk-letter" aria-hidden="true">
                    {group.letter}
                  </div>
                  <For each={group.items}>
                    {(row) => {
                      const key = row.kind === "contact" ? row.c.signaturePubkeyB64 : row.p.signaturePubkeyB64;
                      const open = (): void => {
                        if (row.kind === "contact") select(row.c.signaturePubkeyB64);
                        else props.onOpenInvited(row.p);
                      };
                      return (
                        <div
                          class="lk-row"
                          role="button"
                          tabIndex={0}
                          classList={{ selected: props.selectedKey() === key }}
                          onClick={open}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" || e.key === " ") {
                              e.preventDefault();
                              open();
                            }
                          }}
                        >
                          <Avatar label={row.name} id={key} />
                          <div class="lk-row-body">
                            <span class="lk-row-title">{row.name}</span>
                            <Show when={row.username}>
                              {(u) => <span class="lk-row-sub">{u()}</span>}
                            </Show>
                          </div>
                        </div>
                      );
                    }}
                  </For>
                </>
              )}
            </For>
          </Show>
        </Show>
      </div>
    </aside>
  );
}
