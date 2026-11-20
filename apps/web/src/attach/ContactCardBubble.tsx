// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 10.2 — a shared contact. The card carries
 * a name, a username and an account key; it lets the receiver look the
 * person up, never write to them. The add flow and its outcome are the
 * conversation's; `message` is what it reports back.
 */
import { Match, Show, Switch, type JSX } from "solid-js";

import { t } from "../l10n";
import { Avatar } from "../ui/Avatar";
import type { ContactCardRelation, SharedContactCard } from "./types";
import "./bubbles.css";

export function ContactCardBubble(props: {
  readonly card: SharedContactCard;
  readonly relation: ContactCardRelation;
  readonly busy: boolean;
  readonly message?: string | undefined;
  readonly onAdd: () => void;
  readonly onOpenChat: () => void;
}): JSX.Element {
  return (
    <div class="lk-ak-card lk-contact-card">
      <div class="lk-contact-card-row">
        <Avatar label={props.card.name} id={props.card.accountKey} personKey={props.card.accountKey} size="md" />
        <div class="lk-contact-card-lines">
          <span class="lk-contact-card-name lk-ak-ellipsis">{props.card.name}</span>
          <span class="lk-ak-muted lk-ak-ellipsis">{`@${props.card.username}`}</span>
        </div>
      </div>
      <Switch>
        <Match when={props.relation === "self"}>
          <div class="lk-ak-muted lk-ak-small">{t("contact.this_is_you")}</div>
        </Match>
        <Match when={props.relation === "contact"}>
          <button type="button" class="lk-ak-secondary-button" onClick={() => props.onOpenChat()}>
            {t("contact.open_chat")}
          </button>
        </Match>
        <Match when={props.relation === "other"}>
          <button
            type="button"
            class="lk-ak-secondary-button"
            disabled={props.busy}
            aria-busy={props.busy}
            onClick={() => props.onAdd()}
          >
            {t("contact.add")}
          </button>
        </Match>
      </Switch>
      <Show when={(props.message ?? "").length > 0}>
        <div class="lk-ak-small lk-ak-muted" role="status">{props.message}</div>
      </Show>
    </div>
  );
}
