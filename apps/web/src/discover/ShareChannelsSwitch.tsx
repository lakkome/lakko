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

export const SHARE_CHANNELS_LABEL = "Share my channels with this group";
export const SHARE_CHANNELS_LINE =
  "People in this group can see which channels you follow. Lakko's server cannot.";

export function ShareChannelsSwitch(props: { readonly entry: ActiveGroup }): JSX.Element {
  const shared = useSharedChannels();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const on = (): boolean => shared.isSharing(props.entry.groupIdB64);

  async function toggle(next: boolean): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      await shared.setSharing(props.entry, next);
    } catch (err) {
      setError(err instanceof Error && err.message.length > 0 ? err.message : "Couldn't change this. Try again.");
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
        <span>{SHARE_CHANNELS_LABEL}</span>
      </label>
      <p class="lk-card-desc">{SHARE_CHANNELS_LINE}</p>
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
