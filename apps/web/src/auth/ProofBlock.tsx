// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The shared proof block (docs/ux/identity-recovery.md section 17.1): the
 * sign-in screen's password field, the per-surface helper, and `Use a
 * passkey instead`, drawn INSIDE the surface that needs the proof (the reset
 * panel, the delete dialog, "Is this you?", the scan confirm, add a
 * passkey, change password). Absent when a proof is live. It decides
 * nothing: every rule is `proofBlockView` in `fresh-auth.ts`, driven by the
 * surface's `ProofGate`.
 */
import { createEffect, createUniqueId, Show, type JSX } from "solid-js";

import { PasswordField } from "../ui/PasswordField";
import { t } from "../l10n";
import type { ProofGate } from "./proof-gate";
import { UsernameField } from "./recover/UsernameField";
import { WaitNotice } from "./recover/FormNotices";

export function ProofBlock(props: {
  readonly gate: ProofGate;
  /** Extra content under the field (the surface's `I do not know my
   *  password` link). */
  readonly children?: JSX.Element;
}): JSX.Element {
  const gate = props.gate;
  const helpId = createUniqueId();
  let input: HTMLInputElement | undefined;

  // Wrong password: the field keeps focus and its contents are selected.
  createEffect(() => {
    const line = gate.line();
    if (line !== null && line.error && gate.lastFailure()?.kind === "wrong-password") {
      queueMicrotask(() => {
        input?.focus();
        input?.select();
      });
    }
  });

  return (
    <Show when={gate.view().visible}>
      <div class="lk-stack lk-proof-block">
        <Show when={gate.view().showPasskeyNotice}>
          <p class="lk-muted">
            {gate.phase() === "passkey" ? t("recovery.passkey.waiting") : t("proof.passkey.notice")}
          </p>
        </Show>
        <Show when={gate.view().showPassword}>
          <Show when={gate.view().showUsername}>
            <div>
              <UsernameField
                value={gate.username}
                setValue={(v) => gate.setUsername(v)}
                disabled={() => gate.view().fieldDisabled}
              />
            </div>
          </Show>
          <div dir="auto">
            <PasswordField
              label={t("auth.password.label")}
              autocomplete="current-password"
              required={false}
              value={gate.password}
              setValue={(v) => gate.setPassword(v)}
              disabled={() => gate.view().fieldDisabled}
              inputRef={(el) => {
                input = el;
              }}
              describedBy={helpId}
            />
            <Show when={gate.copy.helpKey !== null}>
              <p class="lk-muted" id={helpId}>
                {t(gate.copy.helpKey ?? "")}
              </p>
            </Show>
          </div>
          <Show when={gate.phase() === "passkey"}>
            <p class="lk-muted">{t("recovery.passkey.waiting")}</p>
          </Show>
        </Show>
        <Show when={gate.line()}>
          {(line) => (
            <Show
              when={line().error}
              fallback={<p class="form-info" role="status">{line().text()}</p>}
            >
              <div
                class="form-error"
                role="alert"
                aria-label={t("common.error.a11y", { message: line().text() })}
              >
                {line().text()}
              </div>
            </Show>
          )}
        </Show>
        <WaitNotice remainingMs={gate.wait.remainingMs} />
        <Show when={gate.view().showUsePasskey}>
          <div>
            <button
              type="button"
              class="ghost"
              disabled={gate.view().passkeyButtonDisabled}
              onClick={() => void gate.usePasskey()}
            >
              {t("proof.passkey.use")}
            </button>
          </div>
        </Show>
        <Show when={gate.view().showUsePassword}>
          <div>
            <button
              type="button"
              class="ghost"
              disabled={gate.view().passkeyButtonDisabled}
              onClick={() => gate.usePassword()}
            >
              {t("proof.password.use")}
            </button>
          </div>
        </Show>
        {props.children}
      </div>
    </Show>
  );
}
