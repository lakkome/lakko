// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `Their contacts can find me` on a contact's screen
 * (`docs/ux/who-can-find-me.md` section 5.1). Whether it is on the screen is the
 * shared info rule; under Only the ones I choose it is that contact's switch
 * from the list, and under All it shows on, and turning it off excludes this
 * one contact (the part becomes Only the ones I choose with every other contact
 * on) and says so once, in place.
 */
import { createResource, createSignal, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";

import { t } from "../l10n";
import { tRich } from "../l10n/rich";
import { visibilitySetting } from "../reach/visibility-store";
import { paths } from "../routes/paths";
import { Icon } from "../ui/icons";
import { loadWasm } from "../wasm";
import { useFindableContacts } from "./findable-contacts";
import * as actions from "./visibility-actions";
import { COPY, viewOf } from "./who-can-find-me";

export function TheirContactsCanFindMeRow(props: {
  /** The contact's account id, the name the setting has for them. */
  readonly accountIdHex: string;
}): JSX.Element {
  const contacts = useFindableContacts();
  const [wasm] = createResource(loadWasm);
  const [excluded, setExcluded] = createSignal(false);

  const on = (): boolean => {
    visibilitySetting();
    return actions.allowsContact(wasm(), props.accountIdHex);
  };

  function run(action: () => Promise<unknown>): void {
    void action().catch((err: unknown) => {
      // A refused transition (too many chosen) leaves the setting as it was.
      // eslint-disable-next-line no-console
      console.warn("[privacy] visibility change refused", err);
    });
  }

  function toggle(): void {
    const s = visibilitySetting();
    if (s === null) return;
    if (viewOf(s).contactsForm === "all") {
      if (!on()) return;
      run(async () => {
        const all = contacts().map((c) => c.accountIdHex);
        const next = await actions.excludeContactUnderAll(all, props.accountIdHex);
        if (next !== null) setExcluded(true);
      });
      return;
    }
    run(() => actions.setContactChosen(props.accountIdHex, !on()));
  }

  return (
    <section class="lk-card">
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="search" size={20} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">{COPY.theirContactsCanFindMe()}</span>
          <Show when={excluded()}>
            <span class="lk-card-row-sub" role="status">
              {tRich("who_can_find_me.contacts_excluded_notice", {
                privacy: <A href={paths.settingsSection("privacy")}>{t("settings.section.privacy")}</A>,
              })}
            </span>
          </Show>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={on()}
          aria-label={COPY.theirContactsCanFindMe()}
          class="lk-card-row-action"
          classList={{ primary: on() }}
          onClick={toggle}
        >
          {on() ? t("common.on") : t("common.off")}
        </button>
      </div>
    </section>
  );
}
