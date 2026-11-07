// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `Banned` list in a channel's `Subscribers` screen, for the owner and
 * publishers (UX `group-and-channel-privacy.md` 13.3, 15.1): the comment
 * authors whose comments are hidden, each with `Unban`. A ban list names
 * per-channel author keys, not people, so a row shows the name the person's
 * comments carried when this device read one. A banned person is told
 * nothing.
 */
import { createSignal, For, Show, type JSX } from "solid-js";

import { base64Encode } from "../auth/account-id";
import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { Avatar } from "../ui/Avatar";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { channelBanList, commentAuthorName, unbanFromComments } from "./ban-list";
import { bindChannelFeedback } from "./ChannelFeedbackRuntime";
import { bytesOfHex } from "./feedback-api";

export function BannedList(props: { channelIdHex: string }): JSX.Element {
  const chats = useChats();
  bindChannelFeedback(useAuth(), chats);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [error, setError] = createLiveText();
  const authors = (): readonly string[] => channelBanList(props.channelIdHex)?.authors ?? [];

  async function unban(authorHex: string): Promise<void> {
    const id = chats.conversationAccountIdentity();
    if (id === null || busy() !== null) return;
    setBusy(authorHex);
    setError(null);
    try {
      await unbanFromComments(id, props.channelIdHex, authorHex);
    } catch {
      setError(() => t("channels.ban.failed"));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section class="lk-card" aria-label={t("channels.ban.title")}>
      <h2 class="lk-card-title">{t("channels.ban.title")}</h2>
      <Show
        when={authors().length > 0}
        fallback={<p class="lk-meta">{t("channels.ban.empty")}</p>}
      >
        <For each={authors()}>
          {(authorHex) => {
            const name = (): string => commentAuthorName(authorHex) ?? t("channels.ban.unknown_name");
            return (
              <div class="lk-card-row">
                <Avatar label={name()} id={authorHex} size="sm" personKey={base64Encode(bytesOfHex(authorHex))} />
                <div class="lk-card-row-body">
                  <bdi class="lk-card-row-title">{name()}</bdi>
                </div>
                <button
                  type="button"
                  class="secondary small lk-card-row-action"
                  disabled={busy() !== null}
                  onClick={() => void unban(authorHex)}
                >
                  {busy() === authorHex ? t("common.saving") : t("channels.ban.unban")}
                </button>
              </div>
            );
          }}
        </For>
      </Show>
      <Show when={error()}>
        {(msg) => (
          <p class="form-error" role="alert">
            {msg()}
          </p>
        )}
      </Show>
    </section>
  );
}
