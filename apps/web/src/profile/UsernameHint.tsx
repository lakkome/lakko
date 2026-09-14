// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 UX §4 — the reason line under a username field, shown as the user
 * types. Renders nothing for a blank or valid name.
 */
import { Show, type JSX } from "solid-js";

import { usernameReason } from "./rules";
import { createUsernameCheck } from "./username-field";

export function UsernameHint(props: { readonly value: () => string }): JSX.Element {
  const check = createUsernameCheck(() => props.value());
  return (
    <Show when={usernameReason(check())}>
      {(reason) => (
        <div class="form-error" role="status" style={{ "font-size": "0.85rem" }}>
          {reason()}
        </div>
      )}
    </Show>
  );
}
