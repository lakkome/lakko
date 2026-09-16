// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "How Lakko protects your password": the one small link on the login and
 * signup cards that opens a plain-language explanation inline
 * (docs/ux/client-redesign.md section 5.6). Nothing about the protocol; only
 * what it means for the user.
 */
import type { JSX } from "solid-js";
import { Icon } from "../ui/icons";

export function PasswordExplainer(): JSX.Element {
  return (
    <details class="lk-disclosure link">
      <summary>
        <Icon name="lock" size={14} />
        How Lakko protects your password
      </summary>
      <div class="lk-disclosure-body">
        <p>
          Your password never leaves your device. Lakko checks it without ever
          receiving it, so nobody at Lakko can read it, and nobody can reset it
          for you.
        </p>
        <p>
          A wrong username and a wrong password get the same answer, so nobody can
          use the login screen to find out which usernames exist.
        </p>
        <p>
          If you forget it, your recovery phrase, another device that is still
          logged in, three words from the phrase, an authenticator app, or a code
          sent by email or phone get you back in.
        </p>
      </div>
    </details>
  );
}
