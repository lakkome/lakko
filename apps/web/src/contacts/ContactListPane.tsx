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
import type { IncomingContactRequest } from "./client";
import {
  groupByLetter,
  matchesContactQuery,
  sortIncomingRequests,
  type SentRequestRecord,
} from "./contact-list-model";
import type { AcceptedContact } from "./local-store";

/** What the add menu offers. Web has no contact QR flow, so there is no
 *  "Show my QR code" / "Scan QR code" here. */
export type AddMode = "username" | "invite" | "call";

const ADD_ITEMS: ReadonlyArray<{ mode: AddMode; label: string; icon: IconName }> = [
  { mode: "username", label: "Add by username", icon: "user" },
  { mode: "invite", label: "Share invite link", icon: "link" },
  { mode: "call", label: "Create call link", icon: "phone" },
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

  const matching = (): AcceptedContact[] =>
    props.contacts().filter((c) => matchesContactQuery(props.filter(), nameOf(c), usernameOf(c)));
  const groups = (): ReturnType<typeof groupByLetter<AcceptedContact>> =>
    groupByLetter(matching(), nameOf);
  const requests = (): IncomingContactRequest[] => sortIncomingRequests(props.pending());
  const nothingAtAll = (): boolean =>
    props.contacts().length === 0 && props.pending().length === 0 && props.sent().length === 0;

  function select(key: string): void {
    props.onSelect(key);
  }

  return (
    <aside class="lk-pane" aria-label="Contacts">
      <div class="lk-pane-head">
        <h1>Contacts</h1>
        <div class="lk-menu-anchor">
          <button
            type="button"
            class="lk-icon-button accent"
            title="Add"
            aria-label="Add"
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
                    {item.label}
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
          placeholder="Search"
          aria-label="Search contacts"
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
          <div class="lk-section-label">Requests</div>
          <For each={requests()}>
            {(req) => {
              const r = (): ReturnType<typeof profiles.resolve> => profiles.resolve(req.fromPubkey);
              return (
                <div class="lk-row lk-request-row">
                  <Avatar label={memberNameOf(r())} id={base64Encode(req.fromPubkey)} />
                  <div class="lk-row-body">
                    <span class="lk-row-title">{memberNameOf(r())}</span>
                    <Show when={r().username}>{(u) => <span class="lk-row-sub">{u()}</span>}</Show>
                    <span class="lk-request-intro">
                      {req.intro !== null && req.intro !== "" ? req.intro : "Wants to connect"}
                    </span>
                    <div class="lk-actions" style={{ "margin-top": "6px" }}>
                      <button
                        class="primary small"
                        type="button"
                        disabled={props.pendingBusy()}
                        onClick={() => props.onAccept(req)}
                      >
                        Accept
                      </button>
                      <button class="small" type="button" onClick={() => props.onDecline(req)}>
                        Decline
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
              Sent ({props.sent().length})
            </button>
            <Show when={sentOpen()}>
              <For each={props.sent()}>
                {(record) => (
                  <div class="lk-row">
                    <Avatar label={record.recipientUsername} id={record.recipientAccountIdHex} />
                    <div class="lk-row-body">
                      <span class="lk-row-title">@{record.recipientUsername}</span>
                      <span class="lk-row-sub">Waiting</span>
                    </div>
                    <button
                      type="button"
                      class="ghost small"
                      title="Remove from this list"
                      onClick={() => props.onDismissSent(record)}
                    >
                      Dismiss
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
              <strong>No contacts yet</strong>
              <span>Add someone by username, or share your invite link.</span>
              <button type="button" class="primary" onClick={() => props.onAdd("username")}>
                Add contact
              </button>
            </div>
          }
        >
          <Show
            when={groups().length > 0}
            fallback={
              <Show when={props.contacts().length > 0}>
                <p class="lk-pane-nomatch">No contacts match.</p>
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
                    {(c) => (
                      <div
                        class="lk-row"
                        role="button"
                        tabIndex={0}
                        classList={{ selected: props.selectedKey() === c.signaturePubkeyB64 }}
                        onClick={() => select(c.signaturePubkeyB64)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            select(c.signaturePubkeyB64);
                          }
                        }}
                      >
                        <Avatar label={nameOf(c)} id={c.signaturePubkeyB64} />
                        <div class="lk-row-body">
                          <span class="lk-row-title">{nameOf(c)}</span>
                          <Show when={usernameOf(c)}>
                            {(u) => <span class="lk-row-sub">{u()}</span>}
                          </Show>
                        </div>
                      </div>
                    )}
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
