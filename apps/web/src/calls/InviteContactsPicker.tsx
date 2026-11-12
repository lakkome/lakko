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
import type { InviteOutcome } from "./invite-route";
import {
  filterInviteContactRows,
  inviteContactRows,
  type InviteContactRow,
} from "./invite-contacts";

export function InviteContactsPicker(props: {
  readonly open: boolean;
  readonly onClose: () => void;
  /** Send the invitation to this contact. Resolves with what the row says
   *  afterwards (`invite-route.ts`); a rejection keeps the layer open with
   *  the error shown. */
  readonly onInvite: (row: InviteContactRow) => Promise<InviteOutcome>;
}): JSX.Element {
  const profiles = useProfiles();
  const [contacts, setContacts] = createSignal<AcceptedContact[]>([]);
  const [query, setQuery] = createSignal("");
  const [busyKey, setBusyKey] = createSignal<string | null>(null);
  const [outcomes, setOutcomes] = createSignal<ReadonlyMap<string, InviteOutcome>>(new Map());
  const [error, setError] = createLiveText();

  // Every opening starts clean and reads the contact list as it is now.
  createEffect(() => {
    if (!props.open) return;
    setQuery("");
    setBusyKey(null);
    setOutcomes(new Map());
    setError(null);
    setContacts(loadAccepted());
  });

  const rows = createMemo((): InviteContactRow[] =>
    inviteContactRows(
      contacts(),
      (c) => memberNameOf(profiles.resolve(c.signaturePubkeyB64), "the call invite picker"),
      (c) => profiles.resolve(c.signaturePubkeyB64).username,
    ),
  );
  const visible = (): InviteContactRow[] => filterInviteContactRows(rows(), query());

  async function invite(row: InviteContactRow): Promise<void> {
    if (busyKey() !== null || rowIsDone(outcomes().get(row.keyB64))) return;
    setError(null);
    setBusyKey(row.keyB64);
    try {
      const outcome = await props.onInvite(row);
      setOutcomes(new Map([...outcomes(), [row.keyB64, outcome]]));
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
          <Icon name="search" size={20} />
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
              const outcome = (): InviteOutcome | undefined => outcomes().get(row.keyB64);
              const done = (): boolean => rowIsDone(outcome());
              const sub = (): string =>
                busy()
                  ? t("common.sending")
                  : (inviteRowLine(outcome(), row.name) ??
                    (row.username === null ? "" : `@${row.username.replace(/^@/, "")}`));
              return (
                <li>
                  <button
                    type="button"
                    class="lk-row lk-forward-row"
                    aria-busy={busy()}
                    disabled={busyKey() !== null || done()}
                    onClick={() => void invite(row)}
                  >
                    <Avatar label={row.name} id={row.keyB64} personKey={row.keyB64} />
                    <div class="lk-row-body">
                      <div class="lk-row-line">
                        <span class="lk-row-title">
                          <span class="lk-ellipsis">{row.name}</span>
                        </span>
                      </div>
                      <div class="lk-row-line">
                        <span class="lk-row-sub">{sub()}</span>
                        <Show when={outcome() === "invited"}>
                          <Icon name="checkmark" size={20} />
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

/** Whether a row cannot be pressed again: invited, or a request already went
 *  to them. A row that could not be messaged stays pressable. */
export function rowIsDone(outcome: InviteOutcome | undefined): boolean {
  return outcome === "invited" || outcome === "request-sent";
}

/** The row's second line after a press (who-can-find-me 6.4), or `null` for
 *  the username. */
export function inviteRowLine(outcome: InviteOutcome | undefined, name: string): string | null {
  switch (outcome) {
    case "invited":
      return t("call.invite.row_invited");
    case "request-sent":
      return t("search.people.request_sent");
    case "cannot-message":
      return t("contacts.detail.cannot_message", { name });
    case undefined:
      return null;
  }
}
