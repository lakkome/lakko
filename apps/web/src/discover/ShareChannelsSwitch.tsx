// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Share my channels with this group" in group info (docs/ux/feed.md 6.3,
 * ADR-0059 section 6). Off by default. Turning it on sends this person's
 * listed channels to the group, end-to-end encrypted; off sends an empty
 * list. The switch changes only once the send succeeded.
 */

import { createSignal, Show, type JSX } from "solid-js";

import type { ActiveGroup } from "../chat/ChatContext";
import { useSharedChannels } from "./SharedChannelsContext";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { keyedErrorText } from "../l10n/keyed-error";

export const SHARE_CHANNELS_LABEL = (): string => t("discover.share.label");
export const SHARE_CHANNELS_LINE = (): string => t("discover.share.line");

export function ShareChannelsSwitch(props: { readonly entry: ActiveGroup }): JSX.Element {
  const shared = useSharedChannels();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();
  const on = (): boolean => shared.isSharing(props.entry.groupIdB64);

  async function toggle(next: boolean): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      await shared.setSharing(props.entry, next);
    } catch (err) {
      setError(() => keyedErrorText(err) ?? t("discover.share.error"));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="lk-card lk-share-channels">
      <label class="lk-row">
        <input
          type="checkbox"
          role="switch"
          checked={on()}
          disabled={busy()}
          onChange={(e): void => {
            const next = e.currentTarget.checked;
            e.currentTarget.checked = on();
            void toggle(next);
          }}
        />
        <span>{SHARE_CHANNELS_LABEL()}</span>
      </label>
      <p class="lk-card-desc">{SHARE_CHANNELS_LINE()}</p>
      <Show when={error()}>
        {(msg) => (
          <div class="form-error" role="alert">
            {msg()}
          </div>
        )}
      </Show>
    </div>
  );
}
