// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The six-digit code field of the recovery email's two flows (UX 19.2 and
 * 19.3), with the browser's one-time-code hint, and the "Send again" control
 * with its countdown.
 */
import { createSignal, onCleanup, Show, type JSX } from "solid-js";

import { formatCountdown } from "../recover/three-words";
import { t } from "../../l10n";
import { resendRemainingMs } from "./email-flow";

export function CodeField(props: {
  readonly label: string;
  readonly value: () => string;
  readonly setValue: (v: string) => void;
  readonly disabled: () => boolean;
  readonly inputRef?: (el: HTMLInputElement) => void;
}): JSX.Element {
  return (
    <label>
      {props.label}
      <input
        ref={(el) => props.inputRef?.(el)}
        type="text"
        inputmode="numeric"
        autocomplete="one-time-code"
        pattern="[0-9]{6}"
        maxlength="6"
        required
        value={props.value()}
        onInput={(e) => props.setValue(e.currentTarget.value)}
        disabled={props.disabled()}
        class="lk-code-input"
      />
    </label>
  );
}

/** "Send again in 0:45", then "Send again". */
export function ResendButton(props: {
  readonly resendAtMs: () => number;
  readonly disabled: () => boolean;
  readonly onResend: () => void;
}): JSX.Element {
  const [nowMs, setNowMs] = createSignal(Date.now());
  const timer = setInterval(() => setNowMs(Date.now()), 1_000);
  onCleanup(() => clearInterval(timer));
  const remaining = (): number => resendRemainingMs(props.resendAtMs(), nowMs());
  return (
    <Show
      when={remaining() > 0}
      fallback={
        <button type="button" class="ghost" disabled={props.disabled()} onClick={() => props.onResend()}>
          {t("recovery_email.code.resend")}
        </button>
      }
    >
      <span class="lk-muted">
        {t("recovery_email.code.resend_in", { time: formatCountdown(remaining()) })}
      </span>
    </Show>
  );
}
