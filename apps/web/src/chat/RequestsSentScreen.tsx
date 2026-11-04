// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/group-and-channel-privacy.md` section 6.2 — the **Requests sent**
 * list: avatar, name, `@name`, one line. Under 7 days "Waiting for an admin."
 * with no control; after that "No answer yet." with `Ask again`. A request
 * cannot be withdrawn, and is never said to be refused.
 */
import { createSignal, For, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Decode } from "../auth/account-id";
import { ContactsClient } from "../contacts/client";
import { t } from "../l10n";
import { useProfiles } from "../profile/ProfileContext";
import { rememberCardAvatar } from "../profile/avatar-runtime";
import { requestErrorText } from "../search/ConversationCard";
import { Avatar } from "../ui/Avatar";
import { PushedScreen } from "../ui/PushedScreen";
import { useChats } from "./ChatContext";
import { durableSnapshotHook, sessionAccountOf } from "./persist-hooks";
import { recordRequestSent, requestsSent, sentRequestState, type SentRequest } from "./requests-sent";

export function RequestsSentScreen(props: { readonly open: boolean; readonly onBack: () => void }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const [busy, setBusy] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
  });

  async function askAgain(r: SentRequest): Promise<void> {
    setBusy(r.idB64);
    setError(null);
    try {
      await client.requestToJoinSealed(
        auth.identitySecrets(),
        base64Decode(r.idB64),
        base64Decode(r.knockKeyB64),
        r.knockVersion,
      );
      recordRequestSent({ ...r, sentAtMs: Date.now() });
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
            const state = (): "waiting" | "no-answer" => sentRequestState(r, Date.now());
            return (
              <div class="lk-row">
                <Avatar label={r.name} id={r.idB64} personKey={`conv:${r.idB64}`} />
                <div class="lk-row-body">
                  <span class="lk-row-title">{r.name}</span>
                  <span class="lk-row-sub">
                    <span dir="ltr">{`@${r.handle}`}</span>
                    {" · "}
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
