// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Two of the states every recovery form shares
 * (docs/ux/identity-recovery.md section 2):
 *
 *   - rate limited: one sentence with a live countdown;
 *   - unavailable: the method is switched off on this server, with a button
 *     back to the list.
 */
import { Show, type JSX } from "solid-js";
import { t } from "../../l10n";
import { formatCountdown } from "./three-words";

/**
 * "Too many attempts. Try again in 14:59." The sentence a screen reader
 * hears is announced once, when the wait starts (the time it names is the
 * time at that moment); the ticking copy is hidden from it, so the wait is
 * not re-announced every second.
 */
export function WaitNotice(props: {
  /** Milliseconds left; nothing renders at 0. */
  readonly remainingMs: () => number;
  /** The sentence's catalog key; it takes `{time}`. */
  readonly messageKey?: string;
}): JSX.Element {
  const key = (): string => props.messageKey ?? "recovery.error.rate_limited";
  return (
    <Show when={props.remainingMs() > 0}>
      <div class="form-error">
        <span aria-hidden="true">
          {t(key(), { time: formatCountdown(props.remainingMs()) })}
        </span>
        <AnnouncedOnce text={() => t(key(), { time: formatCountdown(props.remainingMs()) })} />
      </div>
    </Show>
  );
}

/** The text as it was when this mounted, for assistive technology only. */
function AnnouncedOnce(props: { readonly text: () => string }): JSX.Element {
  // Read once, untracked by design: components run once in Solid.
  const initial = props.text();
  return (
    <span class="lakko-visually-hidden" role="alert">
      {initial}
    </span>
  );
}

/** "This method is not available right now. Choose another one." */
export function MethodUnavailable(props: { readonly onBack: () => void }): JSX.Element {
  return (
    <div class="lk-stack">
      <p class="form-error" role="alert">
        {t("recovery.error.unavailable")}
      </p>
      <button type="button" onClick={props.onBack}>
        {t("recovery.methods.back_to_list")}
      </button>
    </div>
  );
}
