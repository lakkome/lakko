// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The auth screens' frame: the brand panel with the gradient on the left, the
 * form card on a light-indigo ground on the right (design system §1, "Auth
 * screen"). Login, Signup and Recover render their forms as children.
 */
import type { JSX } from "solid-js";
import { Icon } from "./icons";

export function AuthLayout(props: { children: JSX.Element }): JSX.Element {
  return (
    <div class="auth-shell">
      <section class="auth-brand" aria-hidden="true">
        <div class="auth-brand-logo">
          <img src="/icon-192.png" alt="" width="44" height="44" />
          <span>Lakko</span>
        </div>
        <div>
          <h2>Messages that stay between you.</h2>
          <p>
            Chats, groups, channels and calls, end-to-end encrypted by default.
            Lakko's servers never see your messages.
          </p>
        </div>
        <div class="auth-brand-foot">
          <Icon name="lock" size={16} />
          <span>Open source. No phone number required.</span>
        </div>
      </section>
      <section class="auth-side">
        <div class="auth-card">{props.children}</div>
      </section>
    </div>
  );
}
