// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recovery screen (docs/ux/client-redesign.md section 5.6): a list of
 * ways back in, each a tappable row, that reveals the matching form under a
 * heading that leads back to the list.
 *
 *   - Recovery phrase / Social recovery shares: the phrase (typed, or
 *     rebuilt from shares) plus a new password, through `AuthContext.recover`.
 *   - Another device: a device that is still logged in confirms the request
 *     and transfers the identity and access (ADR-0018 factor (a)).
 *   - Three words: the ADR-0049 shortcut, then the same `recover`.
 *   - Authenticator / Email or phone: access only (ADR-0018 factors (c) and
 *     (d)); the guard then shows the restore prompt when this device has no
 *     identity, so those two forms do not navigate.
 *
 * The forms live in `auth/recover/`; this file owns the list and the shared
 * username.
 */
import { createSignal, Show, Switch, Match, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { AuthLayout } from "../ui/AuthLayout";
import { MethodHead, MethodList } from "../ui/MethodList";
import { useAuth } from "../auth/AuthContext";
import { RECOVER_METHODS, recoverMethod, type RecoverMethodId } from "../auth/recover/methods";
import { PhraseRecover } from "../auth/recover/PhraseRecover";
import { WordsRecover } from "../auth/recover/WordsRecover";
import { DeviceRecover } from "../auth/recover/DeviceRecover";
import { TotpRecover } from "../auth/recover/TotpRecover";
import { ContactRecover } from "../auth/recover/ContactRecover";

export default function Recover(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();

  const [method, setMethod] = createSignal<RecoverMethodId | null>(null);
  // Set when the three-word shortcut turns out to be off: the row goes, and
  // the reason is shown over the phrase form the user is moved to.
  const [wordsOff, setWordsOff] = createSignal<string | null>(null);
  const methods = (): typeof RECOVER_METHODS =>
    RECOVER_METHODS.filter((m) => m.id !== "words" || wordsOff() === null);

  // Pre-filled from the device cache when present; the forms resolve any
  // registered username through the directory.
  const cachedUsername = (): string | null => {
    const s = auth.state();
    return s.kind === "anon" && s.cachedUsername !== undefined ? s.cachedUsername : null;
  };
  const [username, setUsername] = createSignal(cachedUsername() ?? "");

  const done = (): void => navigate(paths.home, { replace: true });

  return (
    <AuthLayout>
      <Show
        when={method()}
        fallback={
          <>
            <h1>Recover your account</h1>
            <p class="lk-auth-lede">Choose how to get back in.</p>
            <MethodList methods={methods} onPick={setMethod} />
            <p class="form-info">
              An authenticator or a code gets you back in; your chats then need your
              recovery phrase or another device.
            </p>
            <p class="lk-auth-foot">
              Remembered your password? <A href={paths.login}>Log in</A>
            </p>
          </>
        }
      >
        {(picked) => (
          <>
            <MethodHead method={() => recoverMethod(picked())} onBack={() => setMethod(null)} />
            <Show when={picked() === "phrase" && wordsOff() !== null}>
              <div class="lk-alert info" role="status">
                {wordsOff()}
              </div>
            </Show>
            <Switch>
              <Match when={picked() === "phrase"}>
                <PhraseRecover mode="phrase" username={username} setUsername={setUsername} onDone={done} />
              </Match>
              <Match when={picked() === "shares"}>
                <PhraseRecover mode="shares" username={username} setUsername={setUsername} onDone={done} />
              </Match>
              <Match when={picked() === "words"}>
                <WordsRecover
                  username={username}
                  setUsername={setUsername}
                  onDone={done}
                  onUnavailable={(notice) => {
                    setWordsOff(notice);
                    setMethod("phrase");
                  }}
                />
              </Match>
              <Match when={picked() === "device"}>
                <DeviceRecover username={username} setUsername={setUsername} onDone={done} />
              </Match>
              <Match when={picked() === "totp"}>
                <TotpRecover username={username} setUsername={setUsername} />
              </Match>
              <Match when={picked() === "contact"}>
                <ContactRecover username={username} setUsername={setUsername} />
              </Match>
            </Switch>
            <p class="lk-auth-foot">
              Remembered your password? <A href={paths.login}>Log in</A>
            </p>
          </>
        )}
      </Show>
    </AuthLayout>
  );
}
