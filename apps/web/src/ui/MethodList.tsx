// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A list of ways to do something, each a tappable row with an icon, a title
 * and a one-line subtitle (docs/ux/client-redesign.md section 5.6: the
 * recovery methods). Picking a row hands its id back; the caller reveals the
 * matching form under a `MethodHead` that leads back to the list.
 */
import { For, type JSX } from "solid-js";
import { Icon, type IconName } from "./icons";
import { t } from "../l10n";

export interface MethodSpec<Id extends string> {
  readonly id: Id;
  readonly icon: IconName;
  readonly title: string;
  readonly sub: string;
}

export function MethodList<Id extends string>(props: {
  readonly methods: () => readonly MethodSpec<Id>[];
  readonly onPick: (id: Id) => void;
  readonly disabled?: () => boolean;
}): JSX.Element {
  return (
    <ul class="lk-method-list">
      <For each={props.methods()}>
        {(m) => (
          <li>
            <button
              type="button"
              class="lk-method-row"
              disabled={props.disabled?.() === true}
              onClick={() => props.onPick(m.id)}
            >
              <span class="lk-card-row-icon">
                <Icon name={m.icon} size={18} />
              </span>
              <span class="lk-method-row-body">
                <span class="lk-method-row-title">{m.title}</span>
                <span class="lk-method-row-sub">{m.sub}</span>
              </span>
              <Icon name="chevron" size={18} />
            </button>
          </li>
        )}
      </For>
    </ul>
  );
}

/** The heading over a revealed method: a back control and the method's
 *  icon and title. */
export function MethodHead(props: {
  readonly method: () => MethodSpec<string>;
  readonly onBack: () => void;
  readonly backLabel?: string;
  readonly disabled?: () => boolean;
}): JSX.Element {
  return (
    <div class="lk-method-head">
      <button
        type="button"
        class="lk-icon-button"
        aria-label={props.backLabel ?? t("recovery.methods.back")}
        disabled={props.disabled?.() === true}
        onClick={props.onBack}
      >
        <Icon name="back" size={20} />
      </button>
      <span class="lk-card-row-icon">
        <Icon name={props.method().icon} size={18} />
      </span>
      <h2>{props.method().title}</h2>
    </div>
  );
}
