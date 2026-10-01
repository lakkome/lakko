// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings -> Security -> Channels I share (docs/ux/feed.md 6.3): every group
 * the "Share my channels" switch is on in, so it can be turned off in one
 * place. Turning it off sends the group an empty list (ADR-0059 section 6).
 */

import { createSignal, For, Show, type JSX } from "solid-js";

import { useChats } from "../chat/ChatContext";
import { useGroupNames } from "../chat/GroupNameContext";
import { useSharedChannels } from "./SharedChannelsContext";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";

export function ChannelsIShareCard(): JSX.Element {
  const chats = useChats();
  const groupNames = useGroupNames();
  const shared = useSharedChannels();
  const [error, setError] = createLiveText();
  const [busy, setBusy] = createSignal<string | null>(null);

  const groups = (): string[] => [...shared.row().shareOn].filter((g) => chats.get(g) !== undefined);
  const title = (g: string): string => groupNames.nameOf(g) ?? chats.get(g)?.label ?? t("chat.group.fallback_name");

  async function stop(g: string): Promise<void> {
    const entry = chats.get(g);
    if (entry === undefined || busy() !== null) return;
    setBusy(g);
    setError(null);
    try {
      await shared.setSharing(entry, false);
    } catch {
      setError(() => t("discover.shared.stop_failed"));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section class="lk-card">
      <h2>{t("discover.shared.title")}</h2>
      <p class="lk-card-desc">
        {t("discover.shared.desc")}
      </p>
      <Show when={groups().length > 0} fallback={<p class="lk-card-desc">{t("discover.shared.empty")}</p>}>
        <For each={groups()}>
          {(g) => (
            <div class="lk-card-row">
              <div class="lk-card-row-body">
                <span class="lk-card-row-title">{title(g)}</span>
              </div>
              <button type="button" class="ghost" disabled={busy() !== null} onClick={() => void stop(g)}>
                {t("discover.shared.stop")}
              </button>
            </div>
          )}
        </For>
      </Show>
      <Show when={error()}>
        {(msg) => (
          <div class="form-error" role="alert">
            {msg()}
          </div>
        )}
      </Show>
    </section>
  );
}
