// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `Invite contacts` on the call screen (`docs/ux/call-links.md` section 3):
 * the person's contacts, a search field, and one press per contact.
 *
 * Contacts, not conversations: whom to invite is a question about people.
 * Where the invitation goes (the one-to-one with that contact, founded first
 * when there is none) is the caller's business, behind `onInvite`.
 *
 * The layer stays open so several contacts can be invited one after another;
 * an invited row shows a check and `Invited` and cannot be pressed twice.
 * Which rows there are and how a query filters them is `invite-contacts.ts`,
 * where a test can run it.
 */
import { createEffect, createMemo, createSignal, For, Show, type JSX } from "solid-js";

import { loadAccepted, type AcceptedContact } from "../contacts/local-store";
import { t } from "../l10n";
import { keyedErrorText } from "../l10n/keyed-error";
import { createLiveText } from "../l10n/live-text";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { ModalLayer } from "../ui/ModalLayer";
import {
  filterInviteContactRows,
  inviteContactRows,
  type InviteContactRow,
} from "./invite-contacts";

export function InviteContactsPicker(props: {
  readonly open: boolean;
  readonly onClose: () => void;
  /** Send the invitation to this contact. Resolves once it is published; a
   *  rejection keeps the layer open with the error shown. */
  readonly onInvite: (row: InviteContactRow) => Promise<void>;
}): JSX.Element {
  const profiles = useProfiles();
  const [contacts, setContacts] = createSignal<AcceptedContact[]>([]);
  const [query, setQuery] = createSignal("");
  const [busyKey, setBusyKey] = createSignal<string | null>(null);
  const [doneKeys, setDoneKeys] = createSignal<ReadonlySet<string>>(new Set<string>());
  const [error, setError] = createLiveText();

  // Every opening starts clean and reads the contact list as it is now.
  createEffect(() => {
    if (!props.open) return;
    setQuery("");
    setBusyKey(null);
    setDoneKeys(new Set<string>());
    setError(null);
    setContacts(loadAccepted());
  });

  const rows = createMemo((): InviteContactRow[] =>
    inviteContactRows(
      contacts(),
      (c) => memberNameOf(profiles.resolve(c.signaturePubkeyB64)),
      (c) => profiles.resolve(c.signaturePubkeyB64).username,
    ),
  );
  const visible = (): InviteContactRow[] => filterInviteContactRows(rows(), query());

  async function invite(row: InviteContactRow): Promise<void> {
    if (busyKey() !== null || doneKeys().has(row.keyB64)) return;
    setError(null);
    setBusyKey(row.keyB64);
    try {
      await props.onInvite(row);
      setDoneKeys(new Set([...doneKeys(), row.keyB64]));
    } catch (err) {
      // ADR-0061 section 6: the error's code, never its prose.
      console.warn("[call invite] failed", err);
      setError(() => keyedErrorText(err) ?? t("call.invite.send_failed"));
    } finally {
      setBusyKey(null);
    }
  }

  return (
    <ModalLayer open={props.open} title={t("call.invite.picker_title")} onClose={props.onClose}>
      <Show when={rows().length > 0}>
        <label class="lk-search lk-forward-search">
          <Icon name="search" size={16} />
          <input
            type="search"
            placeholder={t("chats.search.placeholder")}
            aria-label={t("contacts.list.search")}
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
          />
        </label>
      </Show>
      <Show when={error()}>{(message) => <div class="form-error">{message()}</div>}</Show>
      <Show
        when={rows().length > 0}
        fallback={<p class="lk-pane-nomatch">{t("call.invite.picker_empty")}</p>}
      >
        <Show when={visible().length === 0}>
          <p class="lk-pane-nomatch">{t("chats.search.no_results")}</p>
        </Show>
        <ul class="lk-forward-list" aria-label={t("call.invite.picker_title")}>
          <For each={visible()}>
            {(row) => {
              const busy = (): boolean => busyKey() === row.keyB64;
              const done = (): boolean => doneKeys().has(row.keyB64);
              const sub = (): string =>
                busy()
                  ? t("common.sending")
                  : done()
                    ? t("call.invite.row_invited")
                    : row.username === null
                      ? ""
                      : `@${row.username.replace(/^@/, "")}`;
              return (
                <li>
                  <button
                    type="button"
                    class="lk-row lk-forward-row"
                    aria-busy={busy()}
                    disabled={busyKey() !== null || done()}
                    onClick={() => void invite(row)}
                  >
                    <Avatar label={row.name} id={row.keyB64} />
                    <div class="lk-row-body">
                      <div class="lk-row-line">
                        <span class="lk-row-title">
                          <span class="lk-ellipsis">{row.name}</span>
                        </span>
                      </div>
                      <div class="lk-row-line">
                        <span class="lk-row-sub">{sub()}</span>
                        <Show when={done()}>
                          <Icon name="check" size={16} />
                        </Show>
                      </div>
                    </div>
                  </button>
                </li>
              );
            }}
          </For>
        </ul>
      </Show>
    </ModalLayer>
  );
}
