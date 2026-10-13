// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Change username" (ADR-0068): the screen, the wiring of the one resumable
 * operation (`username-change.ts`), and what a landing does with a rename
 * that was not finished.
 *
 * The screen warns what the change costs, asks for the proof of section
 * 17.1 when none is live (moving the login handle needs it), and takes the
 * new name with the usual live validation. When this device holds the
 * recovery address, the recovery email moves with the name (its vault sealed
 * again under the new handle, off the main thread); when it does not, the
 * screen says the rename turns the recovery email off.
 *
 * After the change the profile, the Everyone record and name terms, the
 * introduction offer and the channel member cards are published again by
 * their own publishers: their inputs (the name and its certificate) changed.
 *
 * The host is whole-session; the entry point opens it through
 * `username-claim-host.ts`.
 */
import { createEffect, createSignal, For, Match, onCleanup, onMount, Show, Switch, type JSX } from "solid-js";

import { useAuth } from "./AuthContext";
import { base64Encode } from "./account-id";
import { handleBindLimitedMessage } from "./auth-error-copy";
import { AuthClient, AuthHttpError } from "./client";
import { PROOF_COPY } from "./fresh-auth";
import { loginHandleFor } from "./login-handle";
import { heldAddress, rememberAddress } from "./protect/address-register";
import { runEmailVault } from "./protect/email-vault";
import { plainStore } from "./protect/local";
import { protectionStatus, refreshProtection } from "./protect/protection-state";
import { RESEAL_SALT_LEN } from "./protect/reseal";
import { createProofGate } from "./proof-gate";
import { ProofBlock } from "./ProofBlock";
import {
  readRenamedAt,
  readRenameOwed,
  renameAllowedAtMs,
  renameEmail,
  renameResume,
  renameSealsVault,
  renameWarningKeys,
  rollbackRename,
  runRename,
  writeRenamedAt,
  writeRenameOwed,
  type Answer,
  type RenameFailure,
  type RenameOwed,
  type RenamePorts,
} from "./username-change";
import { closeUsernameChange, usernameChangeOpen } from "./username-claim-host";
import { DiscoveryClient, DiscoveryHttpError } from "../discovery/client";
import { currentLanguage, t } from "../l10n";
import { useProfiles } from "../profile/ProfileContext";
import { mayLookUp, usernameReason } from "../profile/rules";
import { createUsernameCheck } from "../profile/username-field";
import { ModalLayer } from "../ui/ModalLayer";

function mediumDate(ms: number): string {
  return new Date(ms).toLocaleDateString(currentLanguage(), { dateStyle: "medium" });
}

function retryAfterOf(body: string): number | null {
  try {
    const secs = (JSON.parse(body) as { retry_after_secs?: unknown }).retry_after_secs;
    return typeof secs === "number" && Number.isFinite(secs) ? Math.max(0, Math.ceil(secs)) : null;
  } catch {
    return null;
  }
}

/** What the rename needs from the app, for one account. */
function useRenamePorts(): (account: { accountIdB64: string; accountId: Uint8Array; token: string }) => RenamePorts {
  const auth = useAuth();
  const profiles = useProfiles();
  return (account) => ({
    handleFor: (username) => loginHandleFor(username),
    sealVault: async (handle, address) => {
      const phrase = await auth.devicePhrase();
      if (phrase === null) return null;
      const salt = crypto.getRandomValues(new Uint8Array(RESEAL_SALT_LEN));
      const sealed = await runEmailVault({ kind: "seal", phrase, loginHandle: handle, address, vaultSalt: salt });
      return sealed.kind === "sealed" ? { salt, sealed: sealed.sealed } : null;
    },
    bind: async (args): Promise<Answer | null> => {
      try {
        await new AuthClient().bindLoginHandle(account.token, args.handle, {
          freshGrant: args.grant,
          previousHandle: args.previousHandle,
          ...(args.email === null
            ? {}
            : {
                recoveryEmail: {
                  address: args.email.address,
                  ...(args.email.vault === undefined
                    ? {}
                    : {
                        vaultSalt: base64Encode(args.email.vault.salt),
                        sealedEntropy: base64Encode(args.email.vault.sealed),
                      }),
                },
              }),
        });
        return null;
      } catch (err) {
        if (err instanceof AuthHttpError) {
          return { status: err.status, errorCode: err.errorCode, retryAfterSecs: err.retryAfter };
        }
        return { status: null, errorCode: null, retryAfterSecs: null };
      }
    },
    replace: async (username) => {
      try {
        const discovery = new DiscoveryClient();
        discovery.setSessionToken(account.token);
        const done = await discovery.replaceUsername(account.accountId, username);
        return { ok: true, username: done.normalisedUsername, certificate: done.usernameCertificate };
      } catch (err) {
        if (err instanceof DiscoveryHttpError) {
          return {
            ok: false,
            answer: { status: err.status, errorCode: err.errorCode, retryAfterSecs: retryAfterOf(err.body) },
          };
        }
        return { ok: false, answer: { status: null, errorCode: null, retryAfterSecs: null } };
      }
    },
    saveOwed: (record) => writeRenameOwed(plainStore(), account.accountIdB64, record),
    finishLocal: async (username, certificate) => {
      await auth.applyRenamedUsername(username);
      writeRenamedAt(plainStore(), account.accountIdB64, Date.now());
      try {
        await profiles.usernameClaimed(certificate);
      } catch {
        // The publishers retry by themselves.
      }
    },
    nowMs: () => Date.now(),
  });
}

/** The whole-session host: the screen, and the landing's resume. */
export function UsernameChangeHost(): JSX.Element {
  const auth = useAuth();
  const portsFor = useRenamePorts();
  const account = (): { accountIdB64: string; accountId: Uint8Array; token: string } | null => {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null || auth.signedOut() !== null) return null;
    return { accountIdB64: s.account.accountId, accountId: secrets.accountId, token: secrets.sessionToken };
  };

  // A rename that was not finished (a crash, a lost answer) is finished or
  // dropped at the landing; one whose rollback is owed waits for the person.
  let resumedFor: string | null = null;
  async function resume(acct: { accountIdB64: string; accountId: Uint8Array; token: string }): Promise<void> {
    const record = readRenameOwed(plainStore(), acct.accountIdB64);
    if (record === null) return;
    const ports = portsFor(acct);
    let state: "same" | "other" | "none" | null = null;
    if (record.phase === "binding") {
      try {
        state = await new AuthClient().checkLoginHandle(acct.token, (await loginHandleFor(record.newUsername)).wire);
      } catch {
        state = null;
      }
    }
    switch (renameResume(record, state)) {
      case "finish-local":
        await ports.finishLocal(record.newUsername, null);
        ports.saveOwed(null);
        return;
      case "drop":
        ports.saveOwed(null);
        return;
      case "replace-again": {
        const replaced = await ports.replace(record.newUsername);
        if (replaced.ok) {
          ports.saveOwed({ ...record, newUsername: replaced.username, phase: "replaced" });
          await ports.finishLocal(replaced.username, replaced.certificate);
          ports.saveOwed(null);
        } else if (replaced.answer.status !== null) {
          // Refused: the old handle is to be bound back, which needs the
          // person's proof.
          ports.saveOwed({ ...record, phase: "rollback" });
        }
        return;
      }
      case "wait-for-person":
      case "later":
        return;
    }
  }
  createEffect(() => {
    const acct = account();
    if (acct === null) {
      resumedFor = null;
      return;
    }
    if (resumedFor === acct.accountIdB64) return;
    resumedFor = acct.accountIdB64;
    void resume(acct).catch(() => undefined);
  });

  const shown = (): boolean => usernameChangeOpen() && account() !== null;
  return (
    <ModalLayer open={shown()} title={t("username.change.title")} onClose={closeUsernameChange}>
      <Show when={shown() ? account() : null}>
        {(acct) => <UsernameChangeScreen account={acct()} ports={portsFor(acct())} onClose={closeUsernameChange} />}
      </Show>
    </ModalLayer>
  );
}

function failureText(failure: RenameFailure): string | null {
  switch (failure.kind) {
    case "taken":
      return t("auth.signup.error.username_taken");
    case "too-soon":
      return t("username.change.too_soon", { date: mediumDate(failure.untilMs) });
    case "bind-limited":
      return handleBindLimitedMessage(failure.retryAfterSecs);
    case "offline":
      return t("common.error.offline");
    case "address-mismatch":
    case "error":
      return t("recovery_phrase.fresh.error.generic");
    case "proof-required":
      return null;
  }
}

function UsernameChangeScreen(props: {
  readonly account: { readonly accountIdB64: string; readonly accountId: Uint8Array; readonly token: string };
  readonly ports: RenamePorts;
  readonly onClose: () => void;
}): JSX.Element {
  const auth = useAuth();
  const [username, setUsername] = createSignal("");
  const usernameCheck = createUsernameCheck(username);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [done, setDone] = createSignal<string | null>(null);
  // An unfinished rollback of an earlier change (the handle is the new
  // name's, the name is the old one).
  const [owed, setOwed] = createSignal<RenameOwed | null>(readRenameOwed(plainStore(), props.account.accountIdB64));
  const rollbackOwed = (): RenameOwed | null => {
    const r = owed();
    return r !== null && r.phase === "rollback" ? r : null;
  };

  const current = (): string => {
    const s = auth.state();
    return s.kind === "session" ? s.account.username : "";
  };
  const status = () => protectionStatus(props.account.accountIdB64);
  const address = (): string | null => heldAddress(props.account.accountIdB64);
  const email = () => renameEmail(status()?.enabled === true, address() !== null);
  onMount(() => void refreshProtection(props.account.accountIdB64, props.account.token));

  const tooSoonAt = (): number | null =>
    renameAllowedAtMs(readRenamedAt(plainStore(), props.account.accountIdB64), Date.now());

  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.changeUsername,
    busy,
    sessionToken: () => props.account.token,
    usernameKnown: () => current().trim().length > 0,
  });
  onCleanup(() => gate.clear());

  /** The recovery email as the binds carry it, or `null`. */
  function emailInput(): { address: string; sealVault: boolean } | null {
    const held = address();
    if (email() !== "kept" || held === null) return null;
    return { address: held, sealVault: renameSealsVault("kept", status()?.phraseSaved === true) };
  }

  function showFailure(failure: RenameFailure): void {
    if (failure.kind === "proof-required") {
      // The grant was refused: the block comes up, with no error.
      const grant = auth.freshProof();
      if (grant !== null) auth.dropFreshProof(grant);
    }
    // The address this device held is not the account's: forgotten, so the
    // screen now says what the change does to the recovery email.
    if (failure.kind === "address-mismatch") rememberAddress(props.account.accountIdB64, null);
    setError(failureText(failure));
  }

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    if (busy()) return;
    setError(null);
    const checked = usernameCheck();
    if (!mayLookUp(checked)) {
      setError(usernameReason(checked) ?? t("auth.signup.error.enter_username"));
      return;
    }
    const old = current();
    if (checked.normalised === old) return;
    setBusy(true);
    try {
      // What the account's recovery email is NOW decides what the bind
      // carries.
      await refreshProtection(props.account.accountIdB64, props.account.token);
      const grant = await gate.ensure();
      if (grant === null) return;
      const outcome = await runRename(props.ports, {
        oldUsername: old,
        newUsername: checked.normalised,
        grant,
        email: emailInput(),
      });
      setOwed(readRenameOwed(plainStore(), props.account.accountIdB64));
      if (outcome.kind === "done") {
        void refreshProtection(props.account.accountIdB64, props.account.token);
        setDone(outcome.username);
        return;
      }
      showFailure(outcome.failure);
    } finally {
      setBusy(false);
    }
  }

  /** Bind the old handle back (an earlier change that was refused). */
  async function onRollback(): Promise<void> {
    const record = rollbackOwed();
    if (record === null || busy()) return;
    setError(null);
    setBusy(true);
    try {
      const grant = await gate.ensure();
      if (grant === null) return;
      const ok = await rollbackRename(props.ports, {
        oldUsername: record.oldUsername,
        newUsername: record.newUsername,
        grant,
        email: emailInput(),
      });
      if (ok) {
        props.ports.saveOwed(null);
        setOwed(null);
      } else {
        setError(t("recovery_phrase.fresh.error.generic"));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <Switch>
      <Match when={done()}>
        {(name) => (
          <div class="lk-stack">
            <p class="form-info" role="status">
              {t("username.claim.done", { username: `@${name()}` })}
            </p>
            <p class="lk-muted">{t("username.change.other_devices")}</p>
            <div class="lk-actions">
              <button type="button" class="primary" onClick={props.onClose}>
                {t("common.done")}
              </button>
            </div>
          </div>
        )}
      </Match>
      <Match when={rollbackOwed()}>
        <div class="lk-stack">
          <ProofBlock gate={gate} />
          <Show when={error() !== null}>
            <div class="form-error" role="alert">
              {error()}
            </div>
          </Show>
          <div class="lk-actions">
            <button
              type="button"
              class="primary"
              disabled={busy() || !gate.view().ready}
              onClick={() => void onRollback()}
            >
              {busy() ? t("common.saving") : t("common.try_again")}
            </button>
            <button type="button" class="ghost" disabled={busy()} onClick={props.onClose}>
              {t("common.cancel")}
            </button>
          </div>
        </div>
      </Match>
      <Match when={tooSoonAt()}>
        {(at) => (
          <div class="lk-stack">
            <p class="form-info" role="status">
              {t("username.change.too_soon", { date: mediumDate(at()) })}
            </p>
            <div class="lk-actions">
              <button type="button" onClick={props.onClose}>
                {t("common.close")}
              </button>
            </div>
          </div>
        )}
      </Match>
      <Match when={true}>
        <form onSubmit={(e) => void onSubmit(e)}>
          <For each={renameWarningKeys(email())}>
            {(key) => <p class="lk-muted">{t(key, { old: `@${current()}` })}</p>}
          </For>
          <ProofBlock gate={gate} />
          <label>
            {t("auth.username.label")}
            <input
              type="text"
              autocomplete="off"
              autocapitalize="none"
              spellcheck={false}
              required
              value={username()}
              onInput={(e) => setUsername(e.currentTarget.value)}
              disabled={busy()}
              aria-invalid={usernameCheck().kind === "invalid"}
            />
          </label>
          <div role="status" class="form-info">
            <Show
              when={usernameReason(usernameCheck())}
              fallback={
                <Show when={mayLookUp(usernameCheck()) ? usernameCheck() : null}>
                  {(c) => <span>@{(c() as { normalised: string }).normalised}</span>}
                </Show>
              }
            >
              {(reason) => <span class="form-error">{reason()}</span>}
            </Show>
          </div>
          <Show when={error() !== null}>
            <div class="form-error" role="alert">
              {error()}
            </div>
          </Show>
          <div class="lk-actions">
            <button
              class="primary"
              type="submit"
              disabled={busy() || !mayLookUp(usernameCheck()) || !gate.view().ready}
            >
              {busy() ? t("common.saving") : t("username.change.save")}
            </button>
            <button type="button" class="ghost" disabled={busy()} onClick={props.onClose}>
              {t("common.cancel")}
            </button>
          </div>
        </form>
      </Match>
    </Switch>
  );
}
