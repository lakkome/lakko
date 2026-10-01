// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The username field every recovery form starts with, with the ADR-0048
 *  reason line under it. */
import type { JSX } from "solid-js";
import { UsernameHint } from "../../profile/UsernameHint";
import { t } from "../../l10n";

export function UsernameField(props: {
  readonly value: () => string;
  readonly setValue: (v: string) => void;
  readonly disabled?: () => boolean;
}): JSX.Element {
  return (
    <>
      <label>
        {t("auth.username.label")}
        <input
          type="text"
          autocomplete="username"
          autocapitalize="none"
          spellcheck={false}
          required
          value={props.value()}
          onInput={(e) => props.setValue(e.currentTarget.value)}
          disabled={props.disabled?.() === true}
        />
      </label>
      <UsernameHint value={props.value} />
    </>
  );
}
