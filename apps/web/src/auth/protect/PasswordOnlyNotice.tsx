// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Your chats depend on your password" (docs/ux/identity-recovery.md 19.1d):
 * the one notice, drawn in its three places and nowhere else: the "Protect
 * your account" step, the top of Settings > Security > Recovery, and once
 * after a sign-in on a device as the app's own dialog (`PasswordOnlyDialog`
 * in `AccountNotices.tsx`). Whether it applies is `password-only.ts`; this
 * file holds markup only.
 *
 * In the warning colour with the warning icon (not the danger colour:
 * nothing went wrong). Announced once when it appears.
 */
import { Show, type JSX } from "solid-js";

import { t } from "../../l10n";
import { Icon } from "../../ui/icons";

export function PasswordOnlyNotice(props: {
  /** `Save a recovery phrase`. */
  readonly onSavePhrase: () => void;
  /** `Add a passkey`; absent where this device cannot create passkeys. */
  readonly onAddPasskey?: (() => void) | undefined;
  /** A passkey that does not keep the chats was just added. */
  readonly passkeyNoSeal: boolean;
  /** Inside a dialog the title is the dialog's own. */
  readonly untitled?: boolean;
  /** The buttons are the dialog's own (place 3). */
  readonly withoutActions?: boolean;
}): JSX.Element {
  return (
    <div class="lk-warning-note" role="status">
      <span class="lk-warning-note-icon" aria-hidden="true">
        <Icon name="warning" size={20} />
      </span>
      <div class="lk-stack lk-warning-note-body">
        <Show when={props.untitled !== true}>
          <strong>{t("protect.password_only.title")}</strong>
        </Show>
        <span>{t("protect.password_only.body")}</span>
        <Show when={props.passkeyNoSeal}>
          <span>{t("protect.password_only.passkey_no_seal")}</span>
        </Show>
        <Show when={props.withoutActions !== true}>
          <div class="lk-actions">
            <button type="button" onClick={() => props.onSavePhrase()}>
              {t("protect.password_only.save_phrase")}
            </button>
            <Show when={props.onAddPasskey}>
              {(add) => (
                <button type="button" onClick={() => add()()}>
                  {t("protect.password_only.add_passkey")}
                </button>
              )}
            </Show>
          </div>
        </Show>
      </div>
    </div>
  );
}

/** The authenticator-only note of 19.1a and 19.1c, in the same style. */
export function WarningNote(props: { readonly text: string }): JSX.Element {
  return (
    <div class="lk-warning-note" role="status">
      <span class="lk-warning-note-icon" aria-hidden="true">
        <Icon name="warning" size={20} />
      </span>
      <div class="lk-warning-note-body">{props.text}</div>
    </div>
  );
}
