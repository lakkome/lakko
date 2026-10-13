// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recovery screen, "Get back in" (docs/ux/identity-recovery.md section
 * 2, ADR-0072 decision 7): a list of ways back in, each a tappable row, that
 * reveals the matching form under a heading that leads back to the list.
 *
 * First, above both groups (ADR-0075, UX 19.3): Email code, a code sent to
 * the recovery email. What it brings depends on the account, and nothing
 * says which before the code was accepted.
 *
 * Brings back the chats:
 *   - Another device (first, "if you have one"): a device that is still
 *     signed in confirms the request and transfers the keys and a session.
 *   - Passkey: signs in; restores the keys too when that was turned on for
 *     the passkey.
 *   - Recovery phrase: the 12 words plus a new password, through
 *     `AuthContext.recover`.
 *
 * Signs in only:
 *   - Three words from the phrase, checked on this device against the
 *     phrase it holds (maintainer decision 2026-10-02). Listed only when
 *     this device holds the phrase and the keys of its cached account, a
 *     local fact; absent everywhere else, and then the note under the list
 *     does not mention it.
 *   - The authenticator code. On a device that holds the keys the session
 *     lands; otherwise the guard shows the restore prompt, so that form does
 *     not navigate.
 *
 * Nothing is hidden or badged from server state. After a sign-in on a
 * device that holds the keys, the guard shows the "Choose a new password"
 * step (section 7.1) before the app.
 *
 * The forms live in `auth/recover/`; this file owns the list and the shared
 * username.
 */
import { createResource, createSignal, For, Show, Switch, Match, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { AuthLayout } from "../ui/AuthLayout";
import { t } from "../l10n";
import { tRich } from "../l10n/rich";
import { MethodHead, MethodList } from "../ui/MethodList";
import { useAuth } from "../auth/AuthContext";
import {
  accessOnlyNoteKey,
  recoverMethod,
  recoverMethodGroups,
  type RecoverMethodId,
} from "../auth/recover/methods";
import { WordsRecover } from "../auth/recover/WordsRecover";
import { PasskeyRecover } from "../auth/recover/PasskeyRecover";
import { PhraseRecover } from "../auth/recover/PhraseRecover";
import { DeviceRecover } from "../auth/recover/DeviceRecover";
import { TotpRecover } from "../auth/recover/TotpRecover";
import { EmailRecover } from "../auth/recover/EmailRecover";

export default function Recover(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();

  const [method, setMethod] = createSignal<RecoverMethodId | null>(null);
  // Pre-filled from the device cache when present; the forms resolve any
  // registered username through the directory.
  const cachedUsername = (): string | null => {
    const s = auth.state();
    return s.kind === "anon" && s.cachedUsername !== undefined ? s.cachedUsername : null;
  };
  const [username, setUsername] = createSignal(cachedUsername() ?? "");

  const done = (): void => navigate(paths.home, { replace: true });

  // Whether this device can check three words itself: a local fact (the
  // cached account, its phrase and its keys under the device key). While it
  // is being read the row is absent; it appears when the answer is yes.
  const [wordsAccount, { refetch: refetchWordsAccount }] = createResource(() => auth.threeWordsAccount());
  const device = (): { threeWords: boolean } => ({
    threeWords: (wordsAccount() ?? null) !== null,
  });
  const backToList = (): void => {
    setMethod(null);
  };
  // Three words were suspended on this device: the row is re-read (and is
  // gone) when the list comes back.
  const afterWordsSuspended = (): void => {
    setMethod(null);
    void refetchWordsAccount();
  };

  return (
    <AuthLayout>
      <Show
        when={method()}
        fallback={
          <>
            <h1>{t("recovery.title")}</h1>
            <p class="lk-auth-lede">{t("recovery.lede")}</p>
            <For each={recoverMethodGroups(device())}>
              {(group) => (
                <>
                  <Show when={group.label.length > 0}>
                    <h2 class="lk-method-group">{group.label}</h2>
                  </Show>
                  <MethodList methods={() => group.methods} onPick={setMethod} />
                </>
              )}
            </For>
            <p class="form-info">{t(accessOnlyNoteKey(device()))}</p>
            <p class="lk-auth-foot">
              {tRich("recovery.remembered", {
                link: <A href={paths.login}>{t("auth.login.submit")}</A>,
              })}
            </p>
          </>
        }
      >
        {(picked) => (
          <>
            <MethodHead method={() => recoverMethod(picked())} onBack={backToList} />
            <Switch>
              <Match when={picked() === "email"}>
                <EmailRecover
                  username={username}
                  setUsername={setUsername}
                  onBack={backToList}
                  onDone={done}
                />
              </Match>
              <Match when={picked() === "phrase"}>
                <PhraseRecover username={username} setUsername={setUsername} onDone={done} />
              </Match>
              <Match when={picked() === "passkey"}>
                <PasskeyRecover username={username} onBack={backToList} />
              </Match>
              <Match when={picked() === "words" ? wordsAccount() : null}>
                {(account) => 
                  <WordsRecover
                    account={account()}
                    onBack={backToList}
                    onSuspended={afterWordsSuspended}
                  />
                }
              </Match>
              <Match when={picked() === "device"}>
                <DeviceRecover username={username} setUsername={setUsername} onDone={done} />
              </Match>
              <Match when={picked() === "totp"}>
                <TotpRecover username={username} setUsername={setUsername} onBack={backToList} />
              </Match>
            </Switch>
            <p class="lk-auth-foot">
              {tRich("recovery.remembered", {
                link: <A href={paths.login}>{t("auth.login.submit")}</A>,
              })}
            </p>
          </>
        )}
      </Show>
    </AuthLayout>
  );
}
