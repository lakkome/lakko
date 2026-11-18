// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 10.1 — pick one of the person's contacts
 * to share. A contact with no username cannot be shared (the receiver adds
 * through a username lookup, ADR-0092 section 7) and is shown disabled; so
 * is a contact whose "who can find me" is `Nobody` (their newest profile
 * says their card may not be shared, maintainer 2026-11-18).
 */
import { createMemo, createSignal, For, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { initialsOf } from "../ui/Avatar";
import { AttachScreen } from "./AttachScreen";
import { filterContacts } from "./contact-filter";
import { cardPickState, type CardPickState } from "../profile/card-share";
import type { ShareableContact, SharedContactCard } from "./types";
import "./editors.css";

export function ContactPicker(props: {
  readonly contacts: readonly ShareableContact[];
  readonly onCancel: () => void;
  readonly onSend: (card: SharedContactCard) => Promise<void> | void;
}): JSX.Element {
  const [query, setQuery] = createSignal("");
  const [chosen, setChosen] = createSignal<ShareableContact | null>(null);
  const [busy, setBusy] = createSignal(false);
  const shown = createMemo(() => filterContacts(props.contacts, query()));
  const stateOf = (c: ShareableContact): CardPickState =>
    cardPickState({ username: c.username ?? null, cardShareable: c.cardShareable !== false });

  async function send(): Promise<void> {
    const c = chosen();
    if (c === null || c.username === undefined || stateOf(c) !== "enabled" || busy()) return;
    setBusy(true);
    try {
      await props.onSend({ name: c.name, username: c.username, accountKey: c.accountKeyB64 });
    } finally {
      setBusy(false);
    }
  }

  return (
    <AttachScreen
      title={t("contact.picker.title")}
      onClose={() => props.onCancel()}
      footer={
        <button type="button" class="primary" disabled={chosen() === null || busy()} onClick={() => void send()}>
          {t("contact.send")}
        </button>
      }
    >
      <Show when={chosen()}>
        {(c) => (
          <div class="lk-attach-card" aria-live="polite">
            <span class="lk-attach-initials" aria-hidden="true">
              {initialsOf(c().name)}
            </span>
            <span class="lk-attach-contact-text">
              <span>{c().name}</span>
              <span class="lk-attach-note">@{c().username}</span>
            </span>
          </div>
        )}
      </Show>
      <input
        class="lk-attach-field"
        type="search"
        placeholder={t("contact.picker.search")}
        aria-label={t("contact.picker.search")}
        value={query()}
        onInput={(e) => setQuery(e.currentTarget.value)}
      />
      <Show when={props.contacts.length > 0} fallback={<p class="lk-attach-note">{t("contact.picker.empty")}</p>}>
        <div class="lk-attach-contacts" role="list">
          <For each={shown()}>
            {(c) => (
              <div role="listitem">
                <button
                  type="button"
                  class="lk-attach-contact"
                  aria-pressed={chosen()?.accountKeyB64 === c.accountKeyB64}
                  disabled={stateOf(c) !== "enabled"}
                  onClick={() => setChosen(c)}
                >
                  <span class="lk-attach-initials" aria-hidden="true">
                    {initialsOf(c.name)}
                  </span>
                  <span class="lk-attach-contact-text">
                    <span>{c.name}</span>
                    <span class="lk-attach-note">
                      {stateOf(c) === "no-username"
                        ? t("contact.no_username")
                        : stateOf(c) === "not-shareable"
                          ? t("contact.not_shareable", { name: c.name })
                          : `@${c.username ?? ""}`}
                    </span>
                  </span>
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>
    </AttachScreen>
  );
}
