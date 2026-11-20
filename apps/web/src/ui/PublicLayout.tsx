// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The frame for public landings reached from a link (`/i/`, `/g/`, `/c/`):
 * the logo, then one card on the light-indigo ground. These screens render
 * outside the app shell and may be seen by someone without an account.
 */
import type { JSX } from "solid-js";

export function PublicLayout(props: { children: JSX.Element }): JSX.Element {
  return (
    <div class="public-shell">
      <div class="public-brand" aria-hidden="true">
        <img src="/icon-192.png" alt="" width="40" height="40" />
        <span>Lakko</span>
      </div>
      <div class="auth-card public-card">{props.children}</div>
    </div>
  );
}

/**
 * The frame for a public page that reads like a page of the app rather than
 * one card (`/donations`): the app's page ground, the logo, then the content
 * at the app's wide page width (about 1040 px), left-aligned.
 */
export function PublicWideLayout(props: { children: JSX.Element }): JSX.Element {
  return (
    <div class="public-wide">
      <div class="public-wide-page">
        <div class="public-brand" aria-hidden="true">
          <img src="/icon-192.png" alt="" width="40" height="40" />
          <span>Lakko</span>
        </div>
        {props.children}
      </div>
    </div>
  );
}
