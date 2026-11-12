// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/group-and-channel-privacy.md` section 6.2 — the **Requests sent**
 * list: avatar, name, `@name`, one line. Under a day "Waiting for an admin."
 * with no control; after that "No answer yet." with `Ask again` (the rule and
 * the resend are `./ask-again.ts`, shared with the `/g/` page). A request
 * cannot be withdrawn, and is never said to be refused.
 */
import { createSignal, For, onCleanup, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { ContactsClient } from "../contacts/client";
import { t } from "../l10n";
import { useProfiles } from "../profile/ProfileContext";
import { rememberCardAvatar } from "../profile/avatar-runtime";
import { requestErrorText } from "../search/ConversationCard";
import { Avatar } from "../ui/Avatar";
import { PushedScreen } from "../ui/PushedScreen";
import { useChats } from "./ChatContext";
import { durableSnapshotHook, sessionAccountOf } from "./persist-hooks";
import { sendRequestAgain } from "./ask-again";
import { recordRequestSent, requestsSent, sentRequestState, type SentRequest } from "./requests-sent";

export function RequestsSentScreen(props: { readonly open: boolean; readonly onBack: () => void }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const [busy, setBusy] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  // The day is crossed while the list is on screen too.
  const [now, setNow] = createSignal(Date.now());
  const clock = setInterval(() => setNow(Date.now()), 60_000);
  onCleanup(() => clearInterval(clock));
  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
  });

  async function askAgain(r: SentRequest): Promise<void> {
    setBusy(r.idB64);
    setError(null);
    try {
      await sendRequestAgain(client, auth.identitySecrets(), r, Date.now(), auth.identitySecrets()?.accountId);
      const sentAtMs = Date.now();
      recordRequestSent({ ...r, sentAtMs });
      setNow(sentAtMs);
    } catch (err) {
      setError(requestErrorText(err));
    } finally {
      setBusy(null);
    }
  }

  return (
    <PushedScreen open={props.open} title={t("requests_sent.title")} onBack={props.onBack}>
      <div class="lk-stack" style={{ padding: "8px 0" }}>
        <Show when={error()}>
          {(m) => (
            <p class="form-error" role="alert">
              {m()}
            </p>
          )}
        </Show>
        <For each={requestsSent()}>
          {(r) => {
            rememberCardAvatar(`conv:${r.idB64}`, r.avatar);
            const state = (): "waiting" | "no-answer" => sentRequestState(r, now());
            const name = (): string => (r.name.length > 0 ? r.name : t("chat.group.fallback_name"));
            return (
              <div class="lk-row">
                <Avatar label={name()} id={r.idB64} personKey={`conv:${r.idB64}`} />
                <div class="lk-row-body">
                  <span class="lk-row-title">{name()}</span>
                  <span class="lk-row-sub">
                    {/* A request from a link knows no `@name`. */}
                    <Show when={r.handle.length > 0}>
                      <span dir="ltr">{`@${r.handle}`}</span>
                      {" · "}
                    </Show>
                    {state() === "waiting" ? t("requests_sent.waiting") : t("requests_sent.no_answer")}
                  </span>
                </div>
                <Show when={state() === "no-answer"}>
                  <button
                    type="button"
                    class="secondary small"
                    disabled={busy() !== null}
                    onClick={() => void askAgain(r)}
                  >
                    {t("requests_sent.ask_again")}
                  </button>
                </Show>
              </div>
            );
          }}
        </For>
      </div>
    </PushedScreen>
  );
}
